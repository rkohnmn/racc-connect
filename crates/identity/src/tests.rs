use crate::{
    diff_peers, parse_status_json, parse_whois_json, CliConfig, ConnectionPath, HostCapability,
    IdentityError, Peer, PeerEvent, ProcessFailure, ProcessOutput, ProcessRequest, ProcessRunner,
    TailscaleClient,
};
use proptest::prelude::*;
use racc_net::{validate_bind_addr, BindPolicy};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn fixture(name: &str) -> &'static str {
    match name {
        "status_multi.json" => include_str!("../tests/fixtures/status_multi.json"),
        "status_sparse.json" => include_str!("../tests/fixtures/status_sparse.json"),
        "status_empty.json" => include_str!("../tests/fixtures/status_empty.json"),
        "status_logged_out.json" => include_str!("../tests/fixtures/status_logged_out.json"),
        "status_offline_stale_path.json" => {
            include_str!("../tests/fixtures/status_offline_stale_path.json")
        }
        "whois_user.json" => include_str!("../tests/fixtures/whois_user.json"),
        "whois_node.json" => include_str!("../tests/fixtures/whois_node.json"),
        "whois_tagged.json" => include_str!("../tests/fixtures/whois_tagged.json"),
        "whois_ambiguous.json" => include_str!("../tests/fixtures/whois_ambiguous.json"),
        _ => "",
    }
}

#[test]
fn status_fixture_maps_direct_relay_offline_tags_and_owner() {
    let parsed = parse_status_json(fixture("status_multi.json").as_bytes());
    assert!(parsed.is_ok());
    let status = match parsed {
        Ok(value) => value,
        Err(_) => return,
    };
    assert_eq!(status.backend_state.as_deref(), Some("Running"));
    assert_eq!(status.version.as_deref(), Some("1.84.0"));
    let self_node = status.self_node.as_ref();
    assert!(self_node.is_some());
    if let Some(node) = self_node {
        assert_eq!(node.display_name.as_deref(), Some("host-a"));
        assert_eq!(
            node.ipv4,
            Some(
                "192.0.2.10"
                    .parse()
                    .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
            )
        );
        assert!(node.online);
    }
    assert_eq!(status.peers.len(), 3);
    let direct = status
        .peers
        .iter()
        .find(|peer| peer.node_id.as_deref() == Some("node-b"));
    assert!(direct.is_some());
    if let Some(peer) = direct {
        assert!(peer.online);
        assert_eq!(peer.path, ConnectionPath::Direct);
        assert_eq!(peer.owner_login.as_deref(), Some("owner-a"));
        assert_eq!(peer.tags, vec!["tag:viewer"]);
        assert_eq!(peer.host_capability, HostCapability::Unknown);
    }
    let relay = status
        .peers
        .iter()
        .find(|peer| peer.node_id.as_deref() == Some("23"));
    assert!(relay.is_some());
    if let Some(peer) = relay {
        assert_eq!(
            peer.path,
            ConnectionPath::Derp {
                region: Some("nyc".to_owned())
            }
        );
    }
    let offline = status
        .peers
        .iter()
        .find(|peer| peer.node_id.as_deref() == Some("node-d"));
    assert!(offline.is_some_and(|peer| !peer.online && peer.path == ConnectionPath::Unknown));
}

#[test]
fn status_parser_tolerates_missing_fields_and_unknown_fields() {
    let parsed = parse_status_json(fixture("status_sparse.json").as_bytes());
    assert!(parsed.is_ok());
    if let Ok(status) = parsed {
        assert!(status.self_node.is_none());
        assert_eq!(status.peers.len(), 1);
        assert_eq!(status.peers[0].node_id, None);
        assert!(status.peers[0].online);
        assert_eq!(status.peers[0].path, ConnectionPath::Unknown);
    }
}

#[test]
fn offline_peer_ignores_stale_direct_and_relay_route_fields() {
    let parsed = parse_status_json(fixture("status_offline_stale_path.json").as_bytes());
    assert!(parsed.is_ok());
    if let Ok(status) = parsed {
        assert_eq!(status.peers.len(), 1);
        assert!(!status.peers[0].online);
        assert_eq!(status.peers[0].path, ConnectionPath::Unknown);
    }
}

#[test]
fn empty_tailnet_and_logged_out_state_are_distinguished() {
    let empty = parse_status_json(fixture("status_empty.json").as_bytes());
    assert!(empty.is_ok_and(|status| status.peers.is_empty()));
    assert_eq!(
        parse_status_json(fixture("status_logged_out.json").as_bytes()),
        Err(IdentityError::NotLoggedIn)
    );
    assert_eq!(parse_status_json(b"not json"), Err(IdentityError::BadJson));
}

