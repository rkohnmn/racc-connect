use crate::error::IdentityError;
use crate::model::{ConnectionPath, HostCapability, Peer, PeerIdentity, SelfNode, StatusSnapshot};
use serde_json::Value;
use std::net::IpAddr;

const MAX_PEERS: usize = 4096;
const MAX_ADDRESSES: usize = 16;
const MAX_TAGS: usize = 64;
const MAX_FIELD_BYTES: usize = 512;
const MAX_TAG_BYTES: usize = 128;

/// Parse bounded, anonymized-compatible output from Tailscale status.
pub fn parse_status_json(bytes: &[u8]) -> Result<StatusSnapshot, IdentityError> {
    let root: Value = serde_json::from_slice(bytes).map_err(|_| IdentityError::BadJson)?;
    let object = root.as_object().ok_or(IdentityError::BadJson)?;
    let backend_state = get(object, "BackendState").and_then(bounded_string);
    if let Some(state) = backend_state.as_deref() {
        if state.eq_ignore_ascii_case("NeedsLogin")
            || state.eq_ignore_ascii_case("LoggedOut")
            || state.eq_ignore_ascii_case("NeedsMachineAuth")
        {
            return Err(IdentityError::NotLoggedIn);
        }
        if state.eq_ignore_ascii_case("Stopped") {
            return Err(IdentityError::NotRunning);
        }
    }

    let users = get(object, "User").and_then(Value::as_object);
    let self_node = get(object, "Self").and_then(Value::as_object).map(|node| {
        let addresses = addresses_from(node, &["TailscaleIPs", "Addresses"]);
        let online = bool_field(node, "Online").unwrap_or_else(|| {
            backend_state
                .as_deref()
                .is_some_and(|state| state.eq_ignore_ascii_case("Running"))
        });
        SelfNode {
            display_name: first_string(node, &["HostName", "Name"]),
            dns_name: first_string(node, &["DNSName", "DnsName"]),
            ipv4: addresses.iter().copied().find(IpAddr::is_ipv4),
            ipv6: addresses.iter().copied().find(IpAddr::is_ipv6),
            online,
        }
    });

    let peer_map = get(object, "Peer").and_then(Value::as_object);
    if peer_map.is_some_and(|peers| peers.len() > MAX_PEERS) {
        return Err(IdentityError::TooManyPeers);
    }
    let mut peers = Vec::new();
    if let Some(peer_map) = peer_map {
        for value in peer_map.values() {
            let Some(peer) = value.as_object() else {
                continue;
            };
            let user_id = scalar_string(get(peer, "UserID"));
            let owner_login = user_id
                .as_deref()
                .and_then(|id| users.and_then(|map| get(map, id)))
                .and_then(|user| user.as_object())
                .and_then(|user| first_string(user, &["LoginName", "Name"]));
            let online = bool_field(peer, "Online").unwrap_or(false);
            let path = if online {
                let current_address = first_string(peer, &["CurAddr", "CurrentAddress"]);
                let relay = first_string(peer, &["Relay", "DERP"]);
                match (
                    current_address.filter(|value| !value.is_empty()),
                    relay.filter(|value| !value.is_empty()),
                ) {
                    (Some(_), _) => ConnectionPath::Direct,
                    (None, Some(region)) => ConnectionPath::Derp {
                        region: Some(region),
                    },
                    _ => ConnectionPath::Unknown,
                }
            } else {
                ConnectionPath::Unknown
            };
            peers.push(Peer {
                node_id: first_scalar(peer, &["ID", "NodeID", "StableID"]),
                host_name: first_string(peer, &["HostName", "Name"]),
                dns_name: first_string(peer, &["DNSName", "DnsName"]),
                addresses: addresses_from(peer, &["TailscaleIPs", "Addresses"]),
                os: first_string(peer, &["OS", "Os"]),
                online,
                path,
                tags: string_array(peer, "Tags", MAX_TAGS, MAX_TAG_BYTES),
                owner_login,
                host_capability: HostCapability::Unknown,
            });
        }
    }
    peers.sort_by(|left, right| {
        left.node_id
            .cmp(&right.node_id)
            .then_with(|| left.host_name.cmp(&right.host_name))
    });

    Ok(StatusSnapshot {
        self_node,
        peers,
        backend_state,
        version: get(object, "Version").and_then(bounded_string),
    })
}

