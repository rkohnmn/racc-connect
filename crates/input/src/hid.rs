//! USB HID keyboard-page usage to Windows key identity mapping.

/// A Windows keyboard scan code, with a virtual-key fallback for Pause/Break, whose
/// Windows make sequence is not a single Set-1 scan code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanCode {
    /// Scan byte. Zero only when `virtual_key` is set.
    pub code: u16,
    /// Whether the scan code uses the E0 extended-key prefix.
    pub extended: bool,
    /// Windows virtual-key fallback for special keys without a single scan code.
    pub virtual_key: Option<u16>,
}

impl ScanCode {
    const fn scan(code: u16) -> Self {
        Self {
            code,
            extended: false,
            virtual_key: None,
        }
    }

    const fn extended(code: u16) -> Self {
        Self {
            code,
            extended: true,
            virtual_key: None,
        }
    }

    const fn virtual_key(code: u16) -> Self {
        Self {
            code: 0,
            extended: false,
            virtual_key: Some(code),
        }
    }
}

/// Table of supported USB HID keyboard-page usages and Windows key identities.
///
/// The layout-independent alphanumeric and modifier mappings cover a standard 104-key
/// keyboard, navigation/keypad keys, F1-F24, and the keyboard-page volume controls. Consumer
/// Control page media usages are not representable in the current protocol's page-7 field.
pub const SUPPORTED_HID_USAGES: &[(u16, ScanCode)] = &[
    (0x04, ScanCode::scan(0x1e)),        // A
    (0x05, ScanCode::scan(0x30)),        // B
    (0x06, ScanCode::scan(0x2e)),        // C
    (0x07, ScanCode::scan(0x20)),        // D
    (0x08, ScanCode::scan(0x12)),        // E
    (0x09, ScanCode::scan(0x21)),        // F
    (0x0a, ScanCode::scan(0x22)),        // G
    (0x0b, ScanCode::scan(0x23)),        // H
    (0x0c, ScanCode::scan(0x17)),        // I
    (0x0d, ScanCode::scan(0x24)),        // J
    (0x0e, ScanCode::scan(0x25)),        // K
    (0x0f, ScanCode::scan(0x26)),        // L
    (0x10, ScanCode::scan(0x32)),        // M
    (0x11, ScanCode::scan(0x31)),        // N
    (0x12, ScanCode::scan(0x18)),        // O
    (0x13, ScanCode::scan(0x19)),        // P
    (0x14, ScanCode::scan(0x10)),        // Q
    (0x15, ScanCode::scan(0x13)),        // R
    (0x16, ScanCode::scan(0x1f)),        // S
    (0x17, ScanCode::scan(0x14)),        // T
    (0x18, ScanCode::scan(0x16)),        // U
    (0x19, ScanCode::scan(0x2f)),        // V
    (0x1a, ScanCode::scan(0x11)),        // W
    (0x1b, ScanCode::scan(0x2d)),        // X
    (0x1c, ScanCode::scan(0x15)),        // Y
    (0x1d, ScanCode::scan(0x2c)),        // Z
    (0x1e, ScanCode::scan(0x02)),        // 1
    (0x1f, ScanCode::scan(0x03)),        // 2
    (0x20, ScanCode::scan(0x04)),        // 3
    (0x21, ScanCode::scan(0x05)),        // 4
    (0x22, ScanCode::scan(0x06)),        // 5
    (0x23, ScanCode::scan(0x07)),        // 6
    (0x24, ScanCode::scan(0x08)),        // 7
    (0x25, ScanCode::scan(0x09)),        // 8
    (0x26, ScanCode::scan(0x0a)),        // 9
    (0x27, ScanCode::scan(0x0b)),        // 0
    (0x28, ScanCode::scan(0x1c)),        // Enter
    (0x29, ScanCode::scan(0x01)),        // Escape
    (0x2a, ScanCode::scan(0x0e)),        // Backspace
    (0x2b, ScanCode::scan(0x0f)),        // Tab
    (0x2c, ScanCode::scan(0x39)),        // Space
    (0x2d, ScanCode::scan(0x0c)),        // - and _
    (0x2e, ScanCode::scan(0x0d)),        // = and +
    (0x2f, ScanCode::scan(0x1a)),        // [ and {
    (0x30, ScanCode::scan(0x1b)),        // ] and }
    (0x31, ScanCode::scan(0x2b)),        // backslash and pipe
    (0x33, ScanCode::scan(0x27)),        // ; and :
    (0x34, ScanCode::scan(0x28)),        // quote
    (0x35, ScanCode::scan(0x29)),        // grave
    (0x36, ScanCode::scan(0x33)),        // comma
    (0x37, ScanCode::scan(0x34)),        // period
    (0x38, ScanCode::scan(0x35)),        // slash
    (0x39, ScanCode::scan(0x3a)),        // Caps Lock
    (0x3a, ScanCode::scan(0x3b)),        // F1
    (0x3b, ScanCode::scan(0x3c)),        // F2
    (0x3c, ScanCode::scan(0x3d)),        // F3
    (0x3d, ScanCode::scan(0x3e)),        // F4
    (0x3e, ScanCode::scan(0x3f)),        // F5
    (0x3f, ScanCode::scan(0x40)),        // F6
    (0x40, ScanCode::scan(0x41)),        // F7
    (0x41, ScanCode::scan(0x42)),        // F8
    (0x42, ScanCode::scan(0x43)),        // F9
    (0x43, ScanCode::scan(0x44)),        // F10
    (0x44, ScanCode::scan(0x57)),        // F11
    (0x45, ScanCode::scan(0x58)),        // F12
    (0x46, ScanCode::extended(0x37)),    // Print Screen
    (0x47, ScanCode::scan(0x46)),        // Scroll Lock
    (0x48, ScanCode::virtual_key(0x13)), // Pause/Break
    (0x49, ScanCode::extended(0x52)),    // Insert
    (0x4a, ScanCode::extended(0x47)),    // Home
    (0x4b, ScanCode::extended(0x49)),    // Page Up
    (0x4c, ScanCode::extended(0x53)),    // Delete
    (0x4d, ScanCode::extended(0x4f)),    // End
    (0x4e, ScanCode::extended(0x51)),    // Page Down
    (0x4f, ScanCode::extended(0x4d)),    // Right
    (0x50, ScanCode::extended(0x4b)),    // Left
    (0x51, ScanCode::extended(0x50)),    // Down
    (0x52, ScanCode::extended(0x48)),    // Up
    (0x53, ScanCode::scan(0x45)),        // Num Lock
    (0x54, ScanCode::extended(0x35)),    // Keypad /
    (0x55, ScanCode::scan(0x37)),        // Keypad *
    (0x56, ScanCode::scan(0x4a)),        // Keypad -
    (0x57, ScanCode::scan(0x4e)),        // Keypad +
    (0x58, ScanCode::extended(0x1c)),    // Keypad Enter
    (0x59, ScanCode::scan(0x4f)),        // Keypad 1
    (0x5a, ScanCode::scan(0x50)),        // Keypad 2
    (0x5b, ScanCode::scan(0x51)),        // Keypad 3
    (0x5c, ScanCode::scan(0x4b)),        // Keypad 4
    (0x5d, ScanCode::scan(0x4c)),        // Keypad 5
    (0x5e, ScanCode::scan(0x4d)),        // Keypad 6
    (0x5f, ScanCode::scan(0x47)),        // Keypad 7
    (0x60, ScanCode::scan(0x48)),        // Keypad 8
    (0x61, ScanCode::scan(0x49)),        // Keypad 9
    (0x62, ScanCode::scan(0x52)),        // Keypad 0
    (0x63, ScanCode::scan(0x53)),        // Keypad decimal
    (0x64, ScanCode::scan(0x56)),        // Non-US backslash
    (0x65, ScanCode::extended(0x5d)),    // Application/Menu
    (0x67, ScanCode::scan(0x59)),        // Keypad =
    (0x68, ScanCode::scan(0x64)),        // F13
    (0x69, ScanCode::scan(0x65)),        // F14
    (0x6a, ScanCode::scan(0x66)),        // F15
    (0x6b, ScanCode::scan(0x67)),        // F16
    (0x6c, ScanCode::scan(0x68)),        // F17
    (0x6d, ScanCode::scan(0x69)),        // F18
    (0x6e, ScanCode::scan(0x6a)),        // F19
    (0x6f, ScanCode::scan(0x6b)),        // F20
    (0x70, ScanCode::scan(0x6c)),        // F21
    (0x71, ScanCode::scan(0x6d)),        // F22
    (0x72, ScanCode::scan(0x6e)),        // F23
    (0x73, ScanCode::scan(0x76)),        // F24
    (0x7f, ScanCode::extended(0x20)),    // Mute
    (0x80, ScanCode::extended(0x30)),    // Volume Up
    (0x81, ScanCode::extended(0x2e)),    // Volume Down
    (0xe0, ScanCode::scan(0x1d)),        // Left Control
    (0xe1, ScanCode::scan(0x2a)),        // Left Shift
    (0xe2, ScanCode::scan(0x38)),        // Left Alt
    (0xe3, ScanCode::extended(0x5b)),    // Left GUI
    (0xe4, ScanCode::extended(0x1d)),    // Right Control
    (0xe5, ScanCode::scan(0x36)),        // Right Shift
    (0xe6, ScanCode::extended(0x38)),    // Right Alt
    (0xe7, ScanCode::extended(0x5c)),    // Right GUI
];