#[test]
fn current_node_whois_shape_maps_domain_fields_and_prefers_stable_id() {
    let parsed = parse_whois_json(fixture("whois_node.json").as_bytes());
    assert!(parsed.is_ok());
    if let Ok(identity) = parsed {
        assert_eq!(identity.node_id, "stable-node-current");
        assert_eq!(identity.owner_login.as_deref(), Some("owner-current"));
        assert_eq!(identity.node_name.as_deref(), Some("host-current"));
        assert_eq!(identity.tags, vec!["tag:host", "tag:viewer"]);
        assert_eq!(
            identity.addresses,
            vec![
                "198.51.100.40"
                    .parse()
                    .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                "2001:db8::40"
                    .parse()
                    .unwrap_or(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)),
            ]
        );
    }
}

#[test]
fn whois_fixtures_map_user_and_tagged_nodes() {
    let user = parse_whois_json(fixture("whois_user.json").as_bytes());
    assert!(user.is_ok());
    if let Ok(identity) = user {
        assert_eq!(identity.node_id, "node-b");
        assert_eq!(identity.owner_login.as_deref(), Some("owner-a"));
        assert_eq!(identity.node_name.as_deref(), Some("host-b"));
        assert_eq!(
            identity.addresses,
            vec!["198.51.100.20"
                .parse()
                .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))]
        );
    }
    let tagged = parse_whois_json(fixture("whois_tagged.json").as_bytes());
    assert!(tagged.is_ok());
    if let Ok(identity) = tagged {
        assert_eq!(identity.node_id, "node-tagged");
        assert_eq!(identity.owner_login, None);
        assert_eq!(identity.tags, vec!["tag:host"]);
    }
    assert_eq!(
        parse_whois_json(fixture("whois_ambiguous.json").as_bytes()),
        Err(IdentityError::AmbiguousWhois)
    );
}

#[test]
fn fixture_hygiene_has_no_tailnet_ranges_or_email_like_identifiers() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let entries = fs::read_dir(root);
    assert!(entries.is_ok());
    let mut inspected = 0_usize;
    if let Ok(entries) = entries {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let bytes = fs::read(&path);
            assert!(
                bytes.is_ok(),
                "fixture should be readable: {}",
                path.display()
            );
            let text = bytes
                .ok()
                .and_then(|value| String::from_utf8(value).ok())
                .unwrap_or_default();
            assert!(
                !text.contains('@'),
                "fixture must not contain an email marker"
            );
            for token in text.split(|character: char| {
                !(character.is_ascii_hexdigit() || matches!(character, '.' | ':' | '/'))
            }) {
                let candidate = token.split('/').next().unwrap_or_default();
                if let Ok(address) = candidate.parse::<IpAddr>() {
                    assert!(
                        !is_tailscale_address(address),
                        "fixture contains a Tailscale-range address"
                    );
                }
            }
            inspected += 1;
        }
    }
    assert!(inspected >= 6);
}

fn is_tailscale_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            (u32::from(address) & 0xffc0_0000) == u32::from(Ipv4Addr::new(100, 64, 0, 0))
        }
        IpAddr::V6(address) => address.octets()[..6] == [0xfd, 0x7a, 0x11, 0x5c, 0xa1, 0xe0],
    }
}

proptest! {
    #[test]
    fn arbitrary_command_bytes_never_panic_parsers(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = parse_status_json(&bytes);
        let _ = parse_whois_json(&bytes);
    }
}

#[derive(Clone, Default)]
struct FakeRunner {
    results: Arc<Mutex<VecDeque<Result<ProcessOutput, ProcessFailure>>>>,
    calls: Arc<Mutex<Vec<(ProcessRequest, Duration, usize)>>>,
}

impl FakeRunner {
    fn with_results(
        results: impl IntoIterator<Item = Result<ProcessOutput, ProcessFailure>>,
    ) -> Self {
        Self {
            results: Arc::new(Mutex::new(results.into_iter().collect())),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<(ProcessRequest, Duration, usize)> {
        self.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }
}

impl ProcessRunner for FakeRunner {
    fn run(
        &self,
        request: &ProcessRequest,
        timeout: Duration,
        max_output_bytes: usize,
    ) -> Result<ProcessOutput, ProcessFailure> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push((request.clone(), timeout, max_output_bytes));
        }
        self.results
            .lock()
            .map_err(|_| ProcessFailure::Io("fake lock poisoned".to_owned()))?
            .pop_front()
            .unwrap_or_else(|| Err(ProcessFailure::Io("no fake result queued".to_owned())))
    }
}

fn process_output(stdout: impl Into<Vec<u8>>) -> Result<ProcessOutput, ProcessFailure> {
    Ok(ProcessOutput {
        exit_code: Some(0),
        stdout: stdout.into(),
        stderr: Vec::new(),
    })
}

fn client(runner: FakeRunner, config: CliConfig) -> TailscaleClient<FakeRunner> {
    TailscaleClient::with_executable(PathBuf::from("tailscale-test"), runner, config)
}

