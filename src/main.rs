#![no_std]
#![no_main]

mod led_orchestrator;
mod network;
mod peripherals;
mod stream;

use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::{self, pac::SIO};
use embassy_time::{Instant, Timer};

use crate::led_orchestrator::orchestrate_leds;
use crate::network::{Cyw43, network_task, wifi_task};
use crate::peripherals::{AssignedResources, LedPeripherals, WifiPeripherals};

use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let locked_state = SIO.spinlock_st();
    info!("Spinlock state {:b}", locked_state.read());

    SIO.spinlock(31).write_value(1);

    let p = embassy_rp::init(Default::default());
    let p = split_resources!(p);

    let (cyw43, runner) = Cyw43::new(p.wifi).await;

    spawner.must_spawn(wifi_task(runner));

    let cyw43 = cyw43.init().await;

    const CLIENT_NAME: &str = "picow";

    let (cyw43, runner) = cyw43.init_stack(CLIENT_NAME).await;

    spawner.must_spawn(network_task(runner));

    let ssid = env!("WIFI_SSID");
    let password = env!("WIFI_PASSWORD");
    // let cyw43 = cyw43.join(ssid, password).await;

    spawner.must_spawn(orchestrate_leds(p.led));

    loop {
        Timer::at(Instant::MAX).await
    }
}
