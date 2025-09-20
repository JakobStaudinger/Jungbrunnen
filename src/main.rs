#![no_std]
#![no_main]

mod stream;

use defmt::*;
use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_rp::{
    self, Peripheral, bind_interrupts,
    clocks::clk_sys_freq,
    dma::{AnyChannel, Channel, Word},
    pac::{self, SIO, dma::regs::CtrlTrig},
    peripherals::PIO1,
    pio::{InterruptHandler, Pio, ShiftConfig},
    pwm::{self, Pwm, Slice},
};
use embassy_time::Duration;
use fixed::{FixedU32, types::extra::U8};
use heapless::Vec;
use pio_proc::pio_asm;

use crate::stream::{Color, Hz, StreamConfig};

use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    PIO1_IRQ_0 => InterruptHandler<PIO1>;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let locked_state = SIO.spinlock_st();
    info!("Spinlock state {:b}", locked_state.read());

    SIO.spinlock(31).write_value(1);

    let p = embassy_rp::init(Default::default());

    let mut pio = Pio::new(p.PIO1, Irqs);

    let red_pin = p.PIN_2;
    let green_pin = p.PIN_4;
    let blue_pin = p.PIN_6;

    let timing_program = pio_asm! {
        r#"
            .define public MICROS_PER_TICK 64
            .define public TICK_OVERHEAD 5
            wait 0 irq 0

        .wrap_target
            out y 8
            in null 8
            in y 8
            out x 24
        delay:
            jmp x-- delay
        .wrap
        "#
    };

    let mut timing_config = embassy_rp::pio::Config::default();
    timing_config.use_program(&pio.common.load_program(&timing_program.program), &[]);
    let target_frequency = 1_000_000 / timing_program.public_defines.MICROS_PER_TICK;
    let clock_divider = (clk_sys_freq() as f64) / (target_frequency as f64);
    timing_config.clock_divider = FixedU32::<U8>::checked_from_num(clock_divider).unwrap();
    timing_config.shift_out = ShiftConfig {
        direction: embassy_rp::pio::ShiftDirection::Left,
        auto_fill: true,
        threshold: 32,
    };
    timing_config.shift_in = ShiftConfig {
        direction: embassy_rp::pio::ShiftDirection::Left,
        auto_fill: true,
        threshold: 16,
    };

    let mut pwm_config = pwm::Config::default();
    pwm_config.enable = true;
    pwm_config.top = 254;

    let pwm_slice_red = p.PWM_SLICE1.number();
    let pwm = Pwm::new_output_a(p.PWM_SLICE1, red_pin, pwm_config.clone());
    core::mem::forget(pwm);

    sync_pio_to_pwm(
        [p.DMA_CH0.degrade(), p.DMA_CH1.degrade()],
        pwm_slice_red,
        1,
        0,
    );

    let pwm_slice_green = p.PWM_SLICE2.number();
    let pwm = Pwm::new_output_a(p.PWM_SLICE2, green_pin, pwm_config.clone());
    core::mem::forget(pwm);

    sync_pio_to_pwm(
        [p.DMA_CH2.degrade(), p.DMA_CH3.degrade()],
        pwm_slice_green,
        1,
        1,
    );

    let pwm_slice_blue = p.PWM_SLICE3.number();
    let pwm = Pwm::new_output_a(p.PWM_SLICE3, blue_pin, pwm_config.clone());
    core::mem::forget(pwm);

    sync_pio_to_pwm(
        [p.DMA_CH4.degrade(), p.DMA_CH5.degrade()],
        pwm_slice_blue,
        1,
        2,
    );

    pio.sm0.set_config(&timing_config);
    pio.sm0.set_enable(true);

    pio.sm1.set_config(&timing_config);
    pio.sm1.set_enable(true);

    pio.sm2.set_config(&timing_config);
    pio.sm2.set_enable(true);

    let mut dmas = (
        p.DMA_CH7.into_ref(),
        p.DMA_CH8.into_ref(),
        p.DMA_CH9.into_ref(),
    );
    let mut red_data: Vec<u32, 256> = Vec::new();
    let mut green_data: Vec<u32, 256> = Vec::new();
    let mut blue_data: Vec<u32, 256> = Vec::new();

    pio.irq_flags.set_all(0);

    let mut config = stream::Config::new(
        &[
            StreamConfig::new(Color(255, 0, 0), Hz(60.), Duration::from_millis(4), None),
            StreamConfig::new(
                Color(0, 255, 255),
                Hz(60.5),
                Duration::from_millis(4),
                Some(Duration::from_millis(500)),
            ),
            StreamConfig::new(
                Color(0, 255, 0),
                Hz(59.5),
                Duration::from_millis(4),
                Some(Duration::from_millis(2500)),
            ),
        ],
        timing_program.public_defines.MICROS_PER_TICK,
        timing_program.public_defines.TICK_OVERHEAD,
    )
    .into_iter();

    loop {
        info!("Loop");

        red_data.clear();
        green_data.clear();
        blue_data.clear();

        while !red_data.is_full() {
            let next = config.next().unwrap();

            red_data.push(next.encode_red()).unwrap();
            green_data.push(next.encode_green()).unwrap();
            blue_data.push(next.encode_blue()).unwrap();
        }

        join3(
            pio.sm0.tx().dma_push(dmas.0.reborrow(), &red_data),
            pio.sm1.tx().dma_push(dmas.1.reborrow(), &green_data),
            pio.sm2.tx().dma_push(dmas.2.reborrow(), &blue_data),
        )
        .await;
    }
}

fn sync_pio_to_pwm(dmas: [AnyChannel; 2], pwm_slice: usize, pio_number: u8, sm: u8) {
    let raw_pwm = pac::PWM.ch(pwm_slice);

    let treq_sel = pac::dma::vals::TreqSel::from(pio_number * 8 + sm + 4);

    let [first_dma, second_dma] = dmas;
    let r = first_dma.regs();

    r.write_addr().write_value(raw_pwm.cc().as_ptr() as u32);
    r.read_addr()
        .write_value(pac::PIO1.rxf(sm as usize).as_ptr() as u32);
    r.trans_count().write_value(u32::MAX);
    r.al1_ctrl().write(|val| {
        let mut w = CtrlTrig(0);
        w.set_treq_sel(treq_sel);
        w.set_data_size(u16::size());
        w.set_chain_to(second_dma.number());
        w.set_incr_read(false);
        w.set_incr_write(false);
        w.set_en(true);

        *val = w.0;
    });

    let r = second_dma.regs();

    r.write_addr().write_value(raw_pwm.cc().as_ptr() as u32);
    r.read_addr()
        .write_value(pac::PIO1.rxf(sm as usize).as_ptr() as u32);
    r.trans_count().write_value(u32::MAX);
    r.ctrl_trig().write(|w| {
        w.set_treq_sel(treq_sel);
        w.set_data_size(u16::size());
        w.set_chain_to(first_dma.number());
        w.set_incr_read(false);
        w.set_incr_write(false);
        w.set_en(true);
    });
}
