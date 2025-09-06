#![no_std]
#![no_main]

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
use embassy_time::{Duration, Instant};
use fixed::{FixedU32, types::extra::U8};
use heapless::Vec;
use pio_proc::pio_asm;

use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    PIO1_IRQ_0 => InterruptHandler<PIO1>;
});

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Color(u8, u8, u8);

impl Color {
    pub fn black() -> Color {
        Color(0, 0, 0)
    }

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

impl Hz {
    pub fn as_duration(self) -> Duration {
        Duration::from_micros((1e6 / self.0) as u64)
    }
}

#[derive(Clone, Copy)]
struct StreamConfig {
    color: Color,
    frequency: Hz,
    burst_duration: Duration,
    offset: Duration,
}

impl StreamConfig {
    pub fn get_color_at_instant(&self, instant: Instant) -> Color {
        if instant < self.get_start() {
            return Color::black();
        }

        let period = self.frequency.as_duration();
        if (instant.as_micros() - self.offset.as_micros()) % period.as_micros()
            < self.burst_duration.as_micros()
        {
            self.color
        } else {
            Color::black()
        }
    }

    pub fn get_next_change_after(&self, instant: Option<Instant>) -> Instant {
        let start = self.get_start();
        match instant {
            None => start,
            Some(instant) => {
                if instant < start {
                    start
                } else {
                    let period = self.frequency.as_duration();
                    let phase = Duration::from_micros(
                        (instant.as_micros() - self.offset.as_micros()) % period.as_micros(),
                    );

                    if phase < self.burst_duration {
                        instant + self.burst_duration - phase
                    } else {
                        instant + period - phase
                    }
                }
            }
        }
    }

    fn get_start(&self) -> Instant {
        Instant::MIN + self.offset
    }
}

#[derive(Clone, Copy, Debug)]
struct ColorStep {
    color: Color,
    delay: u32,
}

impl ColorStep {
    pub fn encode_red(&self) -> u32 {
        self.encode(|color| color.r())
    }

    pub fn encode_green(&self) -> u32 {
        self.encode(|color| color.g())
    }

    pub fn encode_blue(&self) -> u32 {
        self.encode(|color| color.b())
    }

    fn encode(&self, get_color_component: impl FnOnce(Color) -> u8) -> u32 {
        (get_color_component(self.color) as u32) << 24 | self.delay & 0xFFFFFF
    }
}

struct Config<const N: usize> {
    streams: Vec<StreamConfig, N>,
    micros_per_tick: i32,
    tick_overhead: i32,
}

impl<const N: usize> Config<N> {
    pub fn new(streams: &[StreamConfig; N], micros_per_tick: i32, tick_overhead: i32) -> Self {
        Self {
            streams: Vec::from_slice(streams).unwrap(),
            micros_per_tick,
            tick_overhead,
        }
    }
}

impl<const N: usize> IntoIterator for Config<N> {
    type Item = ColorStep;
    type IntoIter = ColorStepIterator<N>;

    fn into_iter(self) -> Self::IntoIter {
        ColorStepIterator::new(self)
    }
}

struct ColorStepIterator<const N: usize> {
    config: Config<N>,
    current_time: Option<Instant>,
}

impl<const N: usize> ColorStepIterator<N> {
    pub fn new(config: Config<N>) -> Self {
        Self {
            config,
            current_time: None,
        }
    }

    fn get_next_time_after(&self, instant: Option<Instant>) -> Option<Instant> {
        self.config
            .streams
            .iter()
            .map(|stream| stream.get_next_change_after(instant))
            .min()
    }
}

impl<const N: usize> Iterator for ColorStepIterator<N> {
    type Item = ColorStep;

    fn next(&mut self) -> Option<Self::Item> {
        let next_time = self.get_next_time_after(self.current_time);
        let next_time = next_time.unwrap();
        let current_time = self.current_time.unwrap_or(Instant::MIN);

        let color = self
            .config
            .streams
            .iter()
            .map(|stream| stream.get_color_at_instant(current_time))
            .fold((0_u32, 0_u32, 0_u32), |sum, color| {
                (
                    sum.0 + color.r() as u32,
                    sum.1 + color.g() as u32,
                    sum.2 + color.b() as u32,
                )
            });

        let max = color.0.max(color.1).max(color.2);
        let diff = next_time - current_time;
        let delay = ((diff.as_micros() / self.config.micros_per_tick as u64) as u32)
            .saturating_sub(self.config.tick_overhead as u32);

        self.current_time = Some(next_time);

        let color = if max > 255 {
            let normalize = |val| (val * 255 / max) as u8;
            Color(normalize(color.0), normalize(color.1), normalize(color.2))
        } else {
            Color(color.0 as u8, color.1 as u8, color.2 as u8)
        };

        Some(ColorStep { color, delay })
    }
}

impl StreamConfig {
    pub fn new(
        color: Color,
        frequency: Hz,
        burst_duration: Duration,
        offset: Option<Duration>,
    ) -> Self {
        core::assert!(burst_duration <= frequency.as_duration());

        Self {
            color,
            frequency,
            burst_duration,
            offset: offset.unwrap_or_default(),
        }
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

    let mut config = Config::new(
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
