use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Current on-disk schema. Version 3 adds a per-user sign-in autostart preference.
pub const CURRENT_SCHEMA_VERSION: u32 = 3;
/// Settings are preferences only; a larger file is treated as corrupt and quarantined.
pub const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
const MIN_WINDOW_SIZE: u32 = 320;
const MAX_WINDOW_SIZE: u32 = 8192;
const MAX_OPAQUE_ID_BYTES: usize = 256;
/// Default local release chord, always retained as an emergency fallback.
pub const CAPTURE_RELEASE_HOTKEY_ESCAPE: &str = "Ctrl+Alt+Shift+Escape";
/// Alternate supported local release chord.
pub const CAPTURE_RELEASE_HOTKEY_F12: &str = "Ctrl+Alt+Shift+F12";
/// Supported release chords shown by the Settings picker.
pub const CAPTURE_RELEASE_HOTKEY_OPTIONS: [&str; 2] =
    [CAPTURE_RELEASE_HOTKEY_ESCAPE, CAPTURE_RELEASE_HOTKEY_F12];
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// User-selected stream quality preference. The host remains authoritative.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityPreference {
    /// Let the host select a supported tier.
    #[default]
    Auto,
    /// Prefer a 480-pixel stream height.
    P480,
    /// Prefer a 720-pixel stream height.
    P720,
    /// Prefer a 1080-pixel stream height.
    P1080,
}

/// A saved outer-window rectangle in the UI toolkit's desktop coordinate units.
/// The current iced adapter persists logical points without DPI normalization.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WindowGeometry {
    /// Left coordinate, which may be negative on a multi-monitor desktop.
    pub x: i32,
    /// Top coordinate, which may be negative on a multi-monitor desktop.
    pub y: i32,
    /// Requested width in pixels.
    pub width: u32,
    /// Requested height in pixels.
    pub height: u32,
}

impl Default for WindowGeometry {
    fn default() -> Self {
        Self {
            x: 80,
            y: 80,
            width: 1280,
            height: 800,
        }
    }
}

/// Work area of one current display, excluding reserved taskbar/menu areas.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScreenWorkArea {
    /// Left coordinate in virtual desktop pixels.
    pub x: i32,
    /// Top coordinate in virtual desktop pixels.
    pub y: i32,
    /// Usable width in pixels.
    pub width: u32,
    /// Usable height in pixels.
    pub height: u32,
}

/// Settings stored in the current user's config directory.
///
/// Device and display strings are opaque IDs only. This type has no fields for
/// credentials, addresses, peer names, clipboard contents, or input events.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct Settings {
    /// On-disk schema discriminator.
    pub schema_version: u32,
    /// Optional last window rectangle.
    pub window_geometry: Option<WindowGeometry>,
    /// Whether the device list starts collapsed.
    pub device_sidebar_collapsed: bool,
    /// Whether the telemetry panel starts collapsed.
    pub telemetry_sidebar_collapsed: bool,
    /// Preferred stream quality; the host may choose a lower tier.
    pub default_quality: QualityPreference,
    /// Preferred local release chord; the default Escape chord remains an emergency fallback.
    pub capture_release_hotkey: String,
    /// Whether text clipboard synchronization should be enabled after future session handshakes.
    pub clipboard_enabled: bool,
    /// Whether the user prefers this machine to host when its agent is available.
    pub hosting_enabled: bool,
    /// Whether the app should start when the current user signs in.
    pub autostart_enabled: bool,
    /// Whether Ctrl and Command are swapped for a Mac viewer.
    pub swap_ctrl_command: bool,
    /// Last selected device's opaque ID, if known.
    pub last_device_id: Option<String>,
    /// Last selected display's opaque ID, if known.
    pub last_display_id: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            window_geometry: Some(WindowGeometry::default()),
            device_sidebar_collapsed: false,
            telemetry_sidebar_collapsed: false,
            default_quality: QualityPreference::Auto,
            capture_release_hotkey: CAPTURE_RELEASE_HOTKEY_ESCAPE.to_owned(),
            clipboard_enabled: false,
            hosting_enabled: true,
            autostart_enabled: false,
            swap_ctrl_command: false,
            last_device_id: None,
            last_display_id: None,
        }
    }
}

