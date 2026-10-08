//! Bounded EDID lookup for stable Windows monitor identity.
//!
//! The monitor serial is used only as input to `racc_topology::stable_display_id`; it is never
//! returned from this module, formatted, or logged. Registry access is read-only and confined to
//! the monitor interface path supplied by DisplayConfig.

use racc_topology::{stable_display_id, DisplayId, DisplayIdentity};

const EDID_BASE_BLOCK_LEN: usize = 128;
const MAX_MONITOR_PATH_BYTES: usize = 1024;
const MAX_REGISTRY_EDID_BYTES: usize = 32 * 1024;
const EDID_HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];
const DISPLAY_INTERFACE_PREFIX: &str = r"\\?\DISPLAY#";

/// Stable monitor identity without exposing any EDID fields, including the serial.
pub(super) struct MonitorIdentity {
    /// Hashed stable display identifier.
    pub(super) display_id: DisplayId,
    /// True when a valid EDID base block contributed to the identifier.
    pub(super) used_edid: bool,
}

struct EdidBaseBlock {
    manufacturer: [u8; 3],
    product_code: u16,
    serial: u32,
}

struct MonitorRegistryPath {
    hardware_id: String,
    instance_id: String,
}

/// Builds a stable identity from a monitor device interface path.
///
/// If the path is not a valid monitor interface path, or the registry EDID is unavailable or
/// invalid, the connector path alone is used. `None` means the supplied connector cannot be
/// represented by the topology identity format.
pub(super) fn identity_from_monitor_path(monitor_path: &str) -> Option<MonitorIdentity> {
    if monitor_path.is_empty() {
        return None;
    }
    identity_from_parts(monitor_path, read_monitor_edid(monitor_path))
}

fn identity_from_parts(connector: &str, edid: Option<EdidBaseBlock>) -> Option<MonitorIdentity> {
    if connector.is_empty() {
        return None;
    }
    let (manufacturer, product_code, serial, used_edid) = match edid {
        Some(edid) => (edid.manufacturer, edid.product_code, edid.serial, true),
        None => (*b"UNK", 0, 0, false),
    };
    let identity = DisplayIdentity::new(manufacturer, product_code, serial, connector, None);
    let display_id = stable_display_id(&identity).ok()?;
    Some(MonitorIdentity {
        display_id,
        used_edid,
    })
}

fn parse_edid_base_block(bytes: &[u8]) -> Option<EdidBaseBlock> {
    let block = bytes.get(..EDID_BASE_BLOCK_LEN)?;
    if block.get(..EDID_HEADER.len())? != EDID_HEADER
        || block.iter().copied().fold(0u8, u8::wrapping_add) != 0
    {
        return None;
    }

    let manufacturer_code = u16::from_be_bytes([*block.get(8)?, *block.get(9)?]);
    let manufacturer = [
        decode_manufacturer_letter((manufacturer_code >> 10) & 0x1f)?,
        decode_manufacturer_letter((manufacturer_code >> 5) & 0x1f)?,
        decode_manufacturer_letter(manufacturer_code & 0x1f)?,
    ];
    let product_code = u16::from_le_bytes([*block.get(10)?, *block.get(11)?]);
    let serial = u32::from_le_bytes([
        *block.get(12)?,
        *block.get(13)?,
        *block.get(14)?,
        *block.get(15)?,
    ]);
    Some(EdidBaseBlock {
        manufacturer,
        product_code,
        serial,
    })
}

fn decode_manufacturer_letter(value: u16) -> Option<u8> {
    if (1..=26).contains(&value) {
        Some(b'A' + (value as u8 - 1))
    } else {
        None
    }
}

fn bounded_registry_edid_length(value_len: u32) -> Option<usize> {
    let value_len = usize::try_from(value_len).ok()?;
    (EDID_BASE_BLOCK_LEN..=MAX_REGISTRY_EDID_BYTES)
        .contains(&value_len)
        .then_some(value_len)
}

fn parse_monitor_interface_path(path: &str) -> Option<MonitorRegistryPath> {
    if path.len() > MAX_MONITOR_PATH_BYTES || !path.is_ascii() {
        return None;
    }
    let prefix = path.get(..DISPLAY_INTERFACE_PREFIX.len())?;
    if !prefix.eq_ignore_ascii_case(DISPLAY_INTERFACE_PREFIX) {
        return None;
    }
    let remainder = path.get(DISPLAY_INTERFACE_PREFIX.len()..)?;
    let (hardware_id, remainder) = remainder.split_once('#')?;
    let (instance_id, interface_guid) = remainder.split_once('#')?;
    if interface_guid.contains('#')
        || !valid_registry_component(hardware_id)
        || !valid_registry_component(instance_id)
        || !valid_interface_guid(interface_guid)
    {
        return None;
    }
    Some(MonitorRegistryPath {
        hardware_id: hardware_id.to_owned(),
        instance_id: instance_id.to_owned(),
    })
}