/// Returns the bounded Windows key identity for a USB HID keyboard-page usage.
pub fn hid_to_windows_scancode(hid_usage: u16) -> Option<ScanCode> {
    SUPPORTED_HID_USAGES
        .iter()
        .find_map(|(usage, code)| (*usage == hid_usage).then_some(*code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn maps_all_standard_104_keyboard_usages_and_common_extensions() {
        for usage in 0x04..=0x65 {
            if usage == 0x32 {
                continue; // Reserved on the US ANSI layout; 0x64 is the distinct ISO key.
            }
            assert!(
                hid_to_windows_scancode(usage).is_some(),
                "missing HID usage 0x{usage:02x}"
            );
        }
        for usage in [0x67, 0x68, 0x73, 0x7f, 0x80, 0x81, 0xe0, 0xe7] {
            assert!(hid_to_windows_scancode(usage).is_some(), "0x{usage:02x}");
        }
        assert!(
            hid_to_windows_scancode(0x66).is_none(),
            "Power is not injected"
        );
        assert!(hid_to_windows_scancode(0xffff).is_none());
    }

    #[test]
    fn table_has_unique_usage_and_key_identities() {
        let usages: BTreeSet<_> = SUPPORTED_HID_USAGES
            .iter()
            .map(|(usage, _)| usage)
            .collect();
        assert_eq!(usages.len(), SUPPORTED_HID_USAGES.len());
        let identities: BTreeSet<_> = SUPPORTED_HID_USAGES
            .iter()
            .map(|(_, code)| (code.code, code.extended, code.virtual_key))
            .collect();
        assert_eq!(identities.len(), SUPPORTED_HID_USAGES.len());
    }

    #[test]
    fn distinguishes_left_and_right_modifiers_and_extended_navigation() {
        assert_ne!(hid_to_windows_scancode(0xe0), hid_to_windows_scancode(0xe4));
        assert_eq!(
            hid_to_windows_scancode(0x4f),
            Some(ScanCode::extended(0x4d))
        );
        assert_eq!(
            hid_to_windows_scancode(0x48),
            Some(ScanCode::virtual_key(0x13))
        );
    }
}
