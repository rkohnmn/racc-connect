use crate::client::{ProcessRunner, SystemProcessRunner, TailscaleClient};
use crate::error::IdentityError;
use crate::model::{diff_peers, HostCapability, Peer, PeerEvent, SelfNode};
use crate::probe::{probe_peer, ProbeTransport, TcpProbeTransport, DEFAULT_CONTROL_PORT};
use racc_net::{validate_bind_addr, BindPolicy};
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

/// Maximum peer records retained by a refresh.
pub const MAX_DISCOVERY_PEERS: usize = 256;
/// Maximum peers probed in one refresh.
pub const MAX_PROBED_PEERS: usize = 64;
/// Maximum simultaneous probe workers.
pub const MAX_CONCURRENT_PROBES: usize = 8;
/// Largest timeout accepted for each project Hello probe.
pub const MAX_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Finite peer-probe limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiscoveryConfig {
    /// Project control port.
    pub control_port: u16,
    /// Deadline for each connect, write, and read operation.
    pub probe_timeout: Duration,
    /// Maximum online peers probed.
    pub max_probed_peers: usize,
    /// Maximum concurrent probe workers.
    pub max_concurrent_probes: usize,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            control_port: DEFAULT_CONTROL_PORT,
            probe_timeout: Duration::from_millis(250),
            max_probed_peers: MAX_PROBED_PEERS,
            max_concurrent_probes: MAX_CONCURRENT_PROBES,
        }
    }
}

/// Peer data and membership/path events from one successful refresh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerRefresh {
    /// Local node from Tailscale status, if present.
    pub self_node: Option<SelfNode>,
    /// Tailnet peers and their project Hello probe result.
    pub peers: Vec<Peer>,
    /// Online, offline, and route changes since the previous successful refresh.
    pub events: Vec<PeerEvent>,
}

/// Bounded Tailscale status refresh and project host-capability probe.
pub struct PeerDiscovery<
    R: ProcessRunner = SystemProcessRunner,
    P: ProbeTransport = TcpProbeTransport,
> {
    tailscale: TailscaleClient<R>,
    transport: P,
    config: DiscoveryConfig,
    previous: Vec<Peer>,
}

impl PeerDiscovery<SystemProcessRunner, TcpProbeTransport> {
    /// Creates a production discovery client using the installed Tailscale CLI.
    pub fn system() -> Result<Self, IdentityError> {
        Self::new(
            TailscaleClient::system(),
            TcpProbeTransport,
            DiscoveryConfig::default(),
        )
    }
}

impl<R: ProcessRunner, P: ProbeTransport> PeerDiscovery<R, P> {
    /// Creates a refresher after validating all configured bounds.
    pub fn new(
        tailscale: TailscaleClient<R>,
        transport: P,
        config: DiscoveryConfig,
    ) -> Result<Self, IdentityError> {
        if config.control_port == 0
            || config.probe_timeout.is_zero()
            || config.probe_timeout > MAX_PROBE_TIMEOUT
            || config.max_probed_peers > MAX_PROBED_PEERS
            || config.max_concurrent_probes == 0
            || config.max_concurrent_probes > MAX_CONCURRENT_PROBES
        {
            return Err(IdentityError::InvalidDiscoveryConfig);
        }
        Ok(Self {
            tailscale,
            transport,
            config,
            previous: Vec::new(),
        })
    }

    /// Refreshes tailnet peers and probes only validated Tailscale addresses.
    ///
    /// At most 256 peer results, 64 probes, and 8 probe workers are retained.
    /// The production transport applies the finite timeout to each socket operation.
    pub fn refresh(&mut self) -> Result<PeerRefresh, IdentityError> {
        let status = self.tailscale.status()?;
        if status.peers.len() > MAX_DISCOVERY_PEERS {
            return Err(IdentityError::TooManyPeers);
        }
        let candidates = status
            .peers
            .iter()
            .enumerate()
            .filter(|(_, peer)| peer.online)
            .filter_map(|(index, peer)| {
                peer.addresses
                    .iter()
                    .copied()
                    .find(|address| validate_bind_addr(*address, BindPolicy::Tailscale).is_ok())
                    .map(|address| (index, address))
            })
            .take(self.config.max_probed_peers)
            .collect::<Vec<_>>();
        let mut peers = status.peers;
        let capabilities =
            probe_candidates(&candidates, peers.len(), self.config, &self.transport)?;
        for (peer, capability) in peers.iter_mut().zip(capabilities) {
            peer.host_capability = capability;
        }
        let events = diff_peers(&self.previous, &peers);
        self.previous.clone_from(&peers);
        Ok(PeerRefresh {
            self_node: status.self_node,
            peers,
            events,
        })
    }
}

