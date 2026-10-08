use crate::{CoreEvent, DeviceId, DeviceSnapshot};
use racc_identity::{
    HostCapability, IdentityError, PeerDiscovery as IdentityPeerDiscovery, ProbeTransport,
    ProcessRunner, SystemProcessRunner, TcpProbeTransport,
};
use racc_net::{validate_bind_addr, BindPolicy};
use racc_proto::OsType;
use racc_telemetry::PathKind;
use std::net::IpAddr;

/// A Tailscale peer paired with the UI-safe device metadata and validated addresses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredPeer {
    /// Metadata exposed through the core event bus.
    pub device: DeviceSnapshot,
    /// Peer addresses accepted by the production Tailscale bind policy.
    pub addresses: Vec<IpAddr>,
    /// Compact route classification reported by Tailscale status.
    pub path: PathKind,
}

/// One bounded device-discovery result for the UI adapter.
#[derive(Clone, Debug)]
pub struct DeviceDiscoveryUpdate {
    /// Local machine display name, if Tailscale reports one.
    pub local_device_name: Option<String>,
    /// Local IP addresses accepted by the production Tailscale bind policy.
    pub local_addresses: Vec<IpAddr>,
    /// Current peer list, including offline peers still present in Tailscale status.
    pub peers: Vec<DiscoveredPeer>,
    /// Metadata-only events for peer appearance, disappearance, or presence.
    pub events: Vec<CoreEvent>,
}

/// Core adapter from racc-identity records to UI-safe peer metadata.
pub struct CoreDeviceDiscovery<
    R: ProcessRunner = SystemProcessRunner,
    P: ProbeTransport = TcpProbeTransport,
> {
    identity: IdentityPeerDiscovery<R, P>,
    previous_ids: std::collections::BTreeSet<DeviceId>,
}

impl CoreDeviceDiscovery<SystemProcessRunner, TcpProbeTransport> {
    /// Creates a production core discovery adapter.
    pub fn system() -> Result<Self, IdentityError> {
        IdentityPeerDiscovery::system().map(Self::new)
    }
}

impl<R: ProcessRunner, P: ProbeTransport> CoreDeviceDiscovery<R, P> {
    /// Wraps a configured identity discovery service.
    pub fn new(identity: IdentityPeerDiscovery<R, P>) -> Self {
        Self {
            identity,
            previous_ids: std::collections::BTreeSet::new(),
        }
    }

