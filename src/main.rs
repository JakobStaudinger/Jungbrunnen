#![no_std]
#![no_main]

use core::cmp::Ordering;

use defmt::*;
use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_rp::{
    self, Peripheral, bind_interrupts,
    clocks::clk_sys_freq,
    dma::{AnyChannel, Channel, Word},
    pac::{self, SIO, dma::regs::CtrlTrig},
    peripherals::PIO1,
    pio::{Config, InterruptHandler, Pio, ShiftConfig},
    pwm::{self, Pwm, Slice},
};
use embassy_time::{Duration, Instant, Timer};
use fixed::{FixedU32, types::extra::U8};
use heapless::Vec;
use itertools::{Either, Itertools, Merge};
use pio_proc::pio_asm;

use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    PIO1_IRQ_0 => InterruptHandler<PIO1>;
});

#[derive(Clone, Copy, PartialEq, Eq)]
struct Color(u8, u8, u8);

impl Color {
    pub fn r(&self) -> u8 {
        self.0
    }

    pub fn g(&self) -> u8 {
        self.1
    }

    pub fn b(&self) -> u8 {
        self.2
    }
}

#[derive(Clone, Copy)]
struct Hz(pub f32);

#[derive(Clone, Copy)]
struct Percent(pub f32);

#[derive(Clone, Copy)]
struct StreamConfig {
    color: Color,
    frequency: Hz,
    burst_duration: Duration,
    offset: Duration,
}

#[derive(Clone, Copy)]
struct Edge {
    time: Instant,
    direction: EdgeDirection,
}

impl PartialOrd for Edge {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Edge {
    fn cmp(&self, other: &Self) -> Ordering {
        self.time.cmp(&other.time)
    }
}

impl PartialEq for Edge {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time && self.direction == other.direction
    }
}

impl Eq for Edge {}

impl Edge {
    pub fn encode_red(&self, last_edge: &Option<Edge>) -> u16 {
        self.encode(last_edge, |color| color.r())
    }

    pub fn encode_green(&self, last_edge: &Option<Edge>) -> u16 {
        self.encode(last_edge, |color| color.g())
    }

    pub fn encode_blue(&self, last_edge: &Option<Edge>) -> u16 {
        self.encode(last_edge, |color| color.b())
    }

