#![no_std]
#![no_main]

mod led_orchestrator;
mod mqtt;
mod network;
mod peripherals;
mod stream;

use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::{self, pac::SIO};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::pubsub::{PubSubChannel, WaitResult};
use embassy_time::{Instant, Timer};
use indoc::indoc;
use static_cell::StaticCell;

use crate::led_orchestrator::orchestrate_leds;
use crate::mqtt::{
    ConnectionOptions, Credentials, MqttRunner, MqttRxSubscriber, MqttTxSender, RxPacket,
    SubscribeTopic, TxPacket, mqtt_heartbeat, mqtt_task,
};
use crate::network::{Cyw43, network_task, wifi_task};
use crate::peripherals::{AssignedResources, LedPeripherals, WifiPeripherals};

use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::task]
async fn mqtt_autodiscovery_task(
    mut subscriber: MqttRxSubscriber<'static>,
    sender: MqttTxSender<'static>,
) {
    loop {
        let command = subscriber.next_message().await;
        let command = match command {
            WaitResult::Lagged(num) => {
                warn!("Lagged {} messages behind!", num);
                continue;
            }
            WaitResult::Message(command) => command,
        };

        if let RxPacket::Connected = command {
            sender
                .send(TxPacket::Subscribe(&[
                    SubscribeTopic {
                        qos: mqttrs::QoS::AtMostOnce,
                        topic_path: "picow/light/set",
                    },
                    SubscribeTopic {
                        qos: mqttrs::QoS::AtMostOnce,
                        topic_path: "picow/light/brightness/set",
                    },
                ]))
                .await;

            let autodiscovery = TxPacket::Publish {
                qospid: mqttrs::QosPid::AtMostOnce,
                topic_name: "homeassistant/device/picow/config",
                payload: indoc! {
                r#"{
                        "device": {
                            "identifiers": ["picow"],
                            "name": "PicoW",
                            "model": "Rasperry Pi Pico W",
                            "manufacturer": "Raspberry Pi"
                        },
                        "origin": {
                            "name": "Test"
                        },
                        "components": {
                        }
                    }"#
                }
                .as_bytes(),
            };

            sender.send(autodiscovery).await;
        }
    }
}

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
    let cyw43 = cyw43.join(ssid, password).await;

    let mqtt_runner = MqttRunner::new(
        cyw43.stack(),
        ConnectionOptions {
            address: mqtt::ServerAddress::HostName("homeassistant"),
            client_id: "picow",
            credentials: Credentials {
                username: "picow",
                password: "picow".as_bytes(),
            }
            .into(),
        },
    );

    static MQTT_TX_CHANNEL: StaticCell<Channel<CriticalSectionRawMutex, TxPacket, 10>> =
        StaticCell::new();

    let tx_channel = MQTT_TX_CHANNEL.init(Channel::new());

    static MQTT_RX_CHANNEL: StaticCell<
        PubSubChannel<CriticalSectionRawMutex, RxPacket, 10, 10, 1>,
    > = StaticCell::new();
    let rx_channel = MQTT_RX_CHANNEL.init(PubSubChannel::new());

    let autodiscovery_subscriber = rx_channel.subscriber().unwrap();

    spawner.must_spawn(mqtt_task(
        mqtt_runner,
        tx_channel.receiver(),
        rx_channel.publisher().unwrap(),
    ));
    spawner.must_spawn(mqtt_heartbeat(tx_channel.sender()));
    spawner.must_spawn(mqtt_autodiscovery_task(
        autodiscovery_subscriber,
        tx_channel.sender(),
    ));

    spawner.must_spawn(orchestrate_leds(p.led));

    loop {
        Timer::at(Instant::MAX).await
    }
}
