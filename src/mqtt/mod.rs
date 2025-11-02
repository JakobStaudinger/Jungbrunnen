use core::str::FromStr;
use error::{MqttError, Result};

use embassy_futures::select::{Either, select};
use embassy_net::{IpAddress, IpEndpoint, Stack, tcp::TcpSocket};
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    channel::{Receiver, Sender},
    pubsub::{Publisher, Subscriber},
};
use embassy_time::{Duration, Ticker};
use mqttrs::{Connect, Packet, Pid, Protocol, Publish, Subscribe};

mod error;
mod socket;

use socket::MqttSocket;

#[derive(Clone)]
pub enum RxPacket {
    Connected,
}

pub struct SubscribeTopic {
    pub qos: mqttrs::QoS,
    pub topic_path: &'static str,
}

#[allow(unused)]
pub enum TxPacket {
    Subscribe(&'static [SubscribeTopic]),
    Publish {
        qospid: mqttrs::QosPid,
        topic_name: &'static str,
        payload: &'static [u8],
    },
    Pingreq,
}

pub type MqttTxSender<'a> = Sender<'a, CriticalSectionRawMutex, TxPacket, 10>;
pub type MqttTxReceiver<'a> = Receiver<'a, CriticalSectionRawMutex, TxPacket, 10>;

pub type MqttRxPublisher<'a> = Publisher<'a, CriticalSectionRawMutex, RxPacket, 10, 10, 1>;
pub type MqttRxSubscriber<'a> = Subscriber<'a, CriticalSectionRawMutex, RxPacket, 10, 10, 1>;

#[embassy_executor::task]
pub async fn mqtt_heartbeat(sender: MqttTxSender<'static>) -> ! {
    let mut ticker = Ticker::every(Duration::from_secs(30));

    loop {
        ticker.next().await;
        sender.send(TxPacket::Pingreq).await;
    }
}

#[embassy_executor::task]
pub async fn mqtt_task(
    runner: MqttRunner<'static>,
    receiver: MqttTxReceiver<'static>,
    sender: MqttRxPublisher<'static>,
) -> ! {
    runner.run(receiver, sender).await.unwrap();

    unreachable!("MQTT runner should never exit")
}

pub struct MqttRunner<'a> {
    stack: Stack<'a>,
    options: ConnectionOptions<'a>,
    rx_buffer: [u8; 2048],
    tx_buffer: [u8; 2048],
}

pub struct ConnectionOptions<'a> {
    pub address: ServerAddress<'a>,
    pub client_id: &'a str,
    pub credentials: Option<Credentials<'a>>,
}

#[allow(unused)]
pub enum ServerAddress<'a> {
    Ip(IpAddress),
    HostName(&'a str),
}

pub struct Credentials<'a> {
    pub username: &'a str,
    pub password: &'a [u8],
}

impl<'a: 'static> MqttRunner<'a> {
    pub fn new(stack: Stack<'a>, options: ConnectionOptions<'a>) -> Self {
        Self {
            stack,
            options,
            rx_buffer: [0; 2048],
            tx_buffer: [0; 2048],
        }
    }

    pub async fn run(
        mut self,
        receiver: MqttTxReceiver<'a>,
        publisher: MqttRxPublisher<'a>,
    ) -> Result<()> {
        let address = MqttRunner::resolve_server_address(self.options.address, self.stack).await?;
        let mut socket = MqttRunner::connect(
            address,
            self.stack,
            &mut self.rx_buffer,
            &mut self.tx_buffer,
            self.options.client_id,
            self.options
                .credentials
                .as_ref()
                .map(|credentials| credentials.username),
            self.options
                .credentials
                .as_ref()
                .map(|credentials| credentials.password),
        )
        .await?;

        let mut buf = [0; 2048];

        loop {
            let result = select(socket.read_packet(&mut buf), receiver.receive()).await;

            match result {
                Either::First(Ok(Some(packet))) => {
                    MqttRunner::handle_receive(packet, &publisher).await?
                }
                Either::Second(packet) => MqttRunner::handle_transmit(&mut socket, packet).await?,
                _ => {}
            }
        }
    }

    async fn connect<'b, const R: usize, const T: usize>(
        address: IpAddress,
        stack: Stack<'b>,
        rx_buffer: &'b mut [u8; R],
        tx_buffer: &'b mut [u8; T],
        client_id: &str,
        username: Option<&str>,
        password: Option<&[u8]>,
    ) -> Result<TcpSocket<'b>> {
        let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
        socket.set_timeout(Some(Duration::from_secs(60)));
        socket.set_keep_alive(Some(Duration::from_secs(30)));
        socket.connect(IpEndpoint::new(address, 1883)).await?;

        let connect = Connect {
            protocol: Protocol::MQTT311,
            keep_alive: 60,
            clean_session: true,
            client_id,
            last_will: None,
            username,
            password,
        }
        .into();

        socket.send_packet(&connect).await?;

        Ok(socket)
    }

    async fn resolve_server_address(
        address: ServerAddress<'_>,
        stack: Stack<'a>,
    ) -> Result<IpAddress> {
        match address {
            ServerAddress::Ip(ip) => Ok(ip),
            ServerAddress::HostName(name) => {
                let server_address = stack
                    .dns_query(name, embassy_net::dns::DnsQueryType::A)
                    .await?;

                Ok(*server_address.first().ok_or(MqttError::DnsError)?)
            }
        }
    }

    async fn handle_receive(packet: Packet<'_>, publisher: &MqttRxPublisher<'_>) -> Result<()> {
        match packet {
            Packet::Publish(Publish {
                payload,
                topic_name,
                ..
            }) => match (topic_name, core::str::from_utf8(payload)?) {
                _ => {}
            },
            Packet::Connack(_) => {
                publisher.publish(RxPacket::Connected).await;
            }
            _ => {}
        }

        Ok(())
    }

    async fn handle_transmit(socket: &mut TcpSocket<'_>, packet: TxPacket) -> Result<()> {
        match packet {
            TxPacket::Subscribe(topics) => {
                let topics = topics
                    .iter()
                    .map(|topic| {
                        Ok(mqttrs::SubscribeTopic {
                            qos: topic.qos,
                            topic_path: heapless_07::String::from_str(topic.topic_path)?,
                        })
                    })
                    .collect::<Result<heapless_07::Vec<_, 5>>>()?;

                let packet = Packet::Subscribe(Subscribe {
                    pid: Pid::new(),
                    topics,
                });
                socket.send_packet(&packet).await?;
            }
            TxPacket::Publish {
                qospid,
                topic_name,
                payload,
            } => {
                socket
                    .send_packet(
                        &Publish {
                            dup: false,
                            retain: false,
                            qospid,
                            topic_name,
                            payload,
                        }
                        .into(),
                    )
                    .await?
            }
            TxPacket::Pingreq => socket.send_packet(&mqttrs::Packet::Pingreq).await?,
        }

        Ok(())
    }
}
