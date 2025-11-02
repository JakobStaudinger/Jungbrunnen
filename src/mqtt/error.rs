use core::num::ParseIntError;

pub(crate) type Result<T> = core::result::Result<T, MqttError>;

#[derive(Debug, Clone)]
pub(crate) enum MqttError {
    Generic,
    TcpError,
    ConnectError,
    DnsError,
    EncodeError,
    DecodeError,
}

impl From<embassy_net::tcp::Error> for MqttError {
    fn from(_value: embassy_net::tcp::Error) -> Self {
        Self::TcpError
    }
}

impl From<embassy_net::dns::Error> for MqttError {
    fn from(_value: embassy_net::dns::Error) -> Self {
        Self::DnsError
    }
}

impl From<embassy_net::tcp::ConnectError> for MqttError {
    fn from(_value: embassy_net::tcp::ConnectError) -> Self {
        Self::ConnectError
    }
}

impl From<core::str::Utf8Error> for MqttError {
    fn from(_value: core::str::Utf8Error) -> Self {
        Self::DecodeError
    }
}

impl From<ParseIntError> for MqttError {
    fn from(_value: ParseIntError) -> Self {
        Self::DecodeError
    }
}

impl From<()> for MqttError {
    fn from(_value: ()) -> Self {
        Self::Generic
    }
}