fn valid_registry_component(component: &str) -> bool {
    !component.is_empty()
        && component.len() <= 256
        && component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'&' | b'_' | b'-'))
}

fn valid_interface_guid(value: &str) -> bool {
    let Some(guid) = value
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
    else {
        return false;
    };
    guid.len() == 36
        && guid.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

#[cfg(windows)]
fn read_monitor_edid(monitor_path: &str) -> Option<EdidBaseBlock> {
    let parsed_path = parse_monitor_interface_path(monitor_path)?;
    let key_path = format!(
        "SYSTEM\\CurrentControlSet\\Enum\\DISPLAY\\{}\\{}\\Device Parameters",
        parsed_path.hardware_id, parsed_path.instance_id
    );
    let key = RegistryKey::open_read_only(&key_path)?;
    parse_edid_base_block(&key.read_binary_value("EDID")?)
}

#[cfg(not(windows))]
fn read_monitor_edid(_monitor_path: &str) -> Option<EdidBaseBlock> {
    None
}

#[cfg(windows)]
type RegistryKeyHandle = *mut core::ffi::c_void;

#[cfg(windows)]
const ERROR_SUCCESS: i32 = 0;
#[cfg(windows)]
const REG_BINARY: u32 = 3;
#[cfg(windows)]
const KEY_QUERY_VALUE: u32 = 0x0001;

#[cfg(windows)]
#[link(name = "Advapi32")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        key: RegistryKeyHandle,
        sub_key: *const u16,
        options: u32,
        desired_access: u32,
        result: *mut RegistryKeyHandle,
    ) -> i32;
    fn RegQueryValueExW(
        key: RegistryKeyHandle,
        value_name: *const u16,
        reserved: *const u32,
        value_type: *mut u32,
        data: *mut u8,
        data_len: *mut u32,
    ) -> i32;
    fn RegCloseKey(key: RegistryKeyHandle) -> i32;
}

#[cfg(windows)]
struct RegistryKey(RegistryKeyHandle);

#[cfg(windows)]
impl RegistryKey {
    fn open_read_only(sub_key: &str) -> Option<Self> {
        let sub_key: Vec<u16> = sub_key.encode_utf16().chain(Some(0)).collect();
        let mut result = core::ptr::null_mut();
        // SAFETY: `sub_key` is bounded and contains only validated monitor path components.
        // `result` is writable local storage. Access is query-only and the successful handle is
        // uniquely owned by `RegistryKey`.
        let status = unsafe {
            RegOpenKeyExW(
                (-2_147_483_646isize) as RegistryKeyHandle,
                sub_key.as_ptr(),
                0,
                KEY_QUERY_VALUE,
                &mut result,
            )
        };
        (status == ERROR_SUCCESS && !result.is_null()).then_some(Self(result))
    }

    fn read_binary_value(&self, value_name: &str) -> Option<Vec<u8>> {
        let value_name: Vec<u16> = value_name.encode_utf16().chain(Some(0)).collect();
        let mut value_type = 0u32;
        let mut value_len = 0u32;
        // SAFETY: The key is live and RAII-owned; the NUL-terminated value name and output
        // pointers refer to initialized local storage. A null data pointer requests only size.
        let query_status = unsafe {
            RegQueryValueExW(
                self.0,
                value_name.as_ptr(),
                core::ptr::null(),
                &mut value_type,
                core::ptr::null_mut(),
                &mut value_len,
            )
        };
        let value_len_usize = bounded_registry_edid_length(value_len)?;
        if query_status != ERROR_SUCCESS || value_type != REG_BINARY {
            return None;
        }

        let mut bytes = vec![0u8; value_len_usize];
        let mut returned_type = 0u32;
        let mut returned_len = u32::try_from(bytes.len()).ok()?;
        // SAFETY: Allocation follows a 32 KiB size cap. `bytes` has exactly the writable capacity
        // declared to the registry, and metadata outputs are initialized locals. If the registry
        // value changed or grew, the call fails and the caller falls back to connector identity.
        let read_status = unsafe {
            RegQueryValueExW(
                self.0,
                value_name.as_ptr(),
                core::ptr::null(),
                &mut returned_type,
                bytes.as_mut_ptr(),
                &mut returned_len,
            )
        };
        let returned_len = usize::try_from(returned_len).ok()?;
        if read_status != ERROR_SUCCESS
            || returned_type != REG_BINARY
            || returned_len < EDID_BASE_BLOCK_LEN
            || returned_len > bytes.len()
        {
            return None;
        }
        bytes.truncate(returned_len);
        Some(bytes)
    }
}

#[cfg(windows)]
impl Drop for RegistryKey {
    fn drop(&mut self) {
        // SAFETY: This uniquely owned handle comes from a successful RegOpenKeyExW call; Drop
        // closes it once and no method transfers or closes the handle early.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_edid_block() -> [u8; EDID_BASE_BLOCK_LEN] {
        let mut block = [0u8; EDID_BASE_BLOCK_LEN];
        block[..8].copy_from_slice(&EDID_HEADER);
        block[8..10].copy_from_slice(&0x10acu16.to_be_bytes()); // DEL
        block[10..12].copy_from_slice(&0x1234u16.to_le_bytes());
        block[12..16].copy_from_slice(&0x7856_3412u32.to_le_bytes());
        block[127] = block[..127]
            .iter()
            .copied()
            .fold(0u8, u8::wrapping_add)
            .wrapping_neg();
        block
    }

