//! Read-only macOS permission status and explicit links to the matching System Settings pane.

/// The privacy pane that should be opened for a missing app permission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermissionPane {
    /// macOS Screen Recording privacy controls.
    ScreenRecording,
    /// macOS Accessibility privacy controls.
    Accessibility,
}

impl PermissionPane {
    /// Returns the fixed macOS deep link for this permission pane.
    pub const fn settings_url(self) -> &'static str {
        match self {
            Self::ScreenRecording => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            }
            Self::Accessibility => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            }
        }
    }
}

/// Opens only the explicitly selected macOS privacy pane.
///
/// The executable and URL are fixed values; this does not invoke a shell or request a permission.
#[cfg(target_os = "macos")]
pub fn open_settings(pane: PermissionPane) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(pane.settings_url())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open System Settings: {error}"))
}

#[cfg(test)]
mod tests {
    use super::PermissionPane;

    #[test]
    fn each_permission_targets_its_own_privacy_pane() {
        assert_eq!(
            PermissionPane::ScreenRecording.settings_url(),
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
        );
        assert_eq!(
            PermissionPane::Accessibility.settings_url(),
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
        );
        assert_ne!(
            PermissionPane::ScreenRecording.settings_url(),
            PermissionPane::Accessibility.settings_url()
        );
    }
}
