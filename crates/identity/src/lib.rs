//! Tailscale CLI identity, peer discovery, host probing, and allowlist policy.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod allowlist;
mod client;
mod discovery;
mod error;
mod model;
mod parse;
mod probe;

pub use allowlist::{
    allowlist_path, AccessState, Allowlist, AllowlistEntry, AllowlistEvent, AllowlistLoad,
    AllowlistStore, ApprovalState, FileAllowlistStore, MAX_ALLOWLIST_BYTES, MAX_ALLOWLIST_ENTRIES,
    MAX_PENDING_APPROVALS,
};
pub use client::{
    locate_tailscale_cli, CliConfig, ProcessFailure, ProcessOutput, ProcessRequest, ProcessRunner,
    SystemProcessRunner, TailscaleClient, DEFAULT_COMMAND_TIMEOUT, DEFAULT_STATUS_TTL,
    DEFAULT_WHOIS_TTL, MAX_COMMAND_OUTPUT_BYTES,
};
pub use discovery::{
    DiscoveryConfig, PeerDiscovery, PeerRefresh, MAX_CONCURRENT_PROBES, MAX_DISCOVERY_PEERS,
    MAX_PROBED_PEERS, MAX_PROBE_TIMEOUT,
};
pub use error::{IdentityError, WhoisError};
pub use model::{
    diff_peers, ConnectionPath, HostCapability, Peer, PeerEvent, PeerIdentity, SelfNode,
    StatusSnapshot, WhoisIdentity,
};
pub use parse::{parse_status_json, parse_whois_json};
pub use probe::{probe_peer, ProbeError, ProbeTransport, TcpProbeTransport, DEFAULT_CONTROL_PORT};

#[cfg(test)]
mod tests;
