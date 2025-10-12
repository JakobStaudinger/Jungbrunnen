#![no_std]
#![no_main]

mod led_orchestrator;
mod peripherals;
mod stream;

use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::{self, pac::SIO};
use embassy_time::{Instant, Timer};

use crate::led_orchestrator::orchestrate_leds;
use crate::peripherals::{AssignedResources, LedPeripherals};

use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let locked_state = SIO.spinlock_st();
    info!("Spinlock state {:b}", locked_state.read());

    SIO.spinlock(31).write_value(1);

    let p = embassy_rp::init(Default::default());
    let p = split_resources!(p);

    spawner.must_spawn(orchestrate_leds(p.led));

    loop {
        Timer::at(Instant::MAX).await
    }
}