impl Settings {
    fn validate(&self) -> Result<(), String> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err("settings schema version was not migrated".to_owned());
        }
        if !CAPTURE_RELEASE_HOTKEY_OPTIONS.contains(&self.capture_release_hotkey.as_str()) {
            return Err("capture release hotkey is not a supported choice".to_owned());
        }
        if let Some(geometry) = self.window_geometry {
            if geometry.width < MIN_WINDOW_SIZE
                || geometry.height < MIN_WINDOW_SIZE
                || geometry.width > MAX_WINDOW_SIZE
                || geometry.height > MAX_WINDOW_SIZE
            {
                return Err("window geometry is outside supported bounds".to_owned());
            }
        }
        for id in [
            self.last_device_id.as_deref(),
            self.last_display_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if id.trim().is_empty()
                || id.len() > MAX_OPAQUE_ID_BYTES
                || id.chars().any(char::is_control)
            {
                return Err("saved identifiers must be bounded opaque IDs".to_owned());
            }
        }
        Ok(())
    }
}

/// The user-visible explanation produced when a settings file needs attention.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoadNotice {
    /// The invalid file was moved aside; defaults are active.
    CorruptFileQuarantined { quarantined_path: PathBuf },
    /// File was too large to parse and was moved aside.
    OversizedFileQuarantined { quarantined_path: PathBuf },
    /// A newer app wrote the file; defaults are used without modifying it.
    NewerSchema { found: u64 },
    /// A legacy schema was migrated and persisted in the current format.
    Migrated { from: u64, to: u32 },
}

/// A successful load plus an optional message the app should show in its UI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadOutcome {
    /// Loaded or default settings.
    pub settings: Settings,
    /// User-visible status, if the file was changed or could not be used.
    pub notice: Option<LoadNotice>,
}

