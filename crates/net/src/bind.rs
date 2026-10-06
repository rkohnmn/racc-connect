use std::fmt;
use std::net::{IpAddr, Ipv4Addr};

/// Address policy for transport sockets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindPolicy {
    /// Accept only Tailscale IPv4 CGNAT and Tailscale IPv6 addresses.
    Tailscale,
    /// Accept loopback addresses for test harnesses; only built with the test-bind feature.
    #[cfg(feature = "test-bind")]
    TestOnlyLoopback,
}

/// Distinct reason why an address is not allowed by the production bind policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindError {
    /// Unspecified wildcard address.
    Unspecified,
    /// Loopback address.
    Loopback,
    /// Link-local address.
    LinkLocal,
    /// Private LAN or unique-local address outside the Tailscale ranges.
    PrivateLan,
    /// Public, multicast, reserved, or otherwise non-Tailscale address.
    OutsideTailscale,
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unspecified => "unspecified addresses are forbidden",
            Self::Loopback => "loopback is forbidden by the production policy",
            Self::LinkLocal => "link-local addresses are forbidden",
            Self::PrivateLan => "private LAN addresses are forbidden",
            Self::OutsideTailscale => "address is outside the Tailscale ranges",
        })
    }
}

impl std::error::Error for BindError {}

/// Validates an IP address under the selected policy without performing I/O.
pub fn validate_bind_addr(addr: IpAddr, policy: BindPolicy) -> Result<(), BindError> {
    let _ = policy;
    if addr.is_unspecified() {
        return Err(BindError::Unspecified);
    }
    if addr.is_loopback() {
        #[cfg(feature = "test-bind")]
        if policy == BindPolicy::TestOnlyLoopback {
            return Ok(());
        }
        return Err(BindError::Loopback);
    }
    if is_tailscale(addr) {
        return Ok(());
    }
    if is_link_local(addr) {
        return Err(BindError::LinkLocal);
    }
    if is_private_lan(addr) {
        return Err(BindError::PrivateLan);
    }
    Err(BindError::OutsideTailscale)
}

fn is_tailscale(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => in_v4_prefix(v4, Ipv4Addr::new(100, 64, 0, 0), 10),
        IpAddr::V6(v6) => v6.octets()[..6] == [0xfd, 0x7a, 0x11, 0x5c, 0xa1, 0xe0],
    }
}

fn in_v4_prefix(addr: Ipv4Addr, network: Ipv4Addr, prefix_len: u8) -> bool {
    let mask = u32::MAX << (32 - u32::from(prefix_len));
    (u32::from(addr) & mask) == (u32::from(network) & mask)
}

fn is_link_local(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => v4.octets()[0] == 169 && v4.octets()[1] == 254,
        IpAddr::V6(v6) => v6.segments()[0] & 0xffc0 == 0xfe80,
    }
}

fn is_private_lan(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168)
        }
        IpAddr::V6(v6) => v6.segments()[0] & 0xfe00 == 0xfc00,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_address_table_has_distinct_rejections_and_prefix_boundaries() {
        let cases = [
            ("0.0.0.0", Err(BindError::Unspecified)),
            ("::", Err(BindError::Unspecified)),
            ("127.0.0.1", Err(BindError::Loopback)),
            ("::1", Err(BindError::Loopback)),
            ("169.254.1.1", Err(BindError::LinkLocal)),
            ("fe80::1", Err(BindError::LinkLocal)),
            ("10.0.0.1", Err(BindError::PrivateLan)),
            ("172.16.0.1", Err(BindError::PrivateLan)),
            ("172.31.255.255", Err(BindError::PrivateLan)),
            ("192.168.1.1", Err(BindError::PrivateLan)),
            ("fc00::1", Err(BindError::PrivateLan)),
            ("100.63.255.255", Err(BindError::OutsideTailscale)),
            ("100.64.0.0", Ok(())),
            ("100.127.255.255", Ok(())),
            ("100.128.0.0", Err(BindError::OutsideTailscale)),
            ("fd7a:115c:a1e0::1", Ok(())),
            ("fd7a:115c:a1e1::1", Err(BindError::PrivateLan)),
            ("8.8.8.8", Err(BindError::OutsideTailscale)),
            ("ff02::1", Err(BindError::OutsideTailscale)),
        ];
        for (text, expected) in cases {
            let addr = text.parse::<IpAddr>();
            assert!(addr.is_ok(), "test address must parse: {text}");
            if let Ok(addr) = addr {
                assert_eq!(
                    validate_bind_addr(addr, BindPolicy::Tailscale),
                    expected,
                    "{text}"
                );
            }
        }
    }

    #[cfg(feature = "test-bind")]
    #[test]
    fn test_policy_allows_loopback_but_production_does_not() {
        let addr = "127.0.0.1".parse::<IpAddr>();
        assert!(addr.is_ok());
        if let Ok(addr) = addr {
            assert_eq!(
                validate_bind_addr(addr, BindPolicy::TestOnlyLoopback),
                Ok(())
            );
            assert_eq!(
                validate_bind_addr(addr, BindPolicy::Tailscale),
                Err(BindError::Loopback)
            );
        }
    }
}
