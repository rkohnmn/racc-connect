//! Input validation, viewer capture reduction, and host injection interfaces.
//!
//! This crate validates protocol input before an authorized host helper injects it. It
//! deliberately does not log input payloads. OS injection lives in thin platform modules.
#![warn(missing_docs)]
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

mod controller;
mod fake;
mod hid;
mod macos_geometry;
mod macos_keymap;
mod viewer;

pub use controller::{
    InputCommand, InputError, InputInjectionController, InputInjector, InputRateLimiter,
    PointerInjectionMethod, MAX_INPUT_EVENTS_PER_SECOND,
};
pub use fake::FakeInputInjector;
pub use hid::{hid_to_windows_scancode, ScanCode, SUPPORTED_HID_USAGES};
pub use macos_geometry::MacDisplayGeometry;
pub use macos_keymap::{hid_to_macos_keycode, macos_keycode_to_hid};
pub use viewer::{
    ReleaseChord, ViewerControlMode, ViewerInputError, ViewerInputEvent, ViewerInputEvents,
    ViewerInputReducer, ViewerMouseInput, MAX_VIEWER_EVENTS_PER_ACTION,
};

/// Windows SendInput implementation. It is synchronous; callers must dispatch it on the
/// helper's input worker rather than a network task.
#[cfg(windows)]
pub mod windows;

/// macOS Quartz input implementation. The adapter checks Accessibility trust before injection.
#[cfg(target_os = "macos")]
pub mod macos;
