//! Versioned per-user settings and pure lifecycle models for the desktop app.
//!
//! This crate deliberately has no OS, UI, networking, or secret-store dependency.
//! Platform adapters can persist the validated model without exposing credentials.

mod autostart;
mod persistence;
mod tray;

pub use autostart::{
    apply_autostart_plan, apply_current_platform_autostart, current_platform_autostart_plan,
    mac_launch_agent_plan, mac_launch_agent_plist, windows_run_value, AutostartBackend,
    AutostartPlan, AutostartTarget, AUTOSTART_ENTRY_NAME, AUTOSTART_LAUNCH_AGENT_LABEL,
};
pub use persistence::{
    default_config_path, restore_geometry_for_displays, LoadNotice, LoadOutcome, QualityPreference,
    ScreenWorkArea, Settings, SettingsStore, StoreError, WindowGeometry,
    CAPTURE_RELEASE_HOTKEY_ESCAPE, CAPTURE_RELEASE_HOTKEY_F12, CAPTURE_RELEASE_HOTKEY_OPTIONS,
    CURRENT_SCHEMA_VERSION, MAX_SETTINGS_BYTES,
};
pub use tray::{TrayAction, TrayConnectionState, TrayMenuItem, TrayMenuModel};

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Installs a panic hook that appends a bounded diagnostic to the per-user log folder.
/// The caller is responsible for choosing a private, per-user directory.
/// Panic payloads are deliberately redacted because they can contain private runtime data.
/// No frames, clipboard contents, input events, or environment variables are read here.
pub fn install_panic_log_hook(log_dir: impl AsRef<Path>) {
    let log_dir = log_dir.as_ref().to_path_buf();
    let write_lock = Arc::new(Mutex::new(()));
    std::panic::set_hook(Box::new(move |panic_info| {
        let Ok(_guard) = write_lock.lock() else {
            return;
        };
        let location = panic_info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "unknown location".to_owned());
        if fs::create_dir_all(&log_dir).is_err() {
            return;
        }
        let path = log_dir.join("racc-panic.log");
        if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > 1024 * 1024) {
            let _ = fs::remove_file(&path);
        }
        let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
            return;
        };
        let _ = writeln!(file, "panic at {location}: payload redacted");
    }));
}