    /// Refreshes the Tailscale peer list and emits bounded UI metadata.
    pub fn refresh(&mut self) -> Result<DeviceDiscoveryUpdate, IdentityError> {
        let refreshed = self.identity.refresh()?;
        let local_device_name = refreshed
            .self_node
            .as_ref()
            .and_then(|node| node.display_name.clone().or_else(|| node.dns_name.clone()));
        let local_addresses = refreshed
            .self_node
            .as_ref()
            .into_iter()
            .flat_map(|node| [node.ipv4, node.ipv6])
            .flatten()
            .filter(|address| validate_bind_addr(*address, BindPolicy::Tailscale).is_ok())
            .collect::<Vec<_>>();
        let peers = refreshed
            .peers
            .into_iter()
            .filter_map(to_discovered_peer)
            .collect::<Vec<_>>();
        let current_ids = peers
            .iter()
            .map(|peer| peer.device.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let mut events = peers
            .iter()
            .map(|peer| CoreEvent::DeviceDiscovered(peer.device.clone()))
            .collect::<Vec<_>>();
        events.extend(
            self.previous_ids
                .difference(&current_ids)
                .cloned()
                .map(|device_id| CoreEvent::DeviceOffline { device_id }),
        );
        self.previous_ids = current_ids;
        Ok(DeviceDiscoveryUpdate {
            local_device_name,
            local_addresses,
            peers,
            events,
        })
    }
}

fn to_discovered_peer(peer: racc_identity::Peer) -> Option<DiscoveredPeer> {
    let path = telemetry_path_kind(peer.path);
    let addresses = peer
        .addresses
        .into_iter()
        .filter(|address| validate_bind_addr(*address, BindPolicy::Tailscale).is_ok())
        .collect::<Vec<_>>();
    let id = peer
        .node_id
        .clone()
        .or_else(|| addresses.first().map(ToString::to_string))
        .and_then(|value| DeviceId::new(value).ok())?;
    let name = peer
        .host_name
        .clone()
        .or_else(|| peer.dns_name.clone())
        .or_else(|| addresses.first().map(ToString::to_string))
        .unwrap_or_else(|| id.to_string());
    let os = peer.os.as_deref().map(parse_os).unwrap_or(OsType::Unknown);
    let device = DeviceSnapshot {
        id,
        name,
        os,
        online: peer.online,
        host_capable: peer.host_capability == HostCapability::HostCapable,
        displays: Vec::new(),
        streamed_display: None,
    };
    Some(DiscoveredPeer {
        device,
        addresses,
        path,
    })
}

fn telemetry_path_kind(path: racc_identity::ConnectionPath) -> PathKind {
    match path {
        racc_identity::ConnectionPath::Direct => PathKind::Direct,
        racc_identity::ConnectionPath::Derp { .. } => PathKind::Derp,
        racc_identity::ConnectionPath::Unknown => PathKind::Unknown,
    }
}

fn parse_os(value: &str) -> OsType {
    match value.trim().to_ascii_lowercase().as_str() {
        "windows" => OsType::Windows,
        "macos" | "mac os" | "darwin" => OsType::MacOs,
        "linux" => OsType::Linux,
        _ => OsType::Unknown,
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;
    use racc_identity::ConnectionPath;

    #[test]
    fn route_mapping_discards_derp_region_and_keeps_compact_path() {
        assert_eq!(
            telemetry_path_kind(ConnectionPath::Direct),
            PathKind::Direct
        );
        assert_eq!(
            telemetry_path_kind(ConnectionPath::Derp {
                region: Some("private-region-name".to_owned()),
            }),
            PathKind::Derp
        );
        assert_eq!(
            telemetry_path_kind(ConnectionPath::Unknown),
            PathKind::Unknown
        );
    }
    use crate::CoreEvent;
    use racc_identity::{
        CliConfig, DiscoveryConfig, ProbeError, ProbeTransport, ProcessFailure, ProcessOutput,
        ProcessRequest, TailscaleClient,
    };
    use racc_proto::{Hello, HelloAck, HelloStatus, PROTOCOL_VERSION};
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Clone)]
    struct Runner(Arc<Mutex<VecDeque<Vec<u8>>>>);

    impl ProcessRunner for Runner {
        fn run(
            &self,
            request: &ProcessRequest,
            _: Duration,
            cap: usize,
        ) -> Result<ProcessOutput, ProcessFailure> {
            if request.args != [OsString::from("status"), OsString::from("--json")] {
                return Err(ProcessFailure::Io("unexpected command".to_owned()));
            }
            let stdout = self
                .0
                .lock()
                .map_err(|_| ProcessFailure::Io("lock".to_owned()))?
                .pop_front()
                .ok_or_else(|| ProcessFailure::Io("missing status".to_owned()))?;
            if stdout.len() > cap {
                return Err(ProcessFailure::OutputTooLarge);
            }
            Ok(ProcessOutput {
                exit_code: Some(0),
                stdout,
                stderr: Vec::new(),
            })
        }
    }

    struct AckProbe;
    impl ProbeTransport for AckProbe {
        fn exchange_hello(
            &self,
            _: std::net::SocketAddr,
            _: &Hello,
            _: Duration,
        ) -> Result<HelloAck, ProbeError> {
            Ok(HelloAck {
                protocol_version: PROTOCOL_VERSION,
                status: HelloStatus::Ok,
                device_name: "workstation".to_owned(),
                os: OsType::Windows,
                app_version: "test".to_owned(),
                codecs: 1,
                max_height: 720,
                features: 0,
                host_cpu_cores: 0,
            })
        }
    }

