/// A pure model for the supported tray actions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayAction {
    /// Bring the main window to the foreground.
    ShowWindow,
    /// Toggle the host preference.
    ToggleHosting,
    /// Stop the UI process only.
    QuitApp,
}

/// Connection state shown in the tray tooltip.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayConnectionState {
    /// No remote session is active.
    Disconnected,
    /// A remote session is negotiating.
    Connecting,
    /// A remote session is active.
    Connected,
    /// A remote session has a recoverable error.
    Reconnecting,
}

/// A tray menu command and display label, independent from platform widgets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrayMenuItem {
    /// Human-readable menu label.
    pub label: &'static str,
    /// Command dispatched when selected.
    pub action: TrayAction,
    /// Whether the item is checked.
    pub checked: bool,
}

/// Platform-independent menu contents and left-click rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrayMenuModel {
    hosting: bool,
    connection: TrayConnectionState,
}

impl TrayMenuModel {
    /// Builds a menu model from the current app metadata.
    pub const fn new(hosting: bool, connection: TrayConnectionState) -> Self {
        Self {
            hosting,
            connection,
        }
    }

    /// Returns the stable menu sequence Open, Hosting, Quit.
    pub fn items(self) -> [TrayMenuItem; 3] {
        [
            TrayMenuItem {
                label: "Open Racc Connect",
                action: TrayAction::ShowWindow,
                checked: false,
            },
            TrayMenuItem {
                label: "Hosting",
                action: TrayAction::ToggleHosting,
                checked: self.hosting,
            },
            TrayMenuItem {
                label: "Quit",
                action: TrayAction::QuitApp,
                checked: false,
            },
        ]
    }

    /// Left-click restores the window on both Windows and macOS.
    pub const fn left_click_action(self) -> TrayAction {
        TrayAction::ShowWindow
    }

    /// Tooltip text contains only app and connection state, never device names.
    pub const fn tooltip(self) -> &'static str {
        match self.connection {
            TrayConnectionState::Disconnected => "Racc Connect — Disconnected",
            TrayConnectionState::Connecting => "Racc Connect — Connecting",
            TrayConnectionState::Connected => "Racc Connect — Connected",
            TrayConnectionState::Reconnecting => "Racc Connect — Reconnecting",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_has_open_hosting_quit_and_left_click_opens() {
        let model = TrayMenuModel::new(true, TrayConnectionState::Connected);
        assert_eq!(
            model.items().map(|item| item.action),
            [
                TrayAction::ShowWindow,
                TrayAction::ToggleHosting,
                TrayAction::QuitApp
            ]
        );
        assert!(model.items()[1].checked);
        assert_eq!(model.left_click_action(), TrayAction::ShowWindow);
        assert_eq!(model.tooltip(), "Racc Connect — Connected");
    }
}