/// Parse one machine identity from Tailscale whois JSON and reject ambiguous results.
pub fn parse_whois_json(bytes: &[u8]) -> Result<PeerIdentity, IdentityError> {
    let root: Value = serde_json::from_slice(bytes).map_err(|_| IdentityError::BadJson)?;
    let object = root.as_object().ok_or(IdentityError::BadJson)?;
    if let Some(matches) = get(object, "Machines").and_then(Value::as_array) {
        if matches.len() > 1 {
            return Err(IdentityError::AmbiguousWhois);
        }
    }
    if let Some(matches) = get(object, "Matches").and_then(Value::as_array) {
        if matches.len() > 1 {
            return Err(IdentityError::AmbiguousWhois);
        }
    }
    let node_value = get(object, "Node")
        .or_else(|| get(object, "Machine"))
        .ok_or(IdentityError::BadJson)?;
    let node = match node_value {
        Value::Array(values) if values.len() > 1 => return Err(IdentityError::AmbiguousWhois),
        Value::Array(values) => values
            .first()
            .and_then(Value::as_object)
            .ok_or(IdentityError::BadJson)?,
        Value::Object(node) => node,
        _ => return Err(IdentityError::BadJson),
    };
    let node_id = first_non_empty_scalar(node, &["StableID", "NodeID", "ID"])
        .ok_or(IdentityError::InvalidIdentity)?;
    let owner_login = get(object, "UserProfile")
        .and_then(Value::as_object)
        .and_then(|user| first_string(user, &["LoginName"]))
        .or_else(|| {
            get(object, "User")
                .and_then(Value::as_object)
                .and_then(|user| first_string(user, &["LoginName", "Name"]))
        });
    let hostinfo = get(node, "Hostinfo").and_then(Value::as_object);
    let tags = hostinfo
        .and_then(|info| {
            get(info, "Tags").map(|_| string_array(info, "Tags", MAX_TAGS, MAX_TAG_BYTES))
        })
        .unwrap_or_else(|| string_array(node, "Tags", MAX_TAGS, MAX_TAG_BYTES));
    Ok(PeerIdentity {
        node_id,
        owner_login,
        tags,
        node_name: hostinfo
            .and_then(|info| first_string(info, &["Hostname"]))
            .or_else(|| first_string(node, &["Name", "HostName"])),
        addresses: addresses_from(node, &["Addresses", "TailscaleIPs"]),
    })
}

fn get<'a>(object: &'a serde_json::Map<String, Value>, name: &str) -> Option<&'a Value> {
    object.get(name).or_else(|| {
        object
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    })
}

fn bounded_string(value: &Value) -> Option<String> {
    value.as_str().map(|value| bounded(value, MAX_FIELD_BYTES))
}

fn first_string(object: &serde_json::Map<String, Value>, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| get(object, name).and_then(bounded_string))
}

fn first_scalar(object: &serde_json::Map<String, Value>, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| get(object, name).and_then(|value| scalar_string(Some(value))))
        .map(|value| bounded(&value, MAX_FIELD_BYTES))
}

fn first_non_empty_scalar(
    object: &serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<String> {
    names
        .iter()
        .find_map(|name| first_scalar(object, &[*name]).filter(|value| !value.is_empty()))
}

fn scalar_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn bool_field(object: &serde_json::Map<String, Value>, name: &str) -> Option<bool> {
    get(object, name).and_then(Value::as_bool)
}

fn string_array(
    object: &serde_json::Map<String, Value>,
    name: &str,
    max_count: usize,
    max_bytes: usize,
) -> Vec<String> {
    get(object, name)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(max_count)
        .filter_map(Value::as_str)
        .map(|value| bounded(value, max_bytes))
        .collect()
}

fn addresses_from(object: &serde_json::Map<String, Value>, names: &[&str]) -> Vec<IpAddr> {
    let mut addresses = Vec::new();
    for name in names {
        let Some(values) = get(object, name).and_then(Value::as_array) else {
            continue;
        };
        for value in values
            .iter()
            .take(MAX_ADDRESSES.saturating_sub(addresses.len()))
        {
            let Some(text) = value.as_str() else {
                continue;
            };
            let address_text = text.split('/').next().unwrap_or_default();
            if let Ok(address) = address_text.parse::<IpAddr>() {
                if !addresses.contains(&address) {
                    addresses.push(address);
                }
            }
            if addresses.len() == MAX_ADDRESSES {
                break;
            }
        }
        if addresses.len() == MAX_ADDRESSES {
            break;
        }
    }
    addresses
}

fn bounded(value: &str, maximum: usize) -> String {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_owned()
}
