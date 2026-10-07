//! Minimal tray-controller boundary; a native tray is added during M10.

/// Commands that a platform tray adapter can dispatch to the UI.
#[allow(dead_code)] // Variants are constructed by the native M10 adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayAction {
    /// Restore the main window.
    ShowWindow,
    /// Request application shutdown.
    Quit,
}

/// Platform tray interface owned by the application shell.
pub trait TrayController: Send {
    /// Reports whether the current platform has a tray adapter.
    fn available(&self) -> bool;

    /// Returns the next user action from the tray, if any.
    fn poll_action(&mut self) -> Option<TrayAction>;
}

/// No-op tray implementation used by the fake M4b shell.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopTrayController;

impl TrayController for NoopTrayController {
    fn available(&self) -> bool {
        false
    }

    fn poll_action(&mut self) -> Option<TrayAction> {
        None
    }
}