/// Errors writing settings or moving corrupt settings aside.
#[derive(Debug)]
pub enum StoreError {
    /// Filesystem operation failed.
    Io(io::Error),
    /// Current settings could not be serialized within the bounded file size.
    InvalidSettings(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "settings I/O failed: {error}"),
            Self::InvalidSettings(reason) => write!(formatter, "invalid settings: {reason}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Persistence facade with schema migration, atomic writes, and corruption recovery.
#[derive(Clone, Debug)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    /// Uses the explicit path, typically from [`default_config_path`].
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the settings file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads defaults if no file exists; corrupt files are quarantined and reported.
    pub fn load(&self) -> Result<LoadOutcome, StoreError> {
        if !self.path.exists() {
            return Ok(LoadOutcome {
                settings: Settings::default(),
                notice: None,
            });
        }
        let metadata_len = fs::metadata(&self.path)?.len();
        if metadata_len > MAX_SETTINGS_BYTES {
            let quarantined_path = self.quarantine()?;
            return Ok(LoadOutcome {
                settings: Settings::default(),
                notice: Some(LoadNotice::OversizedFileQuarantined { quarantined_path }),
            });
        }
        let mut file = File::open(&self.path)?;
        let mut bytes = Vec::with_capacity(metadata_len as usize);
        Read::by_ref(&mut file)
            .take(MAX_SETTINGS_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            let quarantined_path = self.quarantine()?;
            return Ok(LoadOutcome {
                settings: Settings::default(),
                notice: Some(LoadNotice::OversizedFileQuarantined { quarantined_path }),
            });
        }
        let value: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                let quarantined_path = self.quarantine()?;
                return Ok(LoadOutcome {
                    settings: Settings::default(),
                    notice: Some(LoadNotice::CorruptFileQuarantined { quarantined_path }),
                });
            }
        };
        let version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1);
        if version > CURRENT_SCHEMA_VERSION as u64 {
            return Ok(LoadOutcome {
                settings: Settings::default(),
                notice: Some(LoadNotice::NewerSchema { found: version }),
            });
        }
        if version < 1 {
            let quarantined_path = self.quarantine()?;
            return Ok(LoadOutcome {
                settings: Settings::default(),
                notice: Some(LoadNotice::CorruptFileQuarantined { quarantined_path }),
            });
        }
        let (value, migrated) = match version {
            1 => (migrate_v1(value), true),
            2 => (migrate_v2(value), true),
            _ => (value, false),
        };
        let mut settings: Settings = match serde_json::from_value(value) {
            Ok(settings) => settings,
            Err(_) => {
                let quarantined_path = self.quarantine()?;
                return Ok(LoadOutcome {
                    settings: Settings::default(),
                    notice: Some(LoadNotice::CorruptFileQuarantined { quarantined_path }),
                });
            }
        };
        settings.schema_version = CURRENT_SCHEMA_VERSION;
        if settings.validate().is_err() {
            let quarantined_path = self.quarantine()?;
            return Ok(LoadOutcome {
                settings: Settings::default(),
                notice: Some(LoadNotice::CorruptFileQuarantined { quarantined_path }),
            });
        }
        let notice = if migrated {
            self.save(&settings)?;
            Some(LoadNotice::Migrated {
                from: version,
                to: CURRENT_SCHEMA_VERSION,
            })
        } else {
            None
        };
        Ok(LoadOutcome { settings, notice })
    }

    /// Serializes and atomically replaces the settings file after validation.
    pub fn save(&self, settings: &Settings) -> Result<(), StoreError> {
        settings.validate().map_err(StoreError::InvalidSettings)?;
        let bytes = serde_json::to_vec_pretty(settings)
            .map_err(|error| StoreError::InvalidSettings(error.to_string()))?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(StoreError::InvalidSettings(
                "serialized file exceeds size limit".to_owned(),
            ));
        }
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("settings.json");
        let temp_path = parent.join(format!(".{name}.tmp-{}-{sequence}", std::process::id()));
        let write_result = (|| -> io::Result<()> {
            let mut temporary = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)?;
            temporary.write_all(&bytes)?;
            temporary.sync_all()?;
            drop(temporary);
            fs::rename(&temp_path, &self.path)?;
            if let Ok(directory) = File::open(parent) {
                let _ = directory.sync_all();
            }
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        write_result.map_err(StoreError::Io)
    }

    fn quarantine(&self) -> io::Result<PathBuf> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("settings.json");
        for suffix in 0..100_u32 {
            let path = parent.join(format!("{name}.corrupt-{stamp}-{suffix}"));
            if !path.exists() {
                fs::rename(&self.path, &path)?;
                return Ok(path);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not choose a unique corrupt-settings name",
        ))
    }
}

/// Resolves the app's per-user config path without storing credentials or machine identity.
pub fn default_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|path| path.join("RaccConnect").join("settings.json"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(PathBuf::from).map(|path| {
            path.join("Library")
                .join("Application Support")
                .join("RaccConnect")
                .join("settings.json")
        })
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|path| path.join(".config"))
            })
            .map(|path| path.join("racc-connect").join("settings.json"))
    }
}

/// Clamps or recenters a saved window rectangle into a currently available work area.
/// If no displays are known, a safe visible default at the origin is returned.
pub fn restore_geometry_for_displays(
    saved: Option<WindowGeometry>,
    displays: &[ScreenWorkArea],
) -> WindowGeometry {
    let mut geometry = saved.unwrap_or_default();
    geometry.width = geometry.width.clamp(MIN_WINDOW_SIZE, MAX_WINDOW_SIZE);
    geometry.height = geometry.height.clamp(MIN_WINDOW_SIZE, MAX_WINDOW_SIZE);
    let Some(screen) = displays
        .iter()
        .find(|screen| intersects(geometry, **screen))
        .or_else(|| displays.first())
    else {
        // Without monitor metadata, keep the fallback modest and anchored at the
        // primary-origin convention so stale dimensions cannot place it far away.
        geometry.width = geometry.width.min(WindowGeometry::default().width);
        geometry.height = geometry.height.min(WindowGeometry::default().height);
        geometry.x = 0;
        geometry.y = 0;
        return geometry;
    };
    let max_width = screen.width.max(MIN_WINDOW_SIZE);
    let max_height = screen.height.max(MIN_WINDOW_SIZE);
    geometry.width = geometry.width.min(max_width);
    geometry.height = geometry.height.min(max_height);
    let right = i64::from(screen.x) + i64::from(screen.width.saturating_sub(geometry.width));
    let bottom = i64::from(screen.y) + i64::from(screen.height.saturating_sub(geometry.height));
    geometry.x = i64::from(geometry.x).clamp(i64::from(screen.x), right) as i32;
    geometry.y = i64::from(geometry.y).clamp(i64::from(screen.y), bottom) as i32;
    geometry
}

