//! Read-only Screen Recording permission query with an explicit, opt-in request operation.
#![allow(unsafe_code)]

/// Screen Recording permission reported by the macOS preflight API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScreenRecordingAccess {
    /// The process may acquire screen content.
    Granted,
    /// Permission is absent and the app must present an explanatory UI state.
    Missing,
}

/// Reads current Screen Recording permission without prompting the user.
pub fn screen_recording_access() -> ScreenRecordingAccess {
    // SAFETY: This preflight API accepts no pointers, does not modify settings, and returns the
    // current TCC authorization state for the calling process.
    // SAFETY: CoreGraphics permission preflight is a zero-argument, read-only query.
    if unsafe { CGPreflightScreenCaptureAccess() != 0 } {
        ScreenRecordingAccess::Granted
    } else {
        ScreenRecordingAccess::Missing
    }
}

/// Requests Screen Recording permission only when called from an explicit user action.
///
/// This function may present an operating-system prompt. Construction and passive permission
/// checks never call it.
pub fn request_screen_recording_access() -> ScreenRecordingAccess {
    // SAFETY: This preflight/request API takes no pointers. It is called only through this explicit
    // user-invoked function; no app startup path calls it automatically.
    // SAFETY: The explicit request takes no pointers and is only called from user-triggered UI.
    if unsafe { CGRequestScreenCaptureAccess() != 0 } {
        ScreenRecordingAccess::Granted
    } else {
        ScreenRecordingAccess::Missing
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> u8;
    fn CGRequestScreenCaptureAccess() -> u8;
}
