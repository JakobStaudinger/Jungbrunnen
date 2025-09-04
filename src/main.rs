#![no_std]
#![no_main]

use core::cmp::Ordering;

use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::{
    self, Peripheral, bind_interrupts,
    clocks::clk_sys_freq,
    dma::{AnyChannel, Channel, Word},
    pac::{self, SIO, dma::regs::CtrlTrig},
    peripherals::PIO1,
    pio::{Config, InterruptHandler, Pio, ShiftConfig},
    pwm::{self, Pwm},
};
use embassy_time::{Duration, Instant};
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
    pub fn encode(self, last_edge: Option<Edge>) -> u32 {
        let delay = self.time - last_edge.map(|edge| edge.time).unwrap_or(Instant::MIN);
        let delay = (delay.as_millis() / 10) as u32 & 0xFF;

        match self.direction {
            EdgeDirection::Falling => 0x00000000 | delay,
            EdgeDirection::Rising(color) => {
                (color.0 as u32) << 24 | (color.1 as u32) << 16 | (color.2 as u32) << 8 | delay
            }
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
async fn main(spawner: Spawner) {
    let locked_state = SIO.spinlock_st();
    info!("Spinlock state {:b}", locked_state.read());

    SIO.spinlock(31).write_value(1);

    let p = embassy_rp::init(Default::default());

    let mut pio = Pio::new(p.PIO1, Irqs);

    // let red_pin = pio.common.make_pio_pin(p.PIN_2);
    // let green_pin = pio.common.make_pio_pin(p.PIN_3);
    // let blue_pin = pio.common.make_pio_pin(p.PIN_4);

    let timing_program = pio_asm! {
        r#"
            set y 0

        .wrap_target
            pull
            out x 24
            in x 24
            in x 8
            push
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
        auto_fill: false,
        threshold: 0,
    };
    timing_config.shift_in = ShiftConfig {
        direction: embassy_rp::pio::ShiftDirection::Left,
        auto_fill: false,
        threshold: 0,
    };

    let mut sm = pio.sm0;
    sm.set_config(&timing_config);
    sm.set_enable(true);

    let tx = sm.tx();
    let mut config = pwm::Config::default();
    config.enable = false;
    let pwm = Pwm::new_output_a(p.PWM_SLICE2, p.PIN_4, config);

    spawner.must_spawn(sync_pio_to_pwm(
        [p.DMA_CH0.degrade(), p.DMA_CH1.degrade()],
        pwm,
    ));

    let mut stream_iter = StreamConfig::new(
        Color(255, 255, 255),
        Hz(1.),
        Duration::from_millis(400),
        None,
    )
    .into_iter();

    let mut dma = p.DMA_CH2.into_ref();

    let mut data: Vec<u32, 256> = Vec::new();
    let mut last_edge = None;

    loop {
        info!("Loop");
        data.clear();
        while !data.is_full() {
            let edge = stream_iter.next().unwrap();
            data.push(edge.encode(last_edge)).unwrap();
            info!("{:x}", edge.encode(last_edge));
            last_edge = Some(edge);
        }
        info!("Push");
        tx.dma_push(dma.reborrow(), &data).await;
    }
}

#[embassy_executor::task]
async fn sync_pio_to_pwm(dmas: [AnyChannel; 2], mut pwm: Pwm<'static>) {
    let raw_pwm = pac::PWM.ch(2);

    let pio_no = 1;
    let sm = 0;

    let [first_dma, second_dma] = dmas;
    let r = first_dma.regs();

    r.write_addr().write_value(raw_pwm.cc().as_ptr() as u32);
    r.read_addr().write_value(pac::PIO1.rxf(0).as_ptr() as u32);
    r.trans_count().write_value(u32::MAX);
    r.al1_ctrl().write(|val| {
        let mut w = CtrlTrig(0);
        // Set RX DREQ for this statemachine
        w.set_treq_sel(crate::pac::dma::vals::TreqSel::from(pio_no * 8 + sm + 4));
        w.set_data_size(u16::size());
        w.set_chain_to(second_dma.number());
        w.set_incr_read(false);
        w.set_incr_write(false);
        w.set_en(true);

        *val = w.0;
    });

    let r = second_dma.regs();

    r.write_addr().write_value(raw_pwm.cc().as_ptr() as u32);
    r.read_addr().write_value(pac::PIO1.rxf(0).as_ptr() as u32);
    r.trans_count().write_value(u32::MAX);
    r.ctrl_trig().write(|w| {
        // Set RX DREQ for this statemachine
        w.set_treq_sel(crate::pac::dma::vals::TreqSel::from(pio_no * 8 + sm + 4));
        w.set_data_size(u16::size());
        w.set_chain_to(first_dma.number());
        w.set_incr_read(false);
        w.set_incr_write(false);
        w.set_en(true);
    });

    let mut config = pwm::Config::default();
    config.top = 256;
    config.compare_a = 0;
    config.enable = true;

    pwm.set_config(&config);
}