fn intersects(geometry: WindowGeometry, screen: ScreenWorkArea) -> bool {
    let left = i64::from(geometry.x).max(i64::from(screen.x));
    let top = i64::from(geometry.y).max(i64::from(screen.y));
    let right = (i64::from(geometry.x) + i64::from(geometry.width))
        .min(i64::from(screen.x) + i64::from(screen.width));
    let bottom = (i64::from(geometry.y) + i64::from(geometry.height))
        .min(i64::from(screen.y) + i64::from(screen.height));
    right - left >= 64 && bottom - top >= 64
}

fn migrate_v1(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "schema_version".to_owned(),
            serde_json::Value::from(CURRENT_SCHEMA_VERSION),
        );
        if !object.contains_key("last_device_id") {
            if let Some(old_id) = object.remove("last_device") {
                object.insert("last_device_id".to_owned(), old_id);
            }
        }
        if !object.contains_key("last_display_id") {
            let old_display = object
                .remove("last_display")
                .unwrap_or(serde_json::Value::Null);
            object.insert("last_display_id".to_owned(), old_display);
        }
    }
    value
}

fn migrate_v2(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "schema_version".to_owned(),
            serde_json::Value::from(CURRENT_SCHEMA_VERSION),
        );
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("racc-settings-{}-{unique}", std::process::id()));
            fs::create_dir_all(&path).expect("test temp directory");
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn defaults_save_and_reload() {
        let temp = TempDir::new();
        let store = SettingsStore::new(temp.0.join("settings.json"));
        let expected = Settings::default();
        assert_eq!(store.load().expect("load defaults").settings, expected);
        store.save(&expected).expect("save");
        assert_eq!(store.load().expect("reload").settings, expected);
    }

    #[test]
    fn version_one_renames_opaque_device_id_and_persists_migration() {
        let temp = TempDir::new();
        let path = temp.0.join("settings.json");
        fs::write(
            &path,
            r#"{"schema_version":1,"last_device":"opaque-7","last_display":"display-2"}"#,
        )
        .expect("write v1");
        let outcome = SettingsStore::new(&path).load().expect("migrate");
        assert_eq!(outcome.settings.last_device_id.as_deref(), Some("opaque-7"));
        assert_eq!(
            outcome.settings.last_display_id.as_deref(),
            Some("display-2")
        );
        assert_eq!(outcome.settings.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(
            outcome.notice,
            Some(LoadNotice::Migrated {
                from: 1,
                to: CURRENT_SCHEMA_VERSION
            })
        );
        assert!(fs::read_to_string(path)
            .expect("read migrated")
            .contains("last_device_id"));
    }

    #[test]
    fn version_two_adds_disabled_autostart_preference_and_persists_migration() {
        let temp = TempDir::new();
        let path = temp.0.join("settings.json");
        fs::write(&path, r#"{"schema_version":2,"hosting_enabled":true}"#).expect("write v2");
        let outcome = SettingsStore::new(&path).load().expect("migrate");
        assert!(!outcome.settings.autostart_enabled);
        assert_eq!(outcome.settings.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(
            outcome.notice,
            Some(LoadNotice::Migrated {
                from: 2,
                to: CURRENT_SCHEMA_VERSION
            })
        );
        let persisted = fs::read_to_string(path).expect("read migrated");
        assert!(persisted.contains("\"autostart_enabled\": false"));
    }

    #[test]
    fn corrupt_file_is_quarantined_and_defaults_are_returned() {
        let temp = TempDir::new();
        let path = temp.0.join("settings.json");
        fs::write(&path, b"{definitely not json").expect("write corrupt");
        let outcome = SettingsStore::new(&path).load().expect("recover");
        assert_eq!(outcome.settings, Settings::default());
        let Some(LoadNotice::CorruptFileQuarantined { quarantined_path }) = outcome.notice else {
            panic!("corrupt-file notice");
        };
        assert!(quarantined_path.exists());
        assert!(!path.exists());
    }

    #[test]
    fn oversized_file_is_quarantined_without_reading_unbounded_bytes() {
        let temp = TempDir::new();
        let path = temp.0.join("settings.json");
        let file = File::create(&path).expect("create");
        file.set_len(MAX_SETTINGS_BYTES + 1)
            .expect("grow sparse file");
        assert!(matches!(
            SettingsStore::new(&path).load().expect("recover").notice,
            Some(LoadNotice::OversizedFileQuarantined { .. })
        ));
    }

    #[test]
    fn unknown_future_schema_is_not_modified() {
        let temp = TempDir::new();
        let path = temp.0.join("settings.json");
        let bytes = br#"{"schema_version":999,"future_field":true}"#;
        fs::write(&path, bytes).expect("write future");
        let outcome = SettingsStore::new(&path).load().expect("fallback");
        assert_eq!(outcome.notice, Some(LoadNotice::NewerSchema { found: 999 }));
        assert_eq!(fs::read(path).expect("still intact"), bytes);
    }

    #[test]
    fn saved_geometry_is_clamped_to_a_current_monitor() {
        let screens = [
            ScreenWorkArea {
                x: -1920,
                y: 0,
                width: 1920,
                height: 1080,
            },
            ScreenWorkArea {
                x: -1920,
                y: 0,
                width: 1920,
                height: 1080,
            },
        ];
        let restored = restore_geometry_for_displays(
            Some(WindowGeometry {
                x: 9000,
                y: -8000,
                width: 6000,
                height: 4000,
            }),
            &screens,
        );
        assert_eq!(
            restored,
            WindowGeometry {
                x: -1920,
                y: 0,
                width: 1920,
                height: 1080
            }
        );
        let negative_origin = restore_geometry_for_displays(
            Some(WindowGeometry {
                x: -1900,
                y: 30,
                width: 900,
                height: 700,
            }),
            &screens,
        );
        assert_eq!(negative_origin.x, -1900);
    }

    #[test]
    fn missing_display_data_uses_a_bounded_origin_fallback() {
        let restored = restore_geometry_for_displays(
            Some(WindowGeometry {
                x: i32::MAX,
                y: i32::MIN,
                width: u32::MAX,
                height: u32::MAX,
            }),
            &[],
        );
        assert_eq!(
            restored,
            WindowGeometry {
                x: 0,
                y: 0,
                width: WindowGeometry::default().width,
                height: WindowGeometry::default().height,
            }
        );
    }

    #[test]
    fn unsupported_release_hotkey_is_quarantined() {
        let temp = TempDir::new();
        let path = temp.0.join("settings.json");
        fs::write(
            &path,
            r#"{"schema_version":3,"capture_release_hotkey":"Ctrl+Alt+Shift+Unknown"}"#,
        )
        .expect("write invalid release chord");

        let outcome = SettingsStore::new(&path).load().expect("recover defaults");
        assert_eq!(outcome.settings, Settings::default());
        assert!(matches!(
            outcome.notice,
            Some(LoadNotice::CorruptFileQuarantined { .. })
        ));
        assert!(!path.exists());
    }

    #[test]
    fn invalid_settings_do_not_write() {
        let temp = TempDir::new();
        let path = temp.0.join("settings.json");
        let settings = Settings {
            last_device_id: Some("\u{1}".to_owned()),
            ..Settings::default()
        };
        assert!(matches!(
            SettingsStore::new(path.clone()).save(&settings),
            Err(StoreError::InvalidSettings(_))
        ));
        assert!(!path.exists());
    }
}
