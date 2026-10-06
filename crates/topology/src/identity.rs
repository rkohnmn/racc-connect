use core::fmt;

use crate::DisplayId;
use racc_proto::MAX_DISPLAYS;

/// Stable identity ingredients obtained from host display metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayIdentity {
    /// Three ASCII EDID manufacturer letters.
    pub manufacturer: [u8; 3],
    /// EDID product code.
    pub product_code: u16,
    /// EDID serial; zero represents an absent serial.
    pub serial: u32,
    /// Connector or host instance name.
    pub connector: String,
    /// Optional deterministic disambiguator supplied by the host.
    pub ordinal: Option<u32>,
}

impl DisplayIdentity {
    /// Creates a display identity from host-provided metadata.
    pub fn new(
        manufacturer: [u8; 3],
        product_code: u16,
        serial: u32,
        connector: impl Into<String>,
        ordinal: Option<u32>,
    ) -> Self {
        Self {
            manufacturer,
            product_code,
            serial,
            connector: connector.into(),
            ordinal,
        }
    }
}

/// Errors validating stable display identity inputs or collision sets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityError {
    /// Manufacturer bytes must be three ASCII letters.
    InvalidManufacturer,
    /// A connector or instance name must not be empty.
    EmptyConnector,
    /// A connector name cannot be represented by the canonical u32 byte length.
    ConnectorTooLong,
    /// More identities were supplied than a topology can contain.
    TooManyIdentities,
    /// Two entries had identical canonical identity ingredients.
    DuplicateIdentity,
    /// Bounded collision probing did not produce a unique id.
    CollisionExhausted,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidManufacturer => "EDID manufacturer must be three ASCII letters",
            Self::EmptyConnector => "display connector name is empty",
            Self::ConnectorTooLong => "connector name exceeds canonical encoding length",
            Self::TooManyIdentities => "identity list exceeds the topology display limit",
            Self::DuplicateIdentity => "duplicate canonical display identity",
            Self::CollisionExhausted => "display id collision probe limit exhausted",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for IdentityError {}

/// Computes the base stable id using FNV-1a 32-bit over the documented v1 encoding.
///
/// This hash is for identity stability only; it is not a security primitive.
pub fn stable_display_id(identity: &DisplayIdentity) -> Result<DisplayId, IdentityError> {
    let hash = canonical_hash(identity, None)?;
    Ok(nonzero_id(hash))
}

/// Assigns deterministic unique ids for all identities in one topology.
///
/// Identities are sorted by their canonical ingredients before collision probing,
/// so results do not depend on input order. At most MAX_DISPLAYS entries are accepted.
pub fn assign_display_ids(identities: &[DisplayIdentity]) -> Result<Vec<DisplayId>, IdentityError> {
    assign_display_ids_with(identities, canonical_hash_for_assignment)
}

fn assign_display_ids_with(
    identities: &[DisplayIdentity],
    mut hash_for: impl FnMut(&DisplayIdentity, Option<u32>) -> Result<u32, IdentityError>,
) -> Result<Vec<DisplayId>, IdentityError> {
    if identities.len() > MAX_DISPLAYS {
        return Err(IdentityError::TooManyIdentities);
    }
    for identity in identities {
        validate_identity(identity)?;
    }

    let mut order: Vec<usize> = (0..identities.len()).collect();
    order.sort_by(|left, right| compare_identity(&identities[*left], &identities[*right]));
    for pair in order.windows(2) {
        if let (Some(left_index), Some(right_index)) = (pair.first(), pair.get(1)) {
            if compare_identity(&identities[*left_index], &identities[*right_index])
                == core::cmp::Ordering::Equal
            {
                return Err(IdentityError::DuplicateIdentity);
            }
        }
    }

    let mut output = vec![None; identities.len()];
    let mut used = Vec::with_capacity(identities.len());
    for index in order {
        let identity = &identities[index];
        let mut id = nonzero_id(hash_for(identity, None)?);
        if used.contains(&id) {
            let mut replacement = None;
            for attempt in 1..=MAX_DISPLAYS as u32 {
                let candidate = nonzero_id(hash_for(identity, Some(attempt))?);
                if !used.contains(&candidate) {
                    replacement = Some(candidate);
                    break;
                }
            }
            id = replacement.ok_or(IdentityError::CollisionExhausted)?;
        }
        used.push(id);
        if let Some(slot) = output.get_mut(index) {
            *slot = Some(id);
        }
    }

    output
        .into_iter()
        .map(|value| value.ok_or(IdentityError::CollisionExhausted))
        .collect()
}

fn canonical_hash_for_assignment(
    identity: &DisplayIdentity,
    probe: Option<u32>,
) -> Result<u32, IdentityError> {
    canonical_hash(identity, probe)
}