    #[test]
    fn core_refresh_maps_only_validated_peers_and_hello_success() {
        let runner = Runner(Arc::new(Mutex::new(VecDeque::from([br#"{
          "BackendState":"Running",
          "Self":{"HostName":"local-machine","TailscaleIPs":["100.64.0.3","fd7a:115c:a1e0::3","192.168.1.3"]},
          "Peer":{
            "one":{"ID":"node-one","HostName":"workstation","OS":"windows","Online":true,"TailscaleIPs":["100.64.0.10"]},
            "two":{"ID":"node-two","HostName":"invalid","OS":"windows","Online":true,"TailscaleIPs":["192.0.2.44"]}
          }
        }"#
        .to_vec()]))));
        let client = TailscaleClient::with_executable(
            PathBuf::from("tailscale-test"),
            runner,
            CliConfig {
                command_timeout: Duration::from_secs(1),
                max_output_bytes: 4096,
                status_ttl: Duration::ZERO,
                whois_ttl: Duration::ZERO,
            },
        );
        let identity = IdentityPeerDiscovery::new(client, AckProbe, DiscoveryConfig::default())
            .unwrap_or_else(|error| panic!("{error}"));
        let mut discovery = CoreDeviceDiscovery::new(identity);
        let update = discovery.refresh();
        assert!(update.is_ok());
        if let Ok(update) = update {
            assert_eq!(update.local_device_name.as_deref(), Some("local-machine"));
            assert_eq!(
                update.local_addresses,
                vec![
                    "100.64.0.3".parse::<std::net::IpAddr>().unwrap(),
                    "fd7a:115c:a1e0::3".parse::<std::net::IpAddr>().unwrap(),
                ]
            );
            let accepted = update
                .peers
                .iter()
                .find(|peer| peer.device.id.as_str() == "node-one");
            assert!(accepted.is_some_and(|peer| {
                peer.device.host_capable
                    && peer.device.os == OsType::Windows
                    && peer.addresses == vec!["100.64.0.10".parse::<std::net::IpAddr>().unwrap()]
            }));
            let rejected = update
                .peers
                .iter()
                .find(|peer| peer.device.id.as_str() == "node-two");
            assert!(rejected
                .is_some_and(|peer| { !peer.device.host_capable && peer.addresses.is_empty() }));
            assert!(update.events.iter().any(|event| matches!(
                event,
                CoreEvent::DeviceDiscovered(device)
                    if device.id.as_str() == "node-one" && device.host_capable
            )));
        }
    }

    #[test]
    fn core_update_emits_offline_event_when_peer_disappears() {
        let runner = Runner(Arc::new(Mutex::new(VecDeque::from([
            br#"{"BackendState":"Running","Peer":{"one":{"Online":true,"TailscaleIPs":["100.64.0.10"]}}}"#.to_vec(),
            br#"{"BackendState":"Running","Peer":{}}"#.to_vec(),
        ]))));
        let client = TailscaleClient::with_executable(
            PathBuf::from("tailscale-test"),
            runner,
            CliConfig {
                command_timeout: Duration::from_secs(1),
                max_output_bytes: 4096,
                status_ttl: Duration::ZERO,
                whois_ttl: Duration::ZERO,
            },
        );
        let identity = IdentityPeerDiscovery::new(client, AckProbe, DiscoveryConfig::default())
            .unwrap_or_else(|error| panic!("{error}"));
        let mut discovery = CoreDeviceDiscovery::new(identity);
        assert!(discovery.refresh().is_ok());
        let second = discovery.refresh();
        assert!(
            second.is_ok_and(|update| update.events.iter().any(|event| matches!(
                event,
                CoreEvent::DeviceOffline { device_id } if device_id.as_str() == "100.64.0.10"
            )))
        );
    }
}
