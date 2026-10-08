use crate::model::HostCapability;
use racc_net::{connect_control, BindPolicy, ControlError, ControlSettings};
use racc_proto::{ControlMessage, Hello, HelloAck, OsType, PROTOCOL_VERSION};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// Default TCP agent control and capability-probe port.
pub const DEFAULT_CONTROL_PORT: u16 = 47_473;

/// Failure while checking whether a peer speaks the project control protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeError {
    /// Address is outside the Tailscale interface ranges.
    AddressNotTailscale,
    /// TCP connection or protocol exchange failed.
    Unavailable,
    /// Connect, write, or read exceeded the requested timeout.
    Timeout,
    /// Peer returned a control message other than HelloAck.
    UnexpectedMessage,
}

/// Injectable project-handshake boundary for host-capability probing.
pub trait ProbeTransport: Send + Sync {
    /// Sends Hello to the peer and returns its bounded HelloAck response.
    fn exchange_hello(
        &self,
        address: SocketAddr,
        hello: &Hello,
        timeout: Duration,
    ) -> Result<HelloAck, ProbeError>;
}

/// TCP implementation using the existing Tailscale-only control connection.
#[derive(Clone, Copy, Debug, Default)]
pub struct TcpProbeTransport;

impl ProbeTransport for TcpProbeTransport {
    fn exchange_hello(
        &self,
        address: SocketAddr,
        hello: &Hello,
        timeout: Duration,
    ) -> Result<HelloAck, ProbeError> {
        let settings = ControlSettings {
            connect_timeout: timeout,
            write_timeout: timeout,
            read_timeout: Some(timeout),
            ..ControlSettings::default()
        };
        let mut connection =
            connect_control(address, BindPolicy::Tailscale, settings).map_err(map_control_error)?;
        connection
            .send(&ControlMessage::Hello(hello.clone()))
            .map_err(map_control_error)?;
        match connection.recv().map_err(map_control_error)? {
            ControlMessage::HelloAck(ack) => Ok(ack),
            _ => Err(ProbeError::UnexpectedMessage),
        }
    }
}

/// Probes one Tailscale peer and reports whether it answered with a project HelloAck.
///
/// A valid HelloAck establishes project host capability even when the host reports
/// Busy, NotAuthorized, or an unsupported protocol version.
pub fn probe_peer(
    peer_address: IpAddr,
    port: u16,
    timeout: Duration,
    transport: &impl ProbeTransport,
) -> Result<HostCapability, ProbeError> {
    if timeout.is_zero() {
        return Err(ProbeError::Timeout);
    }
    racc_net::validate_bind_addr(peer_address, BindPolicy::Tailscale)
        .map_err(|_| ProbeError::AddressNotTailscale)?;
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        device_name: "Racc Connect probe".to_owned(),
        os: local_os(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        video_udp_port: 0,
        codecs: 1,
        max_height: 480,
        features: 0,
    };
    match transport.exchange_hello(SocketAddr::new(peer_address, port), &hello, timeout) {
        Ok(_ack) => Ok(HostCapability::HostCapable),
        Err(ProbeError::AddressNotTailscale) => Err(ProbeError::AddressNotTailscale),
        Err(ProbeError::Timeout | ProbeError::Unavailable | ProbeError::UnexpectedMessage) => {
            Ok(HostCapability::NotHost)
        }
    }
}

fn map_control_error(error: ControlError) -> ProbeError {
    match error {
        ControlError::Timeout => ProbeError::Timeout,
        ControlError::Bind(_) => ProbeError::AddressNotTailscale,
        ControlError::Io(_) | ControlError::Proto(_) | ControlError::Closed => {
            ProbeError::Unavailable
        }
    }
}

fn local_os() -> OsType {
    #[cfg(target_os = "windows")]
    {
        OsType::Windows
    }
    #[cfg(target_os = "macos")]
    {
        OsType::MacOs
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        OsType::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_proto::HelloStatus;
    use std::sync::Mutex;

    struct FakeProbe {
        response: Mutex<Result<HelloAck, ProbeError>>,
        observed: Mutex<Option<(SocketAddr, Hello, Duration)>>,
    }

    impl ProbeTransport for FakeProbe {
        fn exchange_hello(
            &self,
            address: SocketAddr,
            hello: &Hello,
            timeout: Duration,
        ) -> Result<HelloAck, ProbeError> {
            if let Ok(mut observed) = self.observed.lock() {
                *observed = Some((address, hello.clone(), timeout));
            }
            self.response
                .lock()
                .map_err(|_| ProbeError::Unavailable)?
                .clone()
        }
    }

    fn ack(status: HelloStatus) -> HelloAck {
        HelloAck {
            protocol_version: PROTOCOL_VERSION,
            status,
            device_name: "host-a".to_owned(),
            os: OsType::Windows,
            app_version: "0.1".to_owned(),
            codecs: 1,
            max_height: 1080,
            features: 1,
            host_cpu_cores: 4,
        }
    }

    #[test]
    fn valid_hello_ack_marks_peer_host_capable_and_uses_configured_port() {
        let fake = FakeProbe {
            response: Mutex::new(Ok(ack(HelloStatus::Busy))),
            observed: Mutex::new(None),
        };
        let result = probe_peer(
            "100.64.0.10"
                .parse()
                .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            47_473,
            Duration::from_millis(300),
            &fake,
        );
        assert_eq!(result, Ok(HostCapability::HostCapable));
        let observed = fake.observed.lock().ok().and_then(|value| value.clone());
        assert!(observed.is_some());
        if let Some((address, hello, timeout)) = observed {
            assert_eq!(address.port(), DEFAULT_CONTROL_PORT);
            assert_eq!(hello.protocol_version, PROTOCOL_VERSION);
            assert_eq!(timeout, Duration::from_millis(300));
        }
    }

    #[test]
    fn unavailable_or_unexpected_peer_is_not_host() {
        for failure in [
            ProbeError::Unavailable,
            ProbeError::UnexpectedMessage,
            ProbeError::Timeout,
        ] {
            let fake = FakeProbe {
                response: Mutex::new(Err(failure)),
                observed: Mutex::new(None),
            };
            assert_eq!(
                probe_peer(
                    "100.64.0.10"
                        .parse()
                        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
                    DEFAULT_CONTROL_PORT,
                    Duration::from_secs(1),
                    &fake,
                ),
                Ok(HostCapability::NotHost)
            );
        }
    }

    #[test]
    fn probe_refuses_non_tailscale_addresses_before_connecting() {
        let fake = FakeProbe {
            response: Mutex::new(Ok(ack(HelloStatus::Ok))),
            observed: Mutex::new(None),
        };
        assert_eq!(
            probe_peer(
                "192.0.2.10"
                    .parse()
                    .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
                DEFAULT_CONTROL_PORT,
                Duration::from_secs(1),
                &fake,
            ),
            Err(ProbeError::AddressNotTailscale)
        );
        assert!(fake
            .observed
            .lock()
            .ok()
            .is_some_and(|value| value.is_none()));
    }
}
