use core::str::FromStr;

use cyw43::{Control, JoinOptions};
use cyw43_pio::{DEFAULT_CLOCK_DIVIDER, PioSpi};
use defmt::*;
use embassy_net::{Config, DhcpConfig, Stack, StackResources};
use embassy_rp::{
    bind_interrupts,
    clocks::RoscRng,
    gpio::{Level, Output},
    peripherals::{DMA_CH9, PIO0},
    pio::{InterruptHandler as PioInterruptHandler, Pio},
};
use embassy_time::{Duration, Timer, with_timeout};
use heapless::String;
use state::{Initialized, Joined, Uninitialized, WithStack};
use static_cell::StaticCell;

use crate::peripherals::WifiPeripherals;

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
});

#[embassy_executor::task]
pub async fn wifi_task(runner: WiFiRunner<PIO0, 0, DMA_CH9>) -> ! {
    runner.run().await;
}

#[embassy_executor::task]
pub async fn network_task(mut runner: NetworkRunner) -> ! {
    runner.run().await;
}

mod state {
    use cyw43::NetDriver;
    use embassy_net::Stack;

    pub struct Uninitialized<'a> {
        pub(super) net_device: NetDriver<'a>,
    }

    pub struct Initialized<'a> {
        pub(super) net_device: NetDriver<'a>,
    }

    pub struct WithStack<'a> {
        pub(super) stack: Stack<'a>,
    }

    pub struct Joined<'a> {
        pub(super) stack: Stack<'a>,
    }
}

pub struct Cyw43<'a, S> {
    pub control: Control<'a>,
    state: S,
}

pub type WiFiRunner<PIO, const SM: usize, DMA> =
    cyw43::Runner<'static, Output<'static>, PioSpi<'static, PIO, SM, DMA>>;

pub type NetworkRunner = embassy_net::Runner<'static, cyw43::NetDriver<'static>>;

impl<'a: 'static> Cyw43<'a, Uninitialized<'a>> {
    pub async fn new(
        p: WifiPeripherals,
    ) -> (Cyw43<'a, Uninitialized<'a>>, WiFiRunner<PIO0, 0, DMA_CH9>) {
        #[cfg(feature = "dev_firmware")]
        let firmware = unsafe { core::slice::from_raw_parts(0x1010_0000 as *const u8, 231077) };

        #[cfg(not(feature = "dev_firmware"))]
        let firmware: &[u8] = include_bytes!("../../cyw43-firmware/43439A0.bin");

        info!("Initializing PIO");

        let pwr = Output::new(p.pwr, Level::Low);
        let cs = Output::new(p.cs, Level::High);
        let mut pio = Pio::new(p.pio, Irqs);
        let spi = PioSpi::new(
            &mut pio.common,
            pio.sm0,
            DEFAULT_CLOCK_DIVIDER,
            pio.irq0,
            cs,
            p.dio,
            p.clk,
            p.dma,
        );

        info!("Initializing cyw43 driver");

        static STATE: StaticCell<cyw43::State> = StaticCell::new();
        let state = STATE.init(cyw43::State::new());
        let (net_device, control, runner) = cyw43::new(state, pwr, spi, firmware).await;

        (
            Cyw43 {
                control,
                state: Uninitialized { net_device },
            },
            runner,
        )
    }

    pub async fn init(mut self) -> Cyw43<'a, Initialized<'a>> {
        #[cfg(feature = "dev_firmware")]
        let clm = unsafe { core::slice::from_raw_parts(0x1014_0000 as *const u8, 984) };

        #[cfg(not(feature = "dev_firmware"))]
        let clm: &[u8] = include_bytes!("../../cyw43-firmware/43439A0_clm.bin");

        info!("Initializing control");
        self.control.init(clm).await;

        info!("Setting power management");
        self.control
            .set_power_management(cyw43::PowerManagementMode::PowerSave)
            .await;

        Cyw43 {
            control: self.control,
            state: Initialized {
                net_device: self.state.net_device,
            },
        }
    }
}

impl<'a: 'static> Cyw43<'a, Initialized<'a>> {
    pub async fn init_stack(self, client_name: &str) -> (Cyw43<'a, WithStack<'a>>, NetworkRunner) {
        let seed = RoscRng.next_u64();

        let mut dhcp_config = DhcpConfig::default();
        let str = String::from_str(client_name);
        dhcp_config.hostname = Some(str.unwrap());

        let net_config = Config::dhcpv4(dhcp_config);
        static RESOURCES: StaticCell<StackResources<16>> = StaticCell::new();

        let (stack, runner) = embassy_net::new(
            self.state.net_device,
            net_config,
            RESOURCES.init(StackResources::new()),
            seed,
        );

        let mac_addr = stack.hardware_address();
        info!("Hardware configured. MAC Address is {}", mac_addr);

        (
            Cyw43 {
                control: self.control,
                state: WithStack { stack },
            },
            runner,
        )
    }
}

impl<'a: 'static> Cyw43<'a, WithStack<'a>> {
    pub async fn join(mut self, ssid: &str, password: &str) -> Cyw43<'a, Joined<'a>> {
        info!("Trying to join {}", ssid);

        loop {
            let join_options = JoinOptions::new(password.as_bytes());
            match self.control.join(ssid, join_options).await {
                Ok(_) => break,
                Err(err) => {
                    info!("Join failed with status={}", err.status);
                    Timer::after_millis(500).await;
                }
            }
        }

        info!("Joined network {}!", ssid);

        let stack = self.state.stack;

        with_timeout(Duration::from_secs(60), stack.wait_config_up())
            .await
            .expect("Failed to establish network connection after 60 seconds");

        match stack.config_v4() {
            Some(a) => info!("IP address is {}", a.address),
            None => core::panic!("No IP address received from DHCP"),
        };

        Cyw43 {
            control: self.control,
            state: Joined { stack },
        }
    }
}

impl<'a: 'static> Cyw43<'a, Joined<'a>> {
    pub fn stack(&self) -> Stack<'a> {
        self.state.stack
    }
}
