//! Layout-independent physical-key mapping for the live viewer.
//!
//! Key labels and typed text are intentionally ignored. Only iced's physical `Code` is mapped
//! to the USB HID keyboard page used by the input protocol.
use iced::keyboard::{key::Code, Modifiers};
use racc_input::{ReleaseChord, ViewerInputError, ViewerInputEvents, ViewerInputReducer};

/// Maps a validated Settings label to the corresponding local release chord.
pub(crate) fn release_chord_from_setting(setting: &str) -> ReleaseChord {
    match setting {
        crate::settings::CAPTURE_RELEASE_HOTKEY_F12 => ReleaseChord::CtrlAltShiftF12,
        _ => ReleaseChord::CtrlAltShiftEscape,
    }
}

/// Maps a physical iced key location to the protocol's USB HID keyboard-page usage.
pub(crate) fn physical_code_to_hid(code: Code) -> Option<u16> {
    use Code::*;
    Some(match code {
        KeyA => 0x04,
        KeyB => 0x05,
        KeyC => 0x06,
        KeyD => 0x07,
        KeyE => 0x08,
        KeyF => 0x09,
        KeyG => 0x0a,
        KeyH => 0x0b,
        KeyI => 0x0c,
        KeyJ => 0x0d,
        KeyK => 0x0e,
        KeyL => 0x0f,
        KeyM => 0x10,
        KeyN => 0x11,
        KeyO => 0x12,
        KeyP => 0x13,
        KeyQ => 0x14,
        KeyR => 0x15,
        KeyS => 0x16,
        KeyT => 0x17,
        KeyU => 0x18,
        KeyV => 0x19,
        KeyW => 0x1a,
        KeyX => 0x1b,
        KeyY => 0x1c,
        KeyZ => 0x1d,
        Digit1 => 0x1e,
        Digit2 => 0x1f,
        Digit3 => 0x20,
        Digit4 => 0x21,
        Digit5 => 0x22,
        Digit6 => 0x23,
        Digit7 => 0x24,
        Digit8 => 0x25,
        Digit9 => 0x26,
        Digit0 => 0x27,
        Enter => 0x28,
        Escape => 0x29,
        Backspace => 0x2a,
        Tab => 0x2b,
        Space => 0x2c,
        Minus => 0x2d,
        Equal => 0x2e,
        BracketLeft => 0x2f,
        BracketRight => 0x30,
        Backslash => 0x31,
        Semicolon => 0x33,
        Quote => 0x34,
        Backquote => 0x35,
        Comma => 0x36,
        Period => 0x37,
        Slash => 0x38,
        CapsLock => 0x39,
        F1 => 0x3a,
        F2 => 0x3b,
        F3 => 0x3c,
        F4 => 0x3d,
        F5 => 0x3e,
        F6 => 0x3f,
        F7 => 0x40,
        F8 => 0x41,
        F9 => 0x42,
        F10 => 0x43,
        F11 => 0x44,
        F12 => 0x45,
        PrintScreen => 0x46,
        ScrollLock => 0x47,
        Pause => 0x48,
        Insert => 0x49,
        Home => 0x4a,
        PageUp => 0x4b,
        Delete => 0x4c,
        End => 0x4d,
        PageDown => 0x4e,
        ArrowRight => 0x4f,
        ArrowLeft => 0x50,
        ArrowDown => 0x51,
        ArrowUp => 0x52,
        NumLock => 0x53,
        NumpadDivide => 0x54,
        NumpadMultiply => 0x55,
        NumpadSubtract => 0x56,
        NumpadAdd => 0x57,
        NumpadEnter => 0x58,
        Numpad1 => 0x59,
        Numpad2 => 0x5a,
        Numpad3 => 0x5b,
        Numpad4 => 0x5c,
        Numpad5 => 0x5d,
        Numpad6 => 0x5e,
        Numpad7 => 0x5f,
        Numpad8 => 0x60,
        Numpad9 => 0x61,
        Numpad0 => 0x62,
        NumpadDecimal => 0x63,
        IntlBackslash => 0x64,
        ContextMenu => 0x65,
        NumpadEqual => 0x67,
        F13 => 0x68,
        F14 => 0x69,
        F15 => 0x6a,
        F16 => 0x6b,
        F17 => 0x6c,
        F18 => 0x6d,
        F19 => 0x6e,
        F20 => 0x6f,
        F21 => 0x70,
        F22 => 0x71,
        F23 => 0x72,
        F24 => 0x73,
        AudioVolumeMute => 0x7f,
        AudioVolumeUp => 0x80,
        AudioVolumeDown => 0x81,
        ControlLeft => 0xe0,
        ShiftLeft => 0xe1,
        AltLeft => 0xe2,
        SuperLeft => 0xe3,
        ControlRight => 0xe4,
        ShiftRight => 0xe5,
        AltRight => 0xe6,
        SuperRight => 0xe7,
        _ => return None,
    })
}