    #[test]
    fn registry_edid_length_is_bounded_before_allocation() {
        assert!(bounded_registry_edid_length(0).is_none());
        assert!(bounded_registry_edid_length((EDID_BASE_BLOCK_LEN - 1) as u32).is_none());
        assert!(bounded_registry_edid_length(EDID_BASE_BLOCK_LEN as u32).is_some());
        assert!(bounded_registry_edid_length(MAX_REGISTRY_EDID_BYTES as u32).is_some());
        assert!(bounded_registry_edid_length((MAX_REGISTRY_EDID_BYTES + 1) as u32).is_none());
        assert!(bounded_registry_edid_length(u32::MAX).is_none());
    }

    #[test]
    fn parses_monitor_interface_path_into_registry_components() {
        let path =
            r"\\?\DISPLAY#DEL40A9#5&1234567&0&UID4356#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}";
        let parsed = parse_monitor_interface_path(path);
        assert!(parsed.is_some());
        let Some(parsed) = parsed else { return };
        assert_eq!(parsed.hardware_id, "DEL40A9");
        assert_eq!(parsed.instance_id, "5&1234567&0&UID4356");
    }

    #[test]
    fn rejects_malformed_or_unsafe_monitor_paths() {
        assert!(parse_monitor_interface_path(
            r"\\?\DISPLAY#..#instance#{00000000-0000-0000-0000-000000000000}"
        )
        .is_none());
        assert!(parse_monitor_interface_path(
            r"\\?\OTHER#DEL40A9#instance#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}"
        )
        .is_none());
        assert!(parse_monitor_interface_path(r"\\?\DISPLAY#DEL40A9#instance#not-a-guid").is_none());
        assert!(parse_monitor_interface_path(&"x".repeat(MAX_MONITOR_PATH_BYTES + 1)).is_none());
    }

    #[test]
    fn validates_edid_header_checksum_and_decodes_identity_fields() {
        let block = valid_edid_block();
        let parsed = parse_edid_base_block(&block);
        assert!(parsed.is_some());
        let Some(parsed) = parsed else { return };
        assert!(parsed.manufacturer == *b"DEL");
        assert!(parsed.product_code == 0x1234);
        assert!(parsed.serial == 0x7856_3412);
    }

    #[test]
    fn rejects_short_bad_header_bad_checksum_and_invalid_manufacturer_blocks() {
        let valid = valid_edid_block();
        assert!(parse_edid_base_block(&valid[..EDID_BASE_BLOCK_LEN - 1]).is_none());

        let mut bad_header = valid;
        bad_header[0] = 0x01;
        assert!(parse_edid_base_block(&bad_header).is_none());

        let mut bad_checksum = valid;
        bad_checksum[127] = bad_checksum[127].wrapping_add(1);
        assert!(parse_edid_base_block(&bad_checksum).is_none());

        let mut bad_manufacturer = valid;
        bad_manufacturer[8..10].copy_from_slice(&0u16.to_be_bytes());
        bad_manufacturer[127] = bad_manufacturer[..127]
            .iter()
            .copied()
            .fold(0u8, u8::wrapping_add)
            .wrapping_neg();
        assert!(parse_edid_base_block(&bad_manufacturer).is_none());
    }

    #[test]
    fn connector_fallback_and_edid_serial_only_affect_the_hashed_id() {
        let connector =
            r"\\?\DISPLAY#DEL40A9#5&1234567&0&UID4356#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}";
        let fallback = identity_from_parts(connector, None);
        assert!(fallback.is_some());
        let Some(fallback) = fallback else { return };
        assert!(!fallback.used_edid);

        let block = valid_edid_block();
        let edid = parse_edid_base_block(&block);
        assert!(edid.is_some());
        let Some(edid) = edid else { return };
        let with_edid = identity_from_parts(connector, Some(edid));
        assert!(with_edid.is_some());
        let Some(with_edid) = with_edid else { return };
        assert!(with_edid.used_edid);
        assert!(with_edid.display_id != fallback.display_id);

        let mut other_serial = block;
        other_serial[12] ^= 0x01;
        other_serial[127] = other_serial[..127]
            .iter()
            .copied()
            .fold(0u8, u8::wrapping_add)
            .wrapping_neg();
        let other_edid = parse_edid_base_block(&other_serial);
        assert!(other_edid.is_some());
        let Some(other_edid) = other_edid else { return };
        let other_identity = identity_from_parts(connector, Some(other_edid));
        assert!(other_identity.is_some());
        let Some(other_identity) = other_identity else {
            return;
        };
        assert!(other_identity.display_id != with_edid.display_id);
    }
}
