use embassy_time::{Duration, Instant};
use heapless::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color(pub u8, pub u8, pub u8);

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
pub struct Hz(pub f32);

impl Hz {
    pub fn as_duration(self) -> Duration {
        Duration::from_micros((1e6 / self.0) as u64)
    }
}

#[derive(Clone, Copy)]
pub struct StreamConfig {
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
pub struct ColorStep {
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

pub struct Config<const N: usize> {
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

pub struct ColorStepIterator<const N: usize> {
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
