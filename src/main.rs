#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::{
    self, bind_interrupts,
    clocks::clk_sys_freq,
    pac::SIO,
    peripherals::PIO1,
    pio::{Config, InterruptHandler, Pio},
};
use pio_proc::pio_asm;

use crate::network::{Cyw43, NetworkConfig, network_task, wifi_task};

use {defmt_rtt as _, panic_probe as _};

mod network;

bind_interrupts!(struct Irqs {
    PIO1_IRQ_0 => InterruptHandler<PIO1>;
});

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let locked_state = SIO.spinlock_st();
    info!("Spinlock state {:b}", locked_state.read());

    SIO.spinlock(31).write_value(1);

    let p = embassy_rp::init(Default::default());

    let (cyw43, runner) = Cyw43::new(NetworkConfig {
        pwr_pin: p.PIN_23,
        cs_pin: p.PIN_25,
        pio: p.PIO0,
        dio_pin: p.PIN_24,
        clk_pin: p.PIN_29,
        dma: p.DMA_CH0,
    })
    .await;

    spawner.must_spawn(wifi_task(runner));

    let cyw43 = cyw43.init().await;

    const CLIENT_NAME: &str = "fountain";

    let (cyw43, runner) = cyw43.init_stack(CLIENT_NAME).await;

    spawner.must_spawn(network_task(runner));

    let ssid = env!("WIFI_SSID");
    let password = env!("WIFI_PASSWORD");
    let cyw43 = cyw43.join(ssid, password).await;

    let mut pio = Pio::new(p.PIO1, Irqs);

    let motor_pin = pio.common.make_pio_pin(p.PIN_2);
    let red_pin = pio.common.make_pio_pin(p.PIN_3);
    let green_pin = pio.common.make_pio_pin(p.PIN_4);
    let blue_pin = pio.common.make_pio_pin(p.PIN_5);

    let pins = [&motor_pin, &red_pin, &green_pin, &blue_pin];

    let program = pio_asm!(
        r#"
            set pindirs, 0b1111
            set pins, 0
        .wrap_target
            nop
        .wrap
    "#
    );

    let mut config = Config::default();
    config.set_out_pins(&pins);
    config.set_set_pins(&pins);
    config.use_program(&pio.common.load_program(&program.program), &[]);

    let mut sm = pio.sm0;
    sm.set_config(&config);
    sm.set_enable(true);

    let (rx, tx) = sm.rx_tx();
    loop {}
}
