#![allow(unsafe_code)]

//! Windows SendInput backend. The containing helper must run on its authorized input desktop.

use std::mem::size_of;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MOUSE_EVENT_FLAGS,
    VIRTUAL_KEY,
};

use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;

use racc_topology::{windows_absolute, CoordinateError};

use crate::{
    hid_to_windows_scancode, InputCommand, InputError, InputInjector, PointerInjectionMethod,
};

/// Synchronous Windows user32 SendInput injector.
#[derive(Clone, Copy, Debug)]
pub struct WindowsSendInput {
    pointer_method: PointerInjectionMethod,
}

impl Default for WindowsSendInput {
    fn default() -> Self {
        Self::new(PointerInjectionMethod::default())
    }
}

impl WindowsSendInput {
    /// Creates an injector with a validated pointer method.
    pub const fn new(pointer_method: PointerInjectionMethod) -> Self {
        Self { pointer_method }
    }

    /// Reads the optional validated local RACC_POINTER_INJECTION_METHOD setting.
    ///
    /// Accepted values are virtual-desktop-absolute and set-cursor-pos. An invalid value
    /// returns an error instead of silently selecting another injection method.
    pub fn from_environment() -> std::io::Result<Self> {
        match std::env::var("RACC_POINTER_INJECTION_METHOD") {
            Ok(value) => PointerInjectionMethod::from_setting(&value)
                .map(Self::new)
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "RACC_POINTER_INJECTION_METHOD must be virtual-desktop-absolute or set-cursor-pos",
                    )
                }),
            Err(std::env::VarError::NotPresent) => Ok(Self::default()),
            Err(std::env::VarError::NotUnicode(_)) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "RACC_POINTER_INJECTION_METHOD is not valid Unicode",
            )),
        }
    }
}

impl InputInjector for WindowsSendInput {
    fn inject(&mut self, command: InputCommand) -> Result<(), InputError> {
        match command {
            InputCommand::Key { hid_usage, pressed } => {
                let scan_code = hid_to_windows_scancode(hid_usage)
                    .ok_or(InputError::UnsupportedHidUsage(hid_usage))?;
                send_keyboard(scan_code, pressed)
            }
            InputCommand::MouseMoveAbsolute {
                pixel,
                virtual_desktop,
            } => match self.pointer_method {
                PointerInjectionMethod::VirtualDesktopAbsolute => {
                    let absolute =
                        windows_absolute(pixel, virtual_desktop).map_err(InputError::Coordinate)?;
                    send_mouse(
                        i32::from(absolute.x),
                        i32::from(absolute.y),
                        0,
                        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                    )
                }
                PointerInjectionMethod::SetCursorPos => {
                    let x = i32::try_from(pixel.x)
                        .map_err(|_| InputError::Coordinate(CoordinateError::ArithmeticOverflow))?;
                    let y = i32::try_from(pixel.y)
                        .map_err(|_| InputError::Coordinate(CoordinateError::ArithmeticOverflow))?;
                    // SAFETY: x and y are checked screen coordinates from the validated active topology.
                    // SetCursorPos retains no pointers; the helper input worker is dispatched on its input desktop.
                    unsafe { SetCursorPos(x, y) }.map_err(|_| InputError::OsInjectionFailed)
                }
            },
            InputCommand::MouseMoveRelative { dx, dy } => {
                send_mouse(i32::from(dx), i32::from(dy), 0, MOUSEEVENTF_MOVE)
            }
            InputCommand::MouseButton { button, pressed } => {
                let (flags, data) = match (button, pressed) {
                    (1, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                    (1, false) => (MOUSEEVENTF_LEFTUP, 0),
                    (2, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                    (2, false) => (MOUSEEVENTF_RIGHTUP, 0),
                    (3, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                    (3, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                    (4 | 5, true) => (MOUSEEVENTF_XDOWN, u32::from(button - 3)),
                    (4 | 5, false) => (MOUSEEVENTF_XUP, u32::from(button - 3)),
                    _ => return Err(InputError::InvalidMouseButton(button)),
                };
                send_mouse(0, 0, data, flags)
            }
            InputCommand::Wheel { dx, dy } => {
                if dx != 0 {
                    send_mouse(0, 0, dx as i32 as u32, MOUSEEVENTF_HWHEEL)?;
                }
                if dy != 0 {
                    send_mouse(0, 0, dy as i32 as u32, MOUSEEVENTF_WHEEL)?;
                }
                Ok(())
            }
        }
    }
}

fn send_keyboard(scan_code: crate::ScanCode, pressed: bool) -> Result<(), InputError> {
    let (virtual_key, scan, mut flags) = match scan_code.virtual_key {
        Some(key) if scan_code.code == 0 && key == 0x13 => {
            (VIRTUAL_KEY(key), 0, Default::default())
        }
        Some(_) => return Err(InputError::OsInjectionFailed),
        None if (1..=0x7f).contains(&scan_code.code) => {
            let mut flags = KEYEVENTF_SCANCODE;
            if scan_code.extended {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            (VIRTUAL_KEY(0), scan_code.code, flags)
        }
        None => return Err(InputError::OsInjectionFailed),
    };
    if !pressed {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: virtual_key,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    send_one(input)
}

fn send_mouse(
    dx: i32,
    dy: i32,
    mouse_data: u32,
    flags: MOUSE_EVENT_FLAGS,
) -> Result<(), InputError> {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: mouse_data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    send_one(input)
}

fn send_one(input: INPUT) -> Result<(), InputError> {
    let Ok(size) = i32::try_from(size_of::<INPUT>()) else {
        return Err(InputError::OsInjectionFailed);
    };
    // SAFETY: `input` is a fully initialized INPUT with its union member matching the type tag;
    // the one-element slice remains valid for the synchronous SendInput call and uses its exact size.
    let inserted = unsafe { SendInput(&[input], size) };
    if inserted == 1 {
        Ok(())
    } else {
        Err(InputError::OsInjectionFailed)
    }
}