    fn encode(
        &self,
        last_edge: &Option<Edge>,
        get_color_component: impl FnOnce(Color) -> u8,
    ) -> u16 {
        let delay = self.time - last_edge.map(|edge| edge.time).unwrap_or(Instant::MIN);
        let delay = (delay.as_millis() / 10) as u16 & 0xFF;

        match self.direction {
            EdgeDirection::Falling => 0x0000_u16 | delay,
            EdgeDirection::Rising(color) => (get_color_component(color) as u16) << 8 | delay,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EdgeDirection {
    Rising(Color),
    Falling,
}

impl StreamConfig {
    pub fn new(
        color: Color,
        frequency: Hz,
        burst_duration: Duration,
        offset: Option<Duration>,
    ) -> Self {
        core::assert!(burst_duration.as_micros() <= (1e9_f32 / frequency.0) as u64);

        Self {
            color,
            frequency,
            burst_duration,
            offset: offset.unwrap_or_default(),
        }
    }

    pub fn into_iter(self) -> StreamConfigIterator {
        StreamConfigIterator {
            config: self,
            last_edge: None,
        }
    }
}

struct StreamConfigIterator {
    config: StreamConfig,
    last_edge: Option<Edge>,
}

impl Iterator for StreamConfigIterator {
    type Item = Edge;

    fn next(&mut self) -> Option<Self::Item> {
        let period = Duration::from_micros((1_000_000. / self.config.frequency.0) as u64);
        let result = match self.last_edge {
            None => Edge {
                direction: EdgeDirection::Rising(self.config.color),
                time: Instant::MIN + self.config.offset,
            },
            Some(Edge {
                direction: EdgeDirection::Rising(_),
                time,
            }) => Edge {
                direction: EdgeDirection::Falling,
                time: time + self.config.burst_duration,
            },
            Some(Edge {
                direction: EdgeDirection::Falling,
                time,
            }) => Edge {
                direction: EdgeDirection::Rising(self.config.color),
                time: time + period - self.config.burst_duration,
            },
        };

        self.last_edge = Some(result);
        Some(result)
    }
}

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
            set y 0
            wait 0 irq 0

        .wrap_target
            out x 8
            in y 8
            in x 8
            out x 8
        wait1:
            jmp x-- wait1 [19]
        .wrap
        "#
    };

    let mut timing_config = Config::default();
    timing_config.use_program(&pio.common.load_program(&timing_program.program), &[]);
    let target_frequency = 2_000.;
    let clock_divider = (clk_sys_freq() as f64) / target_frequency;
    timing_config.clock_divider = FixedU32::<U8>::checked_from_num(clock_divider).unwrap();
    timing_config.shift_out = ShiftConfig {
        direction: embassy_rp::pio::ShiftDirection::Left,
        auto_fill: true,
        threshold: 16,
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
        PwmChannel::A,
        1,
        0,
    );

    let pwm_slice_green = p.PWM_SLICE2.number();
    let pwm = Pwm::new_output_a(p.PWM_SLICE2, green_pin, pwm_config.clone());
    core::mem::forget(pwm);

    sync_pio_to_pwm(
        [p.DMA_CH2.degrade(), p.DMA_CH3.degrade()],
        pwm_slice_green,
        PwmChannel::A,
        1,
        1,
    );

    let pwm_slice_blue = p.PWM_SLICE3.number();
    let pwm = Pwm::new_output_a(p.PWM_SLICE3, blue_pin, pwm_config.clone());
    core::mem::forget(pwm);

    sync_pio_to_pwm(
        [p.DMA_CH4.degrade(), p.DMA_CH5.degrade()],
        pwm_slice_blue,
        PwmChannel::A,
        1,
        2,
    );

    let mut stream_iter =
        StreamConfig::new(Color(10, 0, 0), Hz(1.), Duration::from_millis(400), None).into_iter();

    let mut last_edge = None;

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
    let mut red_data: Vec<u16, 256> = Vec::new();
    let mut green_data: Vec<u16, 256> = Vec::new();
    let mut blue_data: Vec<u16, 256> = Vec::new();

    pio.irq_flags.set_all(0);

    loop {
        info!("Loop");
        red_data.clear();
        green_data.clear();
        blue_data.clear();
        while !red_data.is_full() {
            let edge = stream_iter.next().unwrap();
            red_data.push(edge.encode_red(&last_edge)).unwrap();
            green_data.push(edge.encode_green(&last_edge)).unwrap();
            blue_data.push(edge.encode_blue(&last_edge)).unwrap();
            last_edge = Some(edge);
        }
        info!("Push");
        join3(
            pio.sm0.tx().dma_push(dmas.0.reborrow(), &red_data),
            pio.sm1.tx().dma_push(dmas.1.reborrow(), &green_data),
            pio.sm2.tx().dma_push(dmas.2.reborrow(), &blue_data),
        )
        .await;
    }
}

enum PwmChannel {
    A,
    B,
}

fn sync_pio_to_pwm(
    dmas: [AnyChannel; 2],
    pwm_slice: usize,
    channel: PwmChannel,
    pio_number: u8,
    sm: u8,
) {
    let raw_pwm = pac::PWM.ch(pwm_slice);

    let treq_sel = pac::dma::vals::TreqSel::from(pio_number * 8 + sm + 4);

    let [first_dma, second_dma] = dmas;
    let r = first_dma.regs();

    let offset = match channel {
        PwmChannel::A => 0,
        PwmChannel::B => 2,
    };

    r.write_addr()
        .write_value(raw_pwm.cc().as_ptr() as u32 + offset);
    r.read_addr()
        .write_value(pac::PIO1.rxf(sm as usize).as_ptr() as u32);
    r.trans_count().write_value(u32::MAX);
    r.al1_ctrl().write(|val| {
        let mut w = CtrlTrig(0);
        w.set_treq_sel(treq_sel);
        w.set_data_size(u8::size());
        w.set_chain_to(second_dma.number());
        w.set_incr_read(false);
        w.set_incr_write(false);
        w.set_en(true);

        *val = w.0;
    });

    let r = second_dma.regs();

    r.write_addr()
        .write_value(raw_pwm.cc().as_ptr() as u32 + offset);
    r.read_addr()
        .write_value(pac::PIO1.rxf(sm as usize).as_ptr() as u32);
    r.trans_count().write_value(u32::MAX);
    r.ctrl_trig().write(|w| {
        w.set_treq_sel(treq_sel);
        w.set_data_size(u8::size());
        w.set_chain_to(first_dma.number());
        w.set_incr_read(false);
        w.set_incr_write(false);
        w.set_en(true);
    });
}