/// Converts iced's current modifier state to the protocol's Shift/Ctrl/Alt/Meta bit mask.
pub(crate) fn modifier_bits(modifiers: Modifiers) -> u8 {
    u8::from(modifiers.shift())
        | (u8::from(modifiers.control()) << 1)
        | (u8::from(modifiers.alt()) << 2)
        | (u8::from(modifiers.logo()) << 3)
}

/// Maps and reduces one physical key event without consulting a typed key or text value.
#[cfg(test)]
pub(crate) fn reduce_physical_key(
    reducer: &mut ViewerInputReducer,
    code: Code,
    pressed: bool,
    modifiers: Modifiers,
) -> Option<Result<ViewerInputEvents, ViewerInputError>> {
    reduce_physical_key_with_swap(reducer, code, pressed, modifiers, false)
}

/// Maps the Mac Control and Command physical keys to the opposite remote modifiers when enabled.
pub(crate) fn reduce_physical_key_with_swap(
    reducer: &mut ViewerInputReducer,
    code: Code,
    pressed: bool,
    modifiers: Modifiers,
    swap_ctrl_command: bool,
) -> Option<Result<ViewerInputEvents, ViewerInputError>> {
    let code = if swap_ctrl_command {
        match code {
            Code::ControlLeft => Code::SuperLeft,
            Code::ControlRight => Code::SuperRight,
            Code::SuperLeft => Code::ControlLeft,
            Code::SuperRight => Code::ControlRight,
            other => other,
        }
    } else {
        code
    };
    let hid_usage = physical_code_to_hid(code)?;
    Some(reducer.key_event(hid_usage, pressed, modifier_bits(modifiers)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_input::{ViewerControlMode, ViewerInputEvent, SUPPORTED_HID_USAGES};
    use std::collections::BTreeSet;

    #[test]
    fn settings_release_chord_maps_only_to_the_vetted_input_choices() {
        assert_eq!(
            release_chord_from_setting(crate::settings::CAPTURE_RELEASE_HOTKEY_ESCAPE),
            ReleaseChord::CtrlAltShiftEscape
        );
        assert_eq!(
            release_chord_from_setting(crate::settings::CAPTURE_RELEASE_HOTKEY_F12),
            ReleaseChord::CtrlAltShiftF12
        );
        assert_eq!(
            release_chord_from_setting("invalid"),
            ReleaseChord::CtrlAltShiftEscape
        );
    }

    #[test]
    fn physical_mapping_covers_exactly_the_supported_hid_table() {
        use Code::*;
        let codes = [
            KeyA,
            KeyB,
            KeyC,
            KeyD,
            KeyE,
            KeyF,
            KeyG,
            KeyH,
            KeyI,
            KeyJ,
            KeyK,
            KeyL,
            KeyM,
            KeyN,
            KeyO,
            KeyP,
            KeyQ,
            KeyR,
            KeyS,
            KeyT,
            KeyU,
            KeyV,
            KeyW,
            KeyX,
            KeyY,
            KeyZ,
            Digit1,
            Digit2,
            Digit3,
            Digit4,
            Digit5,
            Digit6,
            Digit7,
            Digit8,
            Digit9,
            Digit0,
            Enter,
            Escape,
            Backspace,
            Tab,
            Space,
            Minus,
            Equal,
            BracketLeft,
            BracketRight,
            Backslash,
            Semicolon,
            Quote,
            Backquote,
            Comma,
            Period,
            Slash,
            CapsLock,
            F1,
            F2,
            F3,
            F4,
            F5,
            F6,
            F7,
            F8,
            F9,
            F10,
            F11,
            F12,
            PrintScreen,
            ScrollLock,
            Pause,
            Insert,
            Home,
            PageUp,
            Delete,
            End,
            PageDown,
            ArrowRight,
            ArrowLeft,
            ArrowDown,
            ArrowUp,
            NumLock,
            NumpadDivide,
            NumpadMultiply,
            NumpadSubtract,
            NumpadAdd,
            NumpadEnter,
            Numpad1,
            Numpad2,
            Numpad3,
            Numpad4,
            Numpad5,
            Numpad6,
            Numpad7,
            Numpad8,
            Numpad9,
            Numpad0,
            NumpadDecimal,
            IntlBackslash,
            ContextMenu,
            NumpadEqual,
            F13,
            F14,
            F15,
            F16,
            F17,
            F18,
            F19,
            F20,
            F21,
            F22,
            F23,
            F24,
            AudioVolumeMute,
            AudioVolumeUp,
            AudioVolumeDown,
            ControlLeft,
            ShiftLeft,
            AltLeft,
            SuperLeft,
            ControlRight,
            ShiftRight,
            AltRight,
            SuperRight,
        ];
        let mapped: BTreeSet<_> = codes.into_iter().filter_map(physical_code_to_hid).collect();
        let supported: BTreeSet<_> = SUPPORTED_HID_USAGES
            .iter()
            .map(|(usage, _)| *usage)
            .collect();
        assert_eq!(mapped, supported);
    }

    #[test]
    fn physical_key_uses_hid_identity_and_modifier_bits() {
        use Code::*;
        let mut reducer = ViewerInputReducer::new(7, 3).expect("valid test target");
        reducer.set_connected(true).expect("valid state");
        reducer.set_focused(true).expect("valid state");
        reducer
            .set_mode(ViewerControlMode::RemoteDesktop)
            .expect("valid state");

        let events =
            reduce_physical_key(&mut reducer, KeyA, true, Modifiers::SHIFT | Modifiers::CTRL)
                .expect("mapped physical key")
                .expect("valid reducer input");
        assert!(matches!(
            events.as_slice(),
            [ViewerInputEvent::Input(racc_proto::InputEvent {
                epoch: 7,
                display_id: 3,
                event: racc_proto::InputEventKind::Key {
                    hid_usage: 0x04,
                    pressed: true,
                    modifiers: 0x03,
                },
            })]
        ));
    }

    #[test]
    fn ctrl_command_swap_maps_physical_modifiers_to_opposite_hid_keys() {
        use Code::*;
        let mut reducer = ViewerInputReducer::new(7, 3).expect("valid test target");
        reducer.set_connected(true).expect("valid state");
        reducer.set_focused(true).expect("valid state");
        reducer
            .set_mode(ViewerControlMode::RemoteDesktop)
            .expect("valid state");

        for (physical, expected_hid) in [(ControlLeft, 0xe3), (SuperRight, 0xe4)] {
            let events = reduce_physical_key_with_swap(
                &mut reducer,
                physical,
                true,
                Modifiers::empty(),
                true,
            )
            .expect("mapped physical modifier")
            .expect("valid reducer input");
            assert!(matches!(
                events.as_slice(),
                [ViewerInputEvent::Input(racc_proto::InputEvent {
                    event: racc_proto::InputEventKind::Key { hid_usage, pressed: true, .. },
                    ..
                })] if *hid_usage == expected_hid
            ));
        }

        assert_eq!(physical_code_to_hid(ControlLeft), Some(0xe0));
        assert_eq!(physical_code_to_hid(SuperRight), Some(0xe7));
    }

    #[test]
    fn release_chord_is_consumed_by_the_reducer() {
        use Code::*;
        let mut reducer = ViewerInputReducer::new(1, 1).expect("valid test target");
        reducer.set_connected(true).expect("valid state");
        reducer.set_focused(true).expect("valid state");
        reducer
            .set_mode(ViewerControlMode::RemoteDesktop)
            .expect("valid state");
        for code in [ControlLeft, AltLeft, ShiftLeft] {
            reduce_physical_key(&mut reducer, code, true, Modifiers::empty())
                .expect("mapped modifier")
                .expect("valid reducer input");
        }

        let events = reduce_physical_key(
            &mut reducer,
            Escape,
            true,
            Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT,
        )
        .expect("mapped escape")
        .expect("valid reducer input");
        assert!(!events.as_slice().iter().any(|event| matches!(
            event,
            ViewerInputEvent::Input(racc_proto::InputEvent {
                event: racc_proto::InputEventKind::Key {
                    hid_usage: 0x29,
                    ..
                },
                ..
            })
        )));
        assert!(events
            .as_slice()
            .contains(&ViewerInputEvent::ReleaseChordTriggered));
        assert!(!reducer.is_capturing());
    }

    #[test]
    fn unsupported_physical_codes_stay_local() {
        assert_eq!(physical_code_to_hid(Code::IntlYen), None);
        assert_eq!(modifier_bits(Modifiers::empty()), 0);
        assert_eq!(
            modifier_bits(Modifiers::SHIFT | Modifiers::CTRL | Modifiers::ALT | Modifiers::LOGO),
            0x0f
        );
    }
}
