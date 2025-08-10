#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::{
    self, bind_interrupts,
    pac::SIO,
    peripherals::PIO1,
    pio::{Config, InterruptHandler, Pio},
};
use embassy_time::{Instant, Timer};
use pio_proc::pio_asm;

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
            set pins, 1
            set x, 31
        wait1:
            jmp x-- wait1 [31]

            set pins, 0
            set x, 31
        wait2:
            jmp x-- wait2 [31]
        .wrap
    "#
    );

    let mut config = Config::default();
    config.set_out_pins(&pins);
    config.set_set_pins(&pins);
    config.use_program(&pio.common.load_program(&program.program), &[]);
    config.clock_divider = u16::MAX.into();

    let mut sm = pio.sm0;
    sm.set_config(&config);
    sm.set_enable(true);

    loop {
        Timer::at(Instant::MAX).await;
    }
}
