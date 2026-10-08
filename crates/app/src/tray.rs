//! Native tray adapters for supported desktop platforms.

/// Commands that a platform tray adapter can dispatch to the UI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayAction {
    /// Restore and focus the main window.
    ShowWindow,
    /// Toggle the persisted local hosting preference.
    ToggleHosting,
    /// Request application shutdown.
    Quit,
}

/// Platform tray interface owned by the application shell.
pub trait TrayController {
    /// Reports whether the current platform has a tray adapter.
    fn available(&self) -> bool;

    /// Creates the icon after the UI event loop has started.
    fn initialize(
        &mut self,
        hosting: bool,
        state: crate::settings::TrayConnectionState,
    ) -> Result<(), ()>;

    /// Synchronizes the checked hosting preference in the tray menu.
    fn set_hosting(&mut self, hosting: bool);

    /// Updates the privacy-safe connection-state tooltip.
    fn set_connection_state(&mut self, state: crate::settings::TrayConnectionState);

    /// Returns the next user action from the tray, if any.
    fn poll_action(&mut self) -> Option<TrayAction>;
}

/// Native Windows and macOS tray controller; other platforms use the no-op controller.
#[derive(Default)]
pub struct PlatformTrayController {
    attempted: bool,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    native: Option<NativeTray>,
}

impl PlatformTrayController {
    /// Creates an uninitialized tray controller.
    pub fn new() -> Self {
        Self::default()
    }
}

impl TrayController for PlatformTrayController {
    fn available(&self) -> bool {
        cfg!(any(target_os = "windows", target_os = "macos"))
    }

    fn initialize(
        &mut self,
        hosting: bool,
        state: crate::settings::TrayConnectionState,
    ) -> Result<(), ()> {
        if self.attempted {
            return if self.available() && self.native_missing() {
                Err(())
            } else {
                Ok(())
            };
        }
        self.attempted = true;
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            self.native = Some(NativeTray::new(hosting, state)?);
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let _ = (hosting, state);
        Ok(())
    }

    fn set_hosting(&mut self, hosting: bool) {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        if let Some(native) = &mut self.native {
            native.set_hosting(hosting);
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let _ = hosting;
    }

    fn set_connection_state(&mut self, state: crate::settings::TrayConnectionState) {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        if let Some(native) = &mut self.native {
            native.set_connection_state(state);
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let _ = state;
    }

    fn poll_action(&mut self) -> Option<TrayAction> {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        if let Some(native) = &self.native {
            return native.poll_action();
        }
        None
    }
}

impl PlatformTrayController {
    fn native_missing(&self) -> bool {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            self.native.is_none()
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            false
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
struct NativeTray {
    _icon: tray_icon::TrayIcon,
    icon_id: tray_icon::TrayIconId,
    hosting_item: tray_icon::menu::CheckMenuItem,
    connection: crate::settings::TrayConnectionState,
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
impl NativeTray {
    fn new(hosting: bool, connection: crate::settings::TrayConnectionState) -> Result<Self, ()> {
        use tray_icon::{menu, Icon, TrayIconBuilder};

        const OPEN_ID: &str = "racc-open";
        const HOSTING_ID: &str = "racc-hosting";
        const QUIT_ID: &str = "racc-quit";

        let icon_bytes: &[u8] = if cfg!(target_os = "macos") {
            include_bytes!("../../../assets/icons/racc-menubar-template.rgba")
        } else if connection == crate::settings::TrayConnectionState::Connected {
            include_bytes!("../../../assets/icons/racc-tray-connected.rgba")
        } else {
            include_bytes!("../../../assets/icons/racc-tray-disconnected.rgba")
        };
        let icon = Icon::from_rgba(icon_bytes.to_vec(), 32, 32).map_err(|_| ())?;
        let menu = menu::Menu::new();
        let open = menu::MenuItem::with_id(OPEN_ID, "Open Racc Connect", true, None);
        let hosting_item =
            menu::CheckMenuItem::with_id(HOSTING_ID, "Hosting preference", true, hosting, None);
        let quit = menu::MenuItem::with_id(QUIT_ID, "Quit", true, None);
        menu.append(&open).map_err(|_| ())?;
        menu.append(&hosting_item).map_err(|_| ())?;
        menu.append(&quit).map_err(|_| ())?;
        let builder = TrayIconBuilder::new()
            .with_tooltip(crate::settings::TrayMenuModel::new(hosting, connection).tooltip())
            .with_icon(icon)
            .with_menu(Box::new(menu));
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_as_template(true);
        let icon_id = builder.id().clone();
        let icon = builder.build().map_err(|_| ())?;
        Ok(Self {
            _icon: icon,
            icon_id,
            hosting_item,
            connection,
        })
    }

    fn set_hosting(&mut self, hosting: bool) {
        self.hosting_item.set_checked(hosting);
        self.hosting_item.set_text(if hosting {
            "Hosting preference is on"
        } else {
            "Hosting preference is off"
        });
    }

    fn set_connection_state(&mut self, state: crate::settings::TrayConnectionState) {
        if self.connection != state {
            self.connection = state;
            let tooltip =
                crate::settings::TrayMenuModel::new(self.hosting_item.is_checked(), state)
                    .tooltip();
            let _ = self._icon.set_tooltip(Some(tooltip));
            #[cfg(target_os = "windows")]
            {
                let icon_bytes: &[u8] = if state == crate::settings::TrayConnectionState::Connected
                {
                    include_bytes!("../../../assets/icons/racc-tray-connected.rgba")
                } else {
                    include_bytes!("../../../assets/icons/racc-tray-disconnected.rgba")
                };
                if let Ok(icon) = tray_icon::Icon::from_rgba(icon_bytes.to_vec(), 32, 32) {
                    let _ = self._icon.set_icon(Some(icon));
                }
            }
        }
    }

    fn poll_action(&self) -> Option<TrayAction> {
        use tray_icon::{menu::MenuEvent, MouseButton, TrayIconEvent};

        for _ in 0..32 {
            let Ok(event) = MenuEvent::receiver().try_recv() else {
                break;
            };
            match event.id().0.as_str() {
                "racc-open" => return Some(TrayAction::ShowWindow),
                "racc-hosting" => return Some(TrayAction::ToggleHosting),
                "racc-quit" => return Some(TrayAction::Quit),
                _ => {}
            }
        }
        for _ in 0..32 {
            let Ok(event) = TrayIconEvent::receiver().try_recv() else {
                break;
            };
            match event {
                TrayIconEvent::Click {
                    id,
                    button: MouseButton::Left,
                    ..
                } if id == self.icon_id => return Some(TrayAction::ShowWindow),
                TrayIconEvent::DoubleClick {
                    id,
                    button: MouseButton::Left,
                    ..
                } if id == self.icon_id => return Some(TrayAction::ShowWindow),
                _ => {}
            }
        }
        None
    }
}
