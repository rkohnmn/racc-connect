use std::fmt;

/// Typed failure from a Tailscale identity, allowlist, or discovery operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityError {
    /// The Tailscale command-line executable could not be found.
    NotInstalled,
    /// Tailscale is installed but its local backend is stopped or unavailable.
    NotRunning,
    /// Tailscale is not authenticated to a tailnet on this machine.
    NotLoggedIn,
    /// A bounded Tailscale command exceeded its deadline.
    Timeout,
    /// Command output exceeded its configured byte limit.
    OutputTooLarge,
    /// Tailscale returned syntactically invalid or structurally unusable JSON.
    BadJson,
    /// An operating-system or filesystem operation failed.
    Io(String),
    /// Tailscale command failed with a non-zero exit status.
    CommandFailed(i32),
    /// Address is outside the approved Tailscale IPv4 or IPv6 ranges.
    AddressNotTailscale,
    /// No address accepted by the production Tailscale bind policy is available.
    NoTailscaleAddress,
    /// Whois returned multiple possible nodes for one address.
    AmbiguousWhois,
    /// Whois data lacks the stable node identifier required by the allowlist.
    InvalidIdentity,
    /// A parsed peer list exceeds the bounded domain model limit.
    TooManyPeers,
    /// Allowlist persistence or validation failed.
    /// Discovery probe settings exceed the finite supported limits.
    InvalidDiscoveryConfig,
    /// A bounded discovery probe worker failed to join or publish its result.
    DiscoveryWorkerFailed,
    /// Allowlist persistence or validation failed.
    Allowlist(String),
}

/// Alias retained for APIs that specifically report a whois resolution error.
pub type WhoisError = IdentityError;

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInstalled => f.write_str("Tailscale CLI is not installed"),
            Self::NotRunning => f.write_str("Tailscale backend is not running"),
            Self::NotLoggedIn => f.write_str("Tailscale is not logged in"),
            Self::Timeout => f.write_str("Tailscale command timed out"),
            Self::OutputTooLarge => f.write_str("Tailscale command output exceeded its size limit"),
            Self::BadJson => f.write_str("Tailscale returned invalid JSON"),
            Self::Io(message) => write!(f, "operating-system I/O failed: {message}"),
            Self::CommandFailed(code) => write!(f, "Tailscale command exited with status {code}"),
            Self::AddressNotTailscale => f.write_str("address is outside the Tailscale ranges"),
            Self::NoTailscaleAddress => f.write_str("no usable Tailscale address is available"),
            Self::AmbiguousWhois => f.write_str("whois returned an ambiguous identity"),
            Self::InvalidIdentity => f.write_str("whois identity lacks a stable node identifier"),
            Self::TooManyPeers => f.write_str("Tailscale peer count exceeds the configured bound"),
            Self::InvalidDiscoveryConfig => {
                f.write_str("peer discovery settings exceed their bounds")
            }
            Self::DiscoveryWorkerFailed => f.write_str("a peer discovery worker failed"),
            Self::Allowlist(message) => write!(f, "allowlist operation failed: {message}"),
        }
    }
}

impl std::error::Error for IdentityError {}
