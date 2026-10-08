use std::collections::BTreeMap;
use std::net::IpAddr;

/// Current connection route observed by Tailscale for a peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionPath {
    /// Tailscale reports a current direct endpoint.
    Direct,
    /// Traffic is relayed through a DERP region, when reported.
    Derp {
        /// DERP region name reported by Tailscale, if available.
        region: Option<String>,
    },
    /// Tailscale has not reported a current endpoint or relay.
    Unknown,
}

/// Local Tailscale node information used for binding and display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelfNode {
    /// Tailscale machine name, if supplied by the CLI.
    pub display_name: Option<String>,
    /// Fully qualified MagicDNS name, if supplied by the CLI.
    pub dns_name: Option<String>,
    /// First reported IPv4 address, if present.
    pub ipv4: Option<IpAddr>,
    /// First reported IPv6 address, if present.
    pub ipv6: Option<IpAddr>,
    /// Whether the local node is reported online.
    pub online: bool,
}

/// One tailnet peer from the Tailscale status command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    /// Stable node identifier, when the CLI provides it.
    pub node_id: Option<String>,
    /// Machine hostname, when available.
    pub host_name: Option<String>,
    /// Fully qualified MagicDNS name, when available.
    pub dns_name: Option<String>,
    /// Tailscale addresses reported for the peer.
    pub addresses: Vec<IpAddr>,
    /// Operating system label, when available.
    pub os: Option<String>,
    /// Whether the peer is currently online.
    pub online: bool,
    /// Current direct, DERP, or unknown route.
    pub path: ConnectionPath,
    /// Tailscale tags reported for the peer.
    pub tags: Vec<String>,
    /// Owner login name, when the CLI includes it.
    pub owner_login: Option<String>,
    /// Whether the peer answered the project's Hello probe.
    pub host_capability: HostCapability,
}

/// Result of resolving an incoming Tailscale address with whois.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerIdentity {
    /// Stable node identifier required by the host allowlist.
    pub node_id: String,
    /// Owner login, when Tailscale reports a user owner.
    pub owner_login: Option<String>,
    /// Tailscale tags associated with the node.
    pub tags: Vec<String>,
    /// Tailscale machine name, when available.
    pub node_name: Option<String>,
    /// Tailscale IP addresses returned for the node.
    pub addresses: Vec<IpAddr>,
}

/// Alias emphasizing that a peer identity came from a whois lookup.
pub type WhoisIdentity = PeerIdentity;

/// Parsed result of a Tailscale status query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusSnapshot {
    /// Local node data, absent when the CLI omits it (for example while logged out).
    pub self_node: Option<SelfNode>,
    /// Tailnet peers, sorted by stable identifier and then hostname.
    pub peers: Vec<Peer>,
    /// Tailscale backend state, when supplied.
    pub backend_state: Option<String>,
    /// Tailscale CLI version, when supplied.
    pub version: Option<String>,
}

/// Host capability learned from the bounded project handshake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostCapability {
    /// No probe has been attempted or its result is unknown.
    Unknown,
    /// A project-compatible HelloAck was received.
    HostCapable,
    /// The address did not answer with a valid project HelloAck.
    NotHost,
}

/// Change emitted by comparing two Tailscale peer snapshots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerEvent {
    /// A previously absent or offline peer is online now.
    PeerOnline {
        /// Stable node identifier when known.
        node_id: Option<String>,
        /// Hostname when known.
        host_name: Option<String>,
    },
    /// A previously online peer is offline or absent now.
    PeerOffline {
        /// Stable node identifier when known.
        node_id: Option<String>,
        /// Hostname when known.
        host_name: Option<String>,
    },
    /// The observed route changed while the peer remained present.
    PathChanged {
        /// Stable node identifier when known.
        node_id: Option<String>,
        /// Previous Tailscale route.
        old: ConnectionPath,
        /// Current Tailscale route.
        new: ConnectionPath,
    },
}

/// Produces deterministic online/offline/path events between two peer snapshots.
pub fn diff_peers(previous: &[Peer], current: &[Peer]) -> Vec<PeerEvent> {
    let previous = index_peers(previous);
    let current = index_peers(current);
    let mut events = Vec::new();
    for (key, old) in &previous {
        match current.get(key) {
            None if old.online => events.push(PeerEvent::PeerOffline {
                node_id: old.node_id.clone(),
                host_name: old.host_name.clone(),
            }),
            Some(new) if old.online && !new.online => events.push(PeerEvent::PeerOffline {
                node_id: new.node_id.clone(),
                host_name: new.host_name.clone(),
            }),
            Some(new) if !old.online && new.online => events.push(PeerEvent::PeerOnline {
                node_id: new.node_id.clone(),
                host_name: new.host_name.clone(),
            }),
            Some(new) if old.path != new.path => events.push(PeerEvent::PathChanged {
                node_id: new.node_id.clone(),
                old: old.path.clone(),
                new: new.path.clone(),
            }),
            _ => {}
        }
    }
    for (key, new) in &current {
        if !previous.contains_key(key) && new.online {
            events.push(PeerEvent::PeerOnline {
                node_id: new.node_id.clone(),
                host_name: new.host_name.clone(),
            });
        }
    }
    events
}

fn index_peers(peers: &[Peer]) -> BTreeMap<String, &Peer> {
    peers
        .iter()
        .filter_map(|peer| {
            let key = peer
                .node_id
                .clone()
                .or_else(|| peer.addresses.first().map(ToString::to_string))?;
            Some((key, peer))
        })
        .collect()
}