#[test]
fn cli_calls_use_separate_arguments_cache_status_and_validate_timeout_bounds() {
    let runner = FakeRunner::with_results([process_output(
        fixture("status_empty.json").as_bytes().to_vec(),
    )]);
    let config = CliConfig {
        command_timeout: Duration::from_millis(1800),
        max_output_bytes: 4096,
        status_ttl: Duration::from_secs(5),
        whois_ttl: Duration::from_secs(1),
    };
    let client = client(runner.clone(), config);
    assert!(client.status().is_ok());
    assert!(client.status().is_ok());
    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0.executable, PathBuf::from("tailscale-test"));
    assert_eq!(
        calls[0].0.args,
        vec![OsString::from("status"), OsString::from("--json")]
    );
    assert_eq!(calls[0].1, Duration::from_millis(1800));
    assert_eq!(calls[0].2, 4096);
}

#[test]
fn cli_timeout_and_output_cap_failures_remain_typed() {
    let runner = FakeRunner::with_results([
        Err(ProcessFailure::Timeout),
        Err(ProcessFailure::OutputTooLarge),
    ]);
    let client = client(runner.clone(), CliConfig::default());
    assert_eq!(client.status(), Err(IdentityError::Timeout));
    assert_eq!(client.status(), Err(IdentityError::OutputTooLarge));
    let calls = runner.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].1, Duration::from_secs(2));
    assert_eq!(calls[0].2, crate::MAX_COMMAND_OUTPUT_BYTES);
}

#[test]
fn whois_command_uses_documented_flag_order_and_rejects_non_tailnet_address() {
    let runner = FakeRunner::with_results([process_output(
        fixture("whois_user.json").as_bytes().to_vec(),
    )]);
    let client = client(runner.clone(), CliConfig::default());
    let address = "100.64.0.10"
        .parse::<IpAddr>()
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert!(client.resolve(address).is_ok());
    let calls = runner.calls();
    assert_eq!(
        calls[0].0.args,
        vec![
            OsString::from("whois"),
            OsString::from("--json"),
            OsString::from("100.64.0.10")
        ]
    );
    assert_eq!(
        client.resolve(
            "192.0.2.10"
                .parse()
                .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
        ),
        Err(IdentityError::AddressNotTailscale)
    );
    assert_eq!(calls.len(), 1);
}

#[test]
fn self_bind_address_prefers_validated_ipv4_then_uses_ipv6_fallback() {
    let runner = FakeRunner::with_results([
        process_output(b"192.0.2.10\n".to_vec()),
        process_output(b"fd7a:115c:a1e0::1\n".to_vec()),
    ]);
    let client = client(runner.clone(), CliConfig::default());
    let selected = client.self_bind_addr();
    assert_eq!(
        selected,
        Ok("fd7a:115c:a1e0::1"
            .parse::<IpAddr>()
            .unwrap_or(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)))
    );
    if let Ok(address) = selected {
        assert!(validate_bind_addr(address, BindPolicy::Tailscale).is_ok());
    }
}

#[test]
fn peer_diff_emits_online_offline_and_path_events() {
    let previous = vec![
        peer("node-a", true, ConnectionPath::Direct),
        peer("node-b", true, ConnectionPath::Unknown),
        peer("node-c", false, ConnectionPath::Unknown),
    ];
    let current = vec![
        peer(
            "node-a",
            true,
            ConnectionPath::Derp {
                region: Some("nyc".to_owned()),
            },
        ),
        peer("node-b", false, ConnectionPath::Unknown),
        peer("node-c", true, ConnectionPath::Direct),
        peer("node-d", true, ConnectionPath::Unknown),
    ];
    let events = diff_peers(&previous, &current);
    assert!(events.contains(&PeerEvent::PathChanged {
        node_id: Some("node-a".to_owned()),
        old: ConnectionPath::Direct,
        new: ConnectionPath::Derp {
            region: Some("nyc".to_owned())
        },
    }));
    assert!(events.contains(&PeerEvent::PeerOffline {
        node_id: Some("node-b".to_owned()),
        host_name: Some("host-node-b".to_owned()),
    }));
    assert!(events.contains(&PeerEvent::PeerOnline {
        node_id: Some("node-c".to_owned()),
        host_name: Some("host-node-c".to_owned()),
    }));
    assert!(events.contains(&PeerEvent::PeerOnline {
        node_id: Some("node-d".to_owned()),
        host_name: Some("host-node-d".to_owned()),
    }));
}

fn peer(id: &str, online: bool, path: ConnectionPath) -> Peer {
    Peer {
        node_id: Some(id.to_owned()),
        host_name: Some(format!("host-{id}")),
        dns_name: None,
        addresses: Vec::new(),
        os: None,
        online,
        path,
        tags: Vec::new(),
        owner_login: None,
        host_capability: HostCapability::Unknown,
    }
}