fn canonical_hash(identity: &DisplayIdentity, probe: Option<u32>) -> Result<u32, IdentityError> {
    validate_identity(identity)?;
    let connector_len =
        u32::try_from(identity.connector.len()).map_err(|_| IdentityError::ConnectorTooLong)?;

    let mut hash = FNV_OFFSET_BASIS;
    feed(&mut hash, b"racc-display-id-v1\0");
    for byte in identity.manufacturer {
        feed(&mut hash, &[byte.to_ascii_uppercase()]);
    }
    feed(&mut hash, &identity.product_code.to_le_bytes());
    if identity.serial == 0 {
        feed(&mut hash, &[0]);
    } else {
        feed(&mut hash, &[1]);
        feed(&mut hash, &identity.serial.to_le_bytes());
    }
    feed(&mut hash, &connector_len.to_le_bytes());
    feed(&mut hash, identity.connector.as_bytes());
    match identity.ordinal {
        Some(ordinal) => {
            feed(&mut hash, &[1]);
            feed(&mut hash, &ordinal.to_le_bytes());
        }
        None => feed(&mut hash, &[0]),
    }
    if let Some(probe) = probe {
        feed(&mut hash, &[0xff]);
        feed(&mut hash, &probe.to_le_bytes());
    }
    Ok(hash)
}

fn validate_identity(identity: &DisplayIdentity) -> Result<(), IdentityError> {
    if !identity.manufacturer.iter().all(u8::is_ascii_alphabetic) {
        return Err(IdentityError::InvalidManufacturer);
    }
    if identity.connector.is_empty() {
        return Err(IdentityError::EmptyConnector);
    }
    u32::try_from(identity.connector.len()).map_err(|_| IdentityError::ConnectorTooLong)?;
    Ok(())
}

fn compare_identity(left: &DisplayIdentity, right: &DisplayIdentity) -> core::cmp::Ordering {
    let left_manufacturer = left.manufacturer.map(|byte| byte.to_ascii_uppercase());
    let right_manufacturer = right.manufacturer.map(|byte| byte.to_ascii_uppercase());
    left_manufacturer
        .cmp(&right_manufacturer)
        .then_with(|| left.product_code.cmp(&right.product_code))
        .then_with(|| left.serial.cmp(&right.serial))
        .then_with(|| left.connector.as_bytes().cmp(right.connector.as_bytes()))
        .then_with(|| left.ordinal.cmp(&right.ordinal))
}

fn nonzero_id(hash: u32) -> DisplayId {
    DisplayId::from_hash(hash)
}

fn feed(hash: &mut u32, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u32::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

const FNV_OFFSET_BASIS: u32 = 0x811c_9dc5;
const FNV_PRIME: u32 = 0x0100_0193;

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(
        manufacturer: [u8; 3],
        product_code: u16,
        serial: u32,
        connector: &str,
        ordinal: Option<u32>,
    ) -> DisplayIdentity {
        DisplayIdentity::new(manufacturer, product_code, serial, connector, ordinal)
    }

    #[test]
    fn stable_id_golden_vectors() {
        let vectors = [
            (
                identity(*b"DEL", 0x1234, 0, "DISPLAY1", Some(0)),
                0x69cb_2786,
            ),
            (
                identity(*b"ACM", 0x00ab, 0x0102_0304, "DP-2", None),
                0xa4cc_8445,
            ),
        ];
        for (value, expected) in vectors {
            assert_eq!(
                stable_display_id(&value).expect("valid identity").get(),
                expected
            );
        }
    }

    #[test]
    fn canonical_identity_and_collision_assignment_are_order_independent() {
        let left = identity(*b"DEL", 1, 10, "DP-1", None);
        let right = identity(*b"DEL", 1, 11, "DP-1", None);
        let forward =
            assign_display_ids(&[left.clone(), right.clone()]).expect("distinct identities");
        let reverse =
            assign_display_ids(&[right.clone(), left.clone()]).expect("distinct identities");
        assert_eq!(forward[0], reverse[1]);
        assert_eq!(forward[1], reverse[0]);
        assert_ne!(forward[0], forward[1]);

        let forced = |_: &DisplayIdentity, probe: Option<u32>| {
            Ok(match probe {
                None => 7,
                Some(value) => 100 + value,
            })
        };
        let forced_forward =
            assign_display_ids_with(&[left.clone(), right.clone()], forced).expect("probe");
        let forced_reverse = assign_display_ids_with(&[right, left], forced).expect("probe");
        assert_eq!(forced_forward[0], forced_reverse[1]);
        assert_eq!(forced_forward[1], forced_reverse[0]);
        assert_ne!(forced_forward[0], forced_forward[1]);
    }

    #[test]
    fn identity_validation_is_typed() {
        assert_eq!(
            stable_display_id(&identity(*b"D1!", 1, 1, "DP-1", None)),
            Err(IdentityError::InvalidManufacturer)
        );
        assert_eq!(
            stable_display_id(&identity(*b"DEL", 1, 1, "", None)),
            Err(IdentityError::EmptyConnector)
        );
        assert_eq!(
            assign_display_ids(&[
                identity(*b"DEL", 1, 1, "DP-1", None),
                identity(*b"DEL", 1, 1, "DP-1", None)
            ]),
            Err(IdentityError::DuplicateIdentity)
        );
    }
}