fn probe_candidates(
    candidates: &[(usize, IpAddr)],
    peer_count: usize,
    config: DiscoveryConfig,
    transport: &impl ProbeTransport,
) -> Result<Vec<HostCapability>, IdentityError> {
    if candidates.is_empty() {
        return Ok(vec![HostCapability::Unknown; peer_count]);
    }
    let next = AtomicUsize::new(0);
    let results = Mutex::new(vec![HostCapability::Unknown; peer_count]);
    let count = config.max_concurrent_probes.min(candidates.len());
    let mut worker_failed = false;
    thread::scope(|scope| {
        let mut workers = Vec::with_capacity(count);
        for _ in 0..count {
            workers.push(scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some((peer_index, address)) = candidates.get(i).copied() else {
                    break;
                };
                let capability = probe_peer(
                    address,
                    config.control_port,
                    config.probe_timeout,
                    transport,
                )
                .unwrap_or(HostCapability::NotHost);
                match results.lock() {
                    Ok(mut results) => results[peer_index] = capability,
                    Err(_) => break,
                }
            }));
        }
        for worker in workers {
            worker_failed |= worker.join().is_err();
        }
    });
    if worker_failed {
        return Err(IdentityError::DiscoveryWorkerFailed);
    }
    results
        .into_inner()
        .map_err(|_| IdentityError::DiscoveryWorkerFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{CliConfig, ProcessFailure, ProcessOutput, ProcessRequest};
    use crate::probe::ProbeError;
    use racc_proto::{Hello, HelloAck, HelloStatus, OsType, PROTOCOL_VERSION};
    use std::collections::{BTreeMap, VecDeque};
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

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
            let bytes = self
                .0
                .lock()
                .map_err(|_| ProcessFailure::Io("lock".to_owned()))?
                .pop_front()
                .ok_or_else(|| ProcessFailure::Io("missing status".to_owned()))?;
            if bytes.len() > cap {
                return Err(ProcessFailure::OutputTooLarge);
            }
            Ok(ProcessOutput {
                exit_code: Some(0),
                stdout: bytes,
                stderr: Vec::new(),
            })
        }
    }

    #[derive(Clone)]
    struct FakeProbe {
        replies: Arc<BTreeMap<IpAddr, Result<HelloAck, ProbeError>>>,
        seen: Arc<Mutex<Vec<IpAddr>>>,
        active: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }
    impl ProbeTransport for FakeProbe {
        fn exchange_hello(
            &self,
            address: std::net::SocketAddr,
            _: &Hello,
            timeout: Duration,
        ) -> Result<HelloAck, ProbeError> {
            if timeout > MAX_PROBE_TIMEOUT {
                return Err(ProbeError::Timeout);
            }
            let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(address.ip());
            }
            thread::sleep(Duration::from_millis(8));
            self.active.fetch_sub(1, Ordering::SeqCst);
            self.replies
                .get(&address.ip())
                .cloned()
                .unwrap_or(Err(ProbeError::Unavailable))
        }
    }

    fn ack(status: HelloStatus) -> HelloAck {
        HelloAck {
            protocol_version: PROTOCOL_VERSION,
            status,
            device_name: "host".to_owned(),
            os: OsType::Windows,
            app_version: "test".to_owned(),
            codecs: 1,
            max_height: 720,
            features: 0,
            host_cpu_cores: 0,
        }
    }

    fn status(peers: &[(String, String, bool)]) -> Vec<u8> {
        let peers = peers.iter().enumerate().map(|(i, (id, ip, online))|
            format!(r#""p{i}":{{"ID":"{id}","HostName":"{id}","TailscaleIPs":["{ip}"],"Online":{online}}}"#))
            .collect::<Vec<_>>().join(",");
        format!(r#"{{"BackendState":"Running","Self":{{"HostName":"local"}},"Peer":{{{peers}}}}}"#)
            .into_bytes()
    }

    fn probe(
        replies: impl IntoIterator<Item = (IpAddr, Result<HelloAck, ProbeError>)>,
    ) -> FakeProbe {
        FakeProbe {
            replies: Arc::new(replies.into_iter().collect()),
            seen: Arc::new(Mutex::new(Vec::new())),
            active: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn discovery(
        statuses: impl IntoIterator<Item = Vec<u8>>,
        probe: FakeProbe,
        config: DiscoveryConfig,
    ) -> PeerDiscovery<Runner, FakeProbe> {
        let runner = Runner(Arc::new(Mutex::new(statuses.into_iter().collect())));
        let client = TailscaleClient::with_executable(
            PathBuf::from("tailscale-test"),
            runner,
            CliConfig {
                command_timeout: Duration::from_secs(1),
                max_output_bytes: 16_384,
                status_ttl: Duration::ZERO,
                whois_ttl: Duration::ZERO,
            },
        );
        PeerDiscovery::new(client, probe, config).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn refresh_requires_a_project_hello_ack_and_skips_non_tailnet_ips() {
        let ip = "100.64.0.10".parse().unwrap();
        let fake = probe([(ip, Ok(ack(HelloStatus::Busy)))]);
        let seen = Arc::clone(&fake.seen);
        let mut client = discovery(
            [status(&[
                ("host".to_owned(), "100.64.0.10".to_owned(), true),
                ("other".to_owned(), "192.0.2.1".to_owned(), true),
            ])],
            fake,
            DiscoveryConfig::default(),
        );
        let result = client.refresh();
        assert!(result.is_ok());
        if let Ok(result) = result {
            assert!(result
                .peers
                .iter()
                .any(|p| p.node_id.as_deref() == Some("host")
                    && p.host_capability == HostCapability::HostCapable));
            assert!(result
                .peers
                .iter()
                .any(|p| p.node_id.as_deref() == Some("other")
                    && p.host_capability == HostCapability::Unknown));
        }
        assert_eq!(seen.lock().map(|v| v.clone()).unwrap_or_default(), vec![ip]);
    }

    #[test]
    fn failed_project_handshake_is_not_host_and_parallelism_is_bounded() {
        let mut peers = Vec::new();
        let mut replies = Vec::new();
        for i in 10..26 {
            let ip = format!("100.64.0.{i}").parse().unwrap();
            peers.push((format!("node-{i}"), format!("100.64.0.{i}"), true));
            replies.push((ip, Err(ProbeError::UnexpectedMessage)));
        }
        let fake = probe(replies);
        let peak = Arc::clone(&fake.peak);
        let mut client = discovery(
            [status(&peers)],
            fake,
            DiscoveryConfig {
                max_concurrent_probes: 3,
                ..DiscoveryConfig::default()
            },
        );
        let result = client.refresh();
        assert!(result.is_ok());
        if let Ok(result) = result {
            assert!(result
                .peers
                .iter()
                .all(|peer| peer.host_capability != HostCapability::HostCapable));
        }
        assert!(peak.load(Ordering::SeqCst) <= 3);
    }

    #[test]
    fn refresh_emits_online_appearance_and_offline_disappearance_events() {
        let fake = probe([]);
        let mut client = discovery(
            [
                status(&[
                    ("kept".to_owned(), "100.64.0.10".to_owned(), true),
                    ("gone".to_owned(), "100.64.0.11".to_owned(), true),
                ]),
                status(&[("kept".to_owned(), "100.64.0.10".to_owned(), true)]),
            ],
            fake,
            DiscoveryConfig::default(),
        );
        let first = client.refresh();
        let second = client.refresh();
        assert!(first.is_ok() && second.is_ok());
        if let (Ok(first), Ok(second)) = (first, second) {
            assert!(first.events.iter().any(|event| matches!(event, PeerEvent::PeerOnline { node_id: Some(id), .. } if id == "gone")));
            assert!(second.events.contains(&PeerEvent::PeerOffline {
                node_id: Some("gone".to_owned()),
                host_name: Some("gone".to_owned())
            }));
        }
    }

    #[test]
    fn discovery_rejects_unbounded_probe_settings() {
        let runner = Runner(Arc::new(Mutex::new(VecDeque::new())));
        let client = TailscaleClient::with_executable(
            PathBuf::from("tailscale-test"),
            runner,
            CliConfig::default(),
        );
        assert!(matches!(
            PeerDiscovery::new(
                client,
                TcpProbeTransport,
                DiscoveryConfig {
                    probe_timeout: Duration::from_secs(1),
                    ..DiscoveryConfig::default()
                }
            ),
            Err(IdentityError::InvalidDiscoveryConfig)
        ));
    }
}
