use embassy_net::tcp::TcpSocket;
use mqttrs::Packet;

use super::error::{MqttError, Result};

pub(crate) trait MqttSocket {
    async fn send_packet(&mut self, packet: &Packet<'_>) -> Result<()>;
    async fn read_packet<'s>(&mut self, buf: &'s mut [u8]) -> Result<Option<Packet<'s>>>;
}

impl<'a> MqttSocket for TcpSocket<'a> {
    async fn send_packet(&mut self, packet: &Packet<'_>) -> Result<()> {
        let mut buf = [0; 2048];
        let size = mqttrs::encode_slice(packet, &mut buf).map_err(|_| MqttError::EncodeError)?;

        self.write(&buf[0..size]).await.map(|_| ())?;

        Ok(())
    }

    async fn read_packet<'s>(&mut self, buf: &'s mut [u8]) -> Result<Option<Packet<'s>>> {
        let count = self.read(buf).await?;

        mqttrs::decode_slice(&buf[0..count]).map_err(|_| MqttError::DecodeError)
    }
}
