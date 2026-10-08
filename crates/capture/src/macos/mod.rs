//! macOS capture and display metadata APIs.
//!
//! No screen stream is started during module construction or enumeration. A host explicitly
//! starts capture and receives retained CVPixelBuffer frames through the native Mac adapter.

mod capture;
mod control;
mod displays;
mod permission;

pub use capture::{
    MacCaptureBackend, MacCaptureConfig, MacCaptureError, MacCaptureEvent, MacCapturedFrame,
};
pub use control::{MacCaptureController, MacCaptureNotice, MacCapturePath, MacCaptureSource};
pub use displays::{enumerate_displays, MacDisplay, MacDisplayError};
pub use permission::{
    request_screen_recording_access, screen_recording_access, ScreenRecordingAccess,
};
