//! Desktop UI shell driven by deterministic fake devices in `--fake` mode.
mod design;
mod focus_button;
mod live;
mod local_host;
#[cfg(any(target_os = "macos", test))]
mod mac_permissions;
#[cfg(target_os = "macos")]
mod macos_host_ipc;
pub mod settings;
mod single_instance;
mod tray;
mod view_model;
mod viewer_cursor;
mod viewer_keys;
mod viewer_pointer;
mod window_lifecycle;
#[cfg(target_os = "windows")]
mod windows_host_ipc;

use focus_button::FocusableButton;

use design::{region_widths, tokens};
use iced::advanced::{graphics::Viewport, mouse::Cursor};
use iced::time::Instant as IcedInstant;
use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, text, tooltip,
};
use iced::{Background, Border, Color, Element, Fill, Rectangle, Subscription, Task, Theme};
use live::{AppBackend, BackendEvents};
use local_host::{LocalHostCommand, LocalHostEvent, LocalHostWorker};
#[cfg(test)]
use racc_core::CoreHandle;
use racc_core::{DeviceId, FramePayload, FrameSource, QualityPreset, UiCommand, VideoFrame};
use racc_input::{
    ViewerControlMode, ViewerInputEvent, ViewerInputEvents, ViewerInputReducer, ViewerMouseInput,
};
use racc_proto::{InputEvent, InputEventKind};
#[cfg(test)]
use racc_testkit::{FakeCore, DEFAULT_FAKE_SEED};
use settings::{
    apply_current_platform_autostart, default_config_path, install_panic_log_hook,
    restore_geometry_for_displays, LoadNotice, QualityPreference, Settings, SettingsStore,
    WindowGeometry,
};
use single_instance::{ShowRequests, SingleInstance};
use std::{
    collections::{hash_map::DefaultHasher, VecDeque},
    env,
    hash::{Hash, Hasher},
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tray::{PlatformTrayController, TrayAction, TrayController};
use view_model::{Page, SessionOverlay, UserAction, ViewModel};

const TELEMETRY_PERIOD: Duration = Duration::from_millis(250);
const VIDEO_PERIOD: Duration = Duration::from_nanos(33_333_333);
const POLL_LIMIT: usize = 64;
const INPUT_DRAIN_PERIOD: Duration = Duration::from_millis(4);
const INPUT_DRAIN_PER_TICK: usize = 8;
const INPUT_QUEUE_CAPACITY: usize = 1_024;
const INPUT_QUEUE_HIGH_WATER: usize = 768;
const PENDING_UI_COMMAND_LIMIT: usize = 32;
const VIDEO_SHADER: &str = include_str!("video.wgsl");

fn window_title(_: &App) -> String {
    "Racc Connect".to_owned()
}
fn app_theme(_: &App) -> Theme {
    Theme::Dark
}

#[derive(Clone, Debug)]
enum LaunchMode {
    Discover,
    Fake {
        idle: bool,
    },
    Live {
        host_addr: SocketAddr,
        local_bind_ip: IpAddr,
    },
}

#[derive(Clone, Debug)]
struct LaunchOptions {
    mode: LaunchMode,
    telemetry_collapsed: bool,
    telemetry_expanded: bool,
    debug_latency: bool,
    measure_secs: Option<u64>,
}

fn parse_launch_options(args: impl IntoIterator<Item = String>) -> Result<LaunchOptions, String> {
    let mut args = args.into_iter();
    let mut fake = false;
    let mut fake_idle = false;
    let mut discover = false;
    let mut host_addr = None;
    let mut local_bind_ip = None;
    let mut telemetry_collapsed = false;
    let mut telemetry_expanded = false;
    let mut debug_latency = false;
    let mut measure_secs = None;

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--discover" => discover = true,
            "--fake" => fake = true,
            "--fake-idle" => fake_idle = true,
            "--telemetry-collapsed" => telemetry_collapsed = true,
            "--telemetry-expanded" => telemetry_expanded = true,
            "--debug-latency" => debug_latency = true,
            "--connect" => {
                let value = args
                    .next()
                    .ok_or("--connect requires an IP:port endpoint")?;
                let endpoint = value
                    .parse::<SocketAddr>()
                    .map_err(|_| "--connect requires a numeric IP:port endpoint")?;
                if endpoint.port() == 0 || !is_tailscale_ip(endpoint.ip()) {
                    return Err("--connect requires a Tailscale IP and nonzero port".to_owned());
                }
                if host_addr.replace(endpoint).is_some() {
                    return Err("--connect may be supplied only once".to_owned());
                }
            }
            "--bind" => {
                let value = args.next().ok_or("--bind requires a local Tailscale IP")?;
                let address = value
                    .parse::<IpAddr>()
                    .map_err(|_| "--bind requires a numeric Tailscale IP without a port")?;
                if !is_tailscale_ip(address) {
                    return Err("--bind requires a Tailscale IP".to_owned());
                }
                if local_bind_ip.replace(address).is_some() {
                    return Err("--bind may be supplied only once".to_owned());
                }
            }
            _ if argument.starts_with("--measure-secs=") => {
                let value = argument
                    .strip_prefix("--measure-secs=")
                    .and_then(|value| value.parse::<u64>().ok())
                    .ok_or("--measure-secs must be an unsigned integer")?;
                if value == 0 {
                    return Err("--measure-secs must be greater than zero".to_owned());
                }
                measure_secs = Some(value);
            }
            "--help" | "-h" => {
                return Err("Usage: racc-app [--discover] [--telemetry-collapsed|--telemetry-expanded] [--debug-latency] | --fake [--fake-idle] [--telemetry-collapsed|--telemetry-expanded] [--debug-latency] [--measure-secs=N] | --connect <Tailscale-IP:PORT> --bind <local-Tailscale-IP> [--telemetry-collapsed|--telemetry-expanded] [--debug-latency] [--measure-secs=N]".to_owned());
            }
            _ => return Err(format!("unknown option: {argument}")),
        }
    }

    let mode = if fake {
        if discover || host_addr.is_some() || local_bind_ip.is_some() {
            return Err("Choose --discover, --fake, or --connect/--bind.".to_owned());
        }
        LaunchMode::Fake { idle: fake_idle }
    } else {
        if fake_idle {
            return Err("--fake-idle is available only with --fake.".to_owned());
        }
        if telemetry_collapsed && telemetry_expanded {
            return Err("Choose either --telemetry-collapsed or --telemetry-expanded.".to_owned());
        }
        match (discover, host_addr, local_bind_ip) {
            (true, Some(_), _) | (true, _, Some(_)) => {
                return Err("Choose --discover or --connect/--bind, not both.".to_owned());
            }
            (true, None, None) | (false, None, None) => LaunchMode::Discover,
            (_, Some(host_addr), Some(local_bind_ip)) => {
                if host_addr.is_ipv4() != local_bind_ip.is_ipv4() {
                    return Err(
                        "--connect and --bind addresses must use the same IP family".to_owned()
                    );
                }
                LaunchMode::Live {
                    host_addr,
                    local_bind_ip,
                }
            }
            (_, None, Some(_)) => {
                return Err("--bind requires --connect <Tailscale-IP:PORT>".to_owned());
            }
            (_, Some(_), None) => {
                return Err("--connect requires --bind <local-Tailscale-IP>".to_owned());
            }
        }
    };
    if telemetry_collapsed && telemetry_expanded {
        return Err("Choose either --telemetry-collapsed or --telemetry-expanded.".to_owned());
    }
    if measure_secs.is_some() && !matches!(mode, LaunchMode::Fake { .. } | LaunchMode::Live { .. })
    {
        return Err("--measure-secs requires --fake or --connect/--bind.".to_owned());
    }

    Ok(LaunchOptions {
        mode,
        telemetry_collapsed,
        telemetry_expanded,
        debug_latency,
        measure_secs,
    })
}

fn is_tailscale_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => (u32::from(address) & 0xffc0_0000) == 0x6440_0000,
        IpAddr::V6(address) => {
            let segments = address.segments();
            segments[..3] == [0xfd7a, 0x115c, 0xa1e0]
        }
    }
}

fn main() -> iced::Result {
    let options = match parse_launch_options(env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            return Ok(());
        }
    };
    let (mut settings, settings_store, settings_notice) = load_app_settings();
    let geometry = restore_geometry_for_displays(
        settings.window_geometry,
        &window_lifecycle::current_screen_work_areas(),
    );
    settings.window_geometry = Some(geometry);
    if let Some(store) = &settings_store {
        if let Some(parent) = store.path().parent() {
            install_panic_log_hook(parent.join("logs"));
        }
    }
    let instance = match settings_store.as_ref() {
        Some(store) => match SingleInstance::acquire(store.path()) {
            Ok(Some(instance)) => instance,
            Ok(None) => {
                eprintln!("Racc Connect is already running; its window was restored.");
                return Ok(());
            }
            Err(_) => {
                eprintln!("Racc Connect could not start or signal its single instance.");
                return Ok(());
            }
        },
        None => {
            eprintln!("Racc Connect could not resolve per-user settings storage.");
            return Ok(());
        }
    };
    let show_requests = instance.show_requests();
    let window_settings = iced::window::Settings {
        size: iced::Size::new(geometry.width as f32, geometry.height as f32),
        position: iced::window::Position::Specific(iced::Point::new(
            geometry.x.clamp(-16_384, 16_384) as f32,
            geometry.y.clamp(-16_384, 16_384) as f32,
        )),
        min_size: Some(iced::Size::new(
            tokens::MIN_WINDOW_WIDTH,
            tokens::MIN_WINDOW_HEIGHT,
        )),
        exit_on_close_request: false,
        ..Default::default()
    };
    let app = iced::application(
        move || {
            App::new(
                options.clone(),
                settings.clone(),
                settings_store.clone(),
                settings_notice.clone(),
                show_requests.clone(),
            )
        },
        App::update,
        App::view,
    )
    .subscription(App::subscription)
    .title(window_title)
    .theme(app_theme)
    .window(window_settings);
    let result = app.run();
    drop(instance);
    result
}

fn load_app_settings() -> (Settings, Option<SettingsStore>, Option<String>) {
    let Some(path) = default_config_path() else {
        return (
            Settings::default(),
            None,
            Some(
                "Per-user settings storage is unavailable; preferences will not persist."
                    .to_owned(),
            ),
        );
    };
    let store = SettingsStore::new(path);
    match store.load() {
        Ok(outcome) => (
            outcome.settings,
            Some(store),
            outcome.notice.map(load_notice_text),
        ),
        Err(_) => (
            Settings::default(),
            Some(store),
            Some("Saved settings could not be loaded; defaults are active.".to_owned()),
        ),
    }
}

fn load_notice_text(notice: LoadNotice) -> String {
    match notice {
        LoadNotice::CorruptFileQuarantined { .. } => {
            "Saved settings were invalid and were moved aside; defaults are active.".to_owned()
        }
        LoadNotice::OversizedFileQuarantined { .. } => {
            "Saved settings exceeded the size limit and were moved aside; defaults are active."
                .to_owned()
        }
        LoadNotice::NewerSchema { .. } => {
            "Saved settings were made by a newer version; defaults are active.".to_owned()
        }
        LoadNotice::Migrated { .. } => "Saved settings were upgraded.".to_owned(),
    }
}

fn quality_from_preference(preference: QualityPreference) -> QualityPreset {
    match preference {
        QualityPreference::Auto => QualityPreset::Auto,
        QualityPreference::P480 => QualityPreset::P480,
        QualityPreference::P720 => QualityPreset::P720,
        QualityPreference::P1080 => QualityPreset::P1080,
    }
}

fn tray_connection_state(state: racc_telemetry::ConnectionState) -> settings::TrayConnectionState {
    match state {
        racc_telemetry::ConnectionState::Connecting => settings::TrayConnectionState::Connecting,
        racc_telemetry::ConnectionState::Connected => settings::TrayConnectionState::Connected,
        racc_telemetry::ConnectionState::Reconnecting => {
            settings::TrayConnectionState::Reconnecting
        }
        _ => settings::TrayConnectionState::Disconnected,
    }
}

fn preference_from_quality(quality: QualityPreset) -> QualityPreference {
    match quality {
        QualityPreset::Auto => QualityPreference::Auto,
        QualityPreset::P480 => QualityPreference::P480,
        QualityPreset::P720 => QualityPreference::P720,
        QualityPreset::P1080 => QualityPreference::P1080,
    }
}

#[derive(Debug, Clone)]
enum Message {
    Action(UserAction),
    SetCaptureReleaseHotkey(String),
    SetClipboardPreference(bool),
    #[cfg(target_os = "macos")]
    SetSwapCtrlCommand(bool),
    LocalHostAction(LocalHostCommand),
    #[cfg(target_os = "macos")]
    OpenMacSettings(mac_permissions::PermissionPane),
    TelemetryTick,
    FrameTick(IcedInstant),
    Resized(iced::Size),
    Moved(iced::Point),
    Minimized(Option<bool>),
    PhysicalKey {
        code: Option<iced::keyboard::key::Code>,
        pressed: bool,
        modifiers: iced::keyboard::Modifiers,
    },
    VideoMouse(ViewerMouseInput),
    WindowFocused(bool),
    InputDrainTick,
    ToggleFullscreen,
    WindowId(Option<iced::window::Id>),
    CloseRequested(iced::window::Id),
    NativeVisibility(Result<(), String>),
    FocusWidget(iced::advanced::widget::Id),
}

struct App {
    core: AppBackend,
    local_host_worker: Option<LocalHostWorker>,
    frame_source: Arc<dyn FrameSource>,
    model: ViewModel,
    fullscreen: bool,
    window_id: Option<iced::window::Id>,
    window_width: f32,
    last_frame_id: Option<u32>,
    last_frame_epoch: Option<u16>,
    frame_intervals_ms: Vec<f64>,
    last_frame_at: Option<Instant>,
    last_observed_frame_interval_us: Option<u64>,
    debug_latency: bool,
    measurement_started: Instant,
    measurement_ready: bool,
    measurement_wait_for_frame: bool,
    measurement_secs: Option<u64>,
    measurement_reported: bool,
    fake_idle: bool,
    tray: PlatformTrayController,
    tray_init_attempted: bool,
    tray_initialized: bool,
    settings_store: Option<SettingsStore>,
    settings: Settings,
    settings_dirty: bool,
    window_height: f32,
    window_x: f32,
    window_y: f32,
    viewer_input: Option<ViewerInputReducer>,
    input_target: Option<InputTarget>,
    window_focused: bool,
    input_queue: VecDeque<QueuedInput>,
    pending_ui_commands: VecDeque<UiCommand>,
    cursor_cache: Arc<Mutex<viewer_cursor::CursorOverlayCache>>,
    show_requests: ShowRequests,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InputTarget {
    device_id: DeviceId,
    epoch: u16,
    display_id: u32,
}

#[derive(Clone, Debug)]
struct QueuedInput {
    device_id: DeviceId,
    event: InputEvent,
}

#[derive(Debug, Eq, PartialEq)]
struct RegionCacheKeys {
    rail: u64,
    device_sidebar: u64,
    workspace: u64,
    telemetry_sidebar: u64,
}

fn hash_dependency<T: Hash>(dependency: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    dependency.hash(&mut hasher);
    hasher.finish()
}

fn event_timestamp_label(ts_us: u64) -> String {
    let total_seconds = ts_us / 1_000_000;
    let hours = total_seconds / 3_600;
    let minutes = total_seconds / 60 % 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("T+{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("T+{minutes:02}:{seconds:02}")
    }
}

fn visible_telemetry_hash(model: &ViewModel) -> u64 {
    let telemetry = &model.core.telemetry;
    let session = telemetry.session;
    let host = telemetry.host;
    let mut hasher = DefaultHasher::new();

    session.connection_state.hash(&mut hasher);
    session.path.hash(&mut hasher);
    session.last_rtt_us.hash(&mut hasher);
    session.loss_fraction.to_bits().hash(&mut hasher);
    session.frame_loss_fraction.to_bits().hash(&mut hasher);
    session.bitrate_bps.hash(&mut hasher);
    session.fps.to_bits().hash(&mut hasher);
    session.codec.hash(&mut hasher);
    session.decoder.hash(&mut hasher);
    session.epoch.hash(&mut hasher);
    host.cpu_pct_x10.hash(&mut hasher);
    host.process_cpu_pct_x10.hash(&mut hasher);
    host.capture_backend.hash(&mut hasher);
    host.encoder.hash(&mut hasher);
    host.width.hash(&mut hasher);
    host.height.hash(&mut hasher);
    host.refresh_mhz.hash(&mut hasher);
    host.target_bitrate_kbps.hash(&mut hasher);
    host.actual_bitrate_kbps.hash(&mut hasher);
    for event in telemetry.events.events().iter().rev().take(8).rev() {
        event.id.hash(&mut hasher);
        event.ts_us.hash(&mut hasher);
        event.kind.hash(&mut hasher);
        event.detail.hash(&mut hasher);
    }
    hasher.finish()
}

fn region_cache_keys(
    model: &ViewModel,
    fullscreen: bool,
    fake_idle: bool,
    debug_latency_key: Option<u64>,
) -> RegionCacheKeys {
    RegionCacheKeys {
        rail: hash_dependency(&(model.page, &model.core.devices, &model.core.selected_device)),
        device_sidebar: hash_dependency(&(
            hash_dependency(&(
                model.device_sidebar_collapsed,
                &model.core.devices,
                &model.core.selected_device,
                model.core.selected_display,
                &model.core.local_device_name,
                model.core.hosting_enabled,
                model.local_host_controls_available,
                model.local_host_connected,
                local_host_status_text(model),
                model
                    .local_host_allowlist
                    .iter()
                    .map(|peer| (&peer.node_id, &peer.display_name, &peer.login_name))
                    .collect::<Vec<_>>(),
                model
                    .local_host_pending
                    .iter()
                    .map(|peer| (&peer.node_id, &peer.display_name, &peer.login_name))
                    .collect::<Vec<_>>(),
                model.page,
            )),
            hash_dependency(&(
                model.keyboard_capture,
                model.keyboard_capture_active,
                model.mouse_capture,
                model.mouse_capture_active,
                &model.clipboard_status,
                model.clipboard_last_sync_label(),
                model.core.clipboard_session_active,
                model.core.clipboard_sync_enabled,
                &model.overlay,
            )),
        )),
        workspace: hash_dependency(&(
            (
                model.page,
                &model.core.devices,
                &model.core.selected_device,
                model.core.selected_display,
                model.stream_dimensions,
                &model.overlay,
                model.session_quality,
            ),
            (
                model.default_quality,
                &model.discovery_state,
                &model.core.local_device_name,
                model.keyboard_capture,
                model.keyboard_capture_active,
                model.core.telemetry.session.last_rtt_us,
                &model.notification,
                fullscreen,
                fake_idle,
                model.core.hosting_enabled,
                debug_latency_key,
            ),
        )),
        telemetry_sidebar: hash_dependency(&(
            model.telemetry_sidebar_collapsed,
            visible_telemetry_hash(model),
            &model.notification,
            &model.clipboard_status,
            model.clipboard_last_sync_label(),
        )),
    }
}

impl App {
    fn new(
        options: LaunchOptions,
        settings: Settings,
        settings_store: Option<SettingsStore>,
        settings_notice: Option<String>,
        show_requests: ShowRequests,
    ) -> Self {
        let discover_mode = matches!(&options.mode, LaunchMode::Discover);
        let fake_mode = matches!(&options.mode, LaunchMode::Fake { .. });
        let (core, snapshot, frame_source, fake_idle, measurement_ready, startup_error) =
            match options.mode.clone() {
                LaunchMode::Discover => match AppBackend::discover() {
                    Ok((core, snapshot, frame_source)) => {
                        (core, snapshot, frame_source, false, true, None)
                    }
                    Err(error) => {
                        let (core, snapshot, frame_source) = AppBackend::unavailable(error.clone());
                        (core, snapshot, frame_source, false, true, Some(error))
                    }
                },
                LaunchMode::Fake { idle } => {
                    let (core, snapshot, frame_source) = AppBackend::fake();
                    (core, snapshot, frame_source, idle, idle, None)
                }
                LaunchMode::Live {
                    host_addr,
                    local_bind_ip,
                } => match AppBackend::connect(host_addr, local_bind_ip) {
                    Ok((core, snapshot, frame_source)) => (
                        core,
                        snapshot,
                        frame_source,
                        false,
                        options.measure_secs.is_none(),
                        None,
                    ),
                    Err(error) => {
                        let (core, snapshot, frame_source) = AppBackend::unavailable(error.clone());
                        (core, snapshot, frame_source, false, true, Some(error))
                    }
                },
            };
        let mut model = ViewModel::new(snapshot);
        if discover_mode {
            model.page = Page::Home;
            model.begin_discovery();
        }
        model.default_quality = quality_from_preference(settings.default_quality);
        model.session_quality = model.default_quality;
        model.device_sidebar_collapsed = settings.device_sidebar_collapsed;
        model.telemetry_sidebar_collapsed = !options.telemetry_expanded
            && (options.telemetry_collapsed || settings.telemetry_sidebar_collapsed);
        model.autostart_enabled = settings.autostart_enabled;
        model.local_host_controls_available = !fake_mode;
        model.core.hosting_enabled = fake_mode && settings.hosting_enabled;
        let local_host_worker = (!fake_mode).then(LocalHostWorker::start);
        if let Some(error) = startup_error {
            model.overlay = SessionOverlay::Error(error.clone());
            if discover_mode {
                model.set_discovery_error(error.clone());
            }
            model.notification = Some(error);
        } else if let Some(notice) = settings_notice {
            model.notification = Some(notice);
        }
        let tray = PlatformTrayController::new();
        Self {
            core,
            local_host_worker,
            show_requests,
            frame_source,
            model,
            fullscreen: false,
            window_id: None,
            window_width: settings
                .window_geometry
                .map_or(1_280.0, |geometry| geometry.width as f32),
            window_height: settings
                .window_geometry
                .map_or(800.0, |geometry| geometry.height as f32),
            window_x: settings
                .window_geometry
                .map_or(80.0, |geometry| geometry.x as f32),
            window_y: settings
                .window_geometry
                .map_or(80.0, |geometry| geometry.y as f32),
            tray_init_attempted: false,
            tray_initialized: false,
            settings_store,
            settings,
            settings_dirty: false,
            last_frame_id: None,
            last_frame_epoch: None,
            frame_intervals_ms: Vec::with_capacity(4_096),
            last_frame_at: None,
            last_observed_frame_interval_us: None,
            debug_latency: options.debug_latency,
            measurement_started: Instant::now(),
            measurement_ready,
            measurement_wait_for_frame: matches!(options.mode, LaunchMode::Live { .. })
                && options.measure_secs.is_some(),
            measurement_secs: options.measure_secs,
            measurement_reported: false,
            fake_idle,
            tray,
            viewer_input: None,
            input_target: None,
            window_focused: false,
            input_queue: VecDeque::with_capacity(INPUT_QUEUE_CAPACITY),
            pending_ui_commands: VecDeque::with_capacity(PENDING_UI_COMMAND_LIMIT),
            cursor_cache: Arc::new(Mutex::new(viewer_cursor::CursorOverlayCache::default())),
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Action(action) => {
                let hide = matches!(&action, UserAction::SetVisible(false));
                let toggled_clipboard_preference =
                    self.core.is_live() && matches!(&action, UserAction::ToggleClipboardSync);
                self.apply_action(action);
                if toggled_clipboard_preference {
                    self.settings.clipboard_enabled = self.model.core.clipboard_sync_enabled;
                    self.settings_dirty = true;
                }
                self.sync_settings_from_model();
                self.tray.set_hosting(self.model.core.hosting_enabled);
                self.persist_settings_if_dirty();
                if hide {
                    return self.hide_to_tray();
                }
            }
            Message::SetCaptureReleaseHotkey(choice) => {
                if !settings::CAPTURE_RELEASE_HOTKEY_OPTIONS.contains(&choice.as_str()) {
                    self.model.notification =
                        Some("Choose one of the supported local release chords.".to_owned());
                } else {
                    let chord = viewer_keys::release_chord_from_setting(&choice);
                    let result = self
                        .viewer_input
                        .as_mut()
                        .map(|reducer| reducer.set_release_chord(chord));
                    let can_persist = match result {
                        Some(Ok(events)) => {
                            self.apply_viewer_input_events(events);
                            true
                        }
                        Some(Err(_)) => {
                            self.model.notification = Some(
                                "Held remote input could not be released while changing the chord."
                                    .to_owned(),
                            );
                            false
                        }
                        None => true,
                    };
                    if can_persist {
                        self.settings.capture_release_hotkey = choice;
                        self.settings_dirty = true;
                        self.persist_settings_if_dirty();
                    }
                }
            }
            Message::SetClipboardPreference(enabled) => {
                self.settings.clipboard_enabled = enabled;
                self.settings_dirty = true;
                self.persist_settings_if_dirty();
            }
            #[cfg(target_os = "macos")]
            Message::SetSwapCtrlCommand(enabled) => {
                if self.model.keyboard_capture || self.model.mouse_capture {
                    self.model.keyboard_capture = false;
                    self.model.mouse_capture = false;
                    self.release_input_devices();
                    self.sync_viewer_input_state();
                }
                self.settings.swap_ctrl_command = enabled;
                self.settings_dirty = true;
                self.persist_settings_if_dirty();
            }
            Message::LocalHostAction(command) => self.queue_local_host_action(command),
            #[cfg(target_os = "macos")]
            Message::OpenMacSettings(pane) => {
                self.model.notification = mac_permissions::open_settings(pane).err().map(|error| {
                    format!("Could not open the requested macOS privacy pane: {error}")
                });
            }
            Message::TelemetryTick => {
                if self.window_id.is_some() && self.show_requests.take_pending() {
                    return self.restore_window();
                }
                self.poll_local_host_events();
                self.initialize_tray_if_ready();
                self.tray.set_hosting(self.model.core.hosting_enabled);
                self.tray.set_connection_state(tray_connection_state(
                    self.model.core.telemetry.session.connection_state,
                ));
                if let Some(action) = self.tray.poll_action() {
                    match action {
                        TrayAction::ShowWindow => return self.restore_window(),
                        TrayAction::ToggleHosting => {
                            self.apply_action(UserAction::SetHosting(
                                !self.model.core.hosting_enabled,
                            ));
                            self.sync_settings_from_model();
                            self.tray.set_hosting(self.model.core.hosting_enabled);
                            self.persist_settings_if_dirty();
                        }
                        TrayAction::Quit => {
                            self.persist_settings_if_dirty();
                            return iced::exit();
                        }
                    }
                }
                match self.core.poll_events(POLL_LIMIT) {
                    Ok(BackendEvents::Core(events)) => {
                        for event in events {
                            self.model.apply_event(event);
                        }
                    }
                    Ok(BackendEvents::Viewer {
                        device_id,
                        events,
                        telemetry,
                        clipboard_metadata,
                    }) => {
                        for event in events {
                            self.apply_viewer_event(&device_id, event);
                        }
                        if let Some(telemetry) = telemetry {
                            self.model.apply_live_telemetry(*telemetry);
                        }
                        for metadata in clipboard_metadata {
                            self.model.apply_clipboard_metadata(metadata);
                        }
                    }
                    Ok(BackendEvents::Empty) => {}
                    Err(error) => self.model.notification = Some(error),
                }
                if let Some(snapshot) = self.core.snapshot() {
                    match snapshot {
                        Ok(snapshot) => self.model.apply_snapshot(snapshot),
                        Err(error) => self.model.notification = Some(error),
                    }
                }
                if !self.model.local_host_controls_available {
                    self.model.core.hosting_enabled = self.settings.hosting_enabled;
                }
                self.sync_viewer_input_state();
                self.sync_cursor_target();
                self.drain_input_queue();
                self.sync_settings_from_model();
                self.persist_settings_if_dirty();

                if self.measurement_ready
                    && self.measurement_secs.is_some_and(|limit| {
                        self.measurement_started.elapsed() >= Duration::from_secs(limit)
                    })
                    && !self.measurement_reported
                {
                    self.measurement_reported = true;
                    self.print_measurement();
                    return iced::exit();
                }
                let mut tasks = vec![iced::window::oldest().map(Message::WindowId)];
                if let Some(id) = self.window_id {
                    tasks.push(iced::window::is_minimized(id).map(Message::Minimized));
                }
                return Task::batch(tasks);
            }
            Message::FrameTick(now) => {
                if !self.measurement_ready && !self.measurement_wait_for_frame {
                    self.measurement_started = Instant::now();
                    self.measurement_ready = true;
                }
                let elapsed = now
                    .saturating_duration_since(self.measurement_started)
                    .as_micros()
                    .min(u128::from(u64::MAX)) as u64;
                if !self.fake_idle {
                    self.core.advance_to(elapsed);
                }
                if !self.fake_idle {
                    if let Some(frame) = self.frame_source.latest_frame() {
                        self.model.record_frame(frame.frame_id);
                        if self.last_frame_id != Some(frame.frame_id)
                            || self.last_frame_epoch != Some(frame.epoch)
                        {
                            let now = Instant::now();
                            if self.measurement_wait_for_frame && !self.measurement_ready {
                                self.measurement_started = now;
                                self.measurement_ready = true;
                                self.frame_intervals_ms.clear();
                                self.last_frame_at = Some(now);
                            } else if let Some(previous) = self.last_frame_at {
                                let interval_us =
                                    u64::try_from(now.duration_since(previous).as_micros())
                                        .unwrap_or(u64::MAX);
                                self.last_observed_frame_interval_us = Some(interval_us);
                                self.frame_intervals_ms.push(interval_us as f64 / 1_000.0);
                                if self.frame_intervals_ms.len() > 8_192 {
                                    self.frame_intervals_ms.remove(0);
                                }
                            }
                            self.last_frame_at = Some(now);
                            self.last_frame_id = Some(frame.frame_id);
                            self.last_frame_epoch = Some(frame.epoch);
                        }
                    }
                }
            }
            Message::WindowId(id) => self.window_id = id,
            Message::Resized(size) => {
                self.window_width = size.width;
                self.window_height = size.height;
                self.capture_window_geometry();
            }
            Message::Moved(position) => {
                self.window_x = position.x;
                self.window_y = position.y;
                self.capture_window_geometry();
            }
            Message::Minimized(minimized) => {
                if minimized == Some(true) && self.model.core.visible {
                    self.apply_action(UserAction::SetVisible(false));
                } else if minimized == Some(false) && !self.model.core.visible {
                    self.apply_action(UserAction::SetVisible(true));
                }
            }
            Message::PhysicalKey {
                code,
                pressed,
                modifiers,
            } => return self.handle_physical_key(code, pressed, modifiers),
            Message::VideoMouse(input) => self.handle_video_mouse(input),
            Message::WindowFocused(focused) => {
                self.window_focused = focused;
                self.sync_viewer_input_state();
            }
            Message::InputDrainTick => self.drain_input_queue(),
            Message::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
                let mode = if self.fullscreen {
                    iced::window::Mode::Fullscreen
                } else {
                    iced::window::Mode::Windowed
                };
                return self
                    .window_id
                    .map_or_else(Task::none, |id| iced::window::set_mode(id, mode));
            }
            Message::FocusWidget(id) => return iced::widget::operation::focus(id),
            Message::CloseRequested(id) => {
                self.window_id = Some(id);
                self.apply_action(UserAction::SetVisible(false));
                return self.hide_to_tray();
            }
            Message::NativeVisibility(result) => match result {
                Ok(()) if self.model.core.visible => {
                    return self
                        .window_id
                        .map_or_else(Task::none, iced::window::gain_focus);
                }
                Ok(()) => {}
                Err(error) => {
                    self.model.notification = Some(format!(
                        "The window could not be hidden or restored natively ({error}); using the taskbar or Dock instead."
                    ));
                    if let Some(id) = self.window_id {
                        return if self.model.core.visible {
                            Task::batch([
                                iced::window::minimize(id, false),
                                iced::window::gain_focus(id),
                            ])
                        } else {
                            iced::window::minimize(id, true)
                        };
                    }
                }
            },
        }
        Task::none()
    }

    fn initialize_tray_if_ready(&mut self) {
        if self.tray_init_attempted || self.window_id.is_none() {
            return;
        }
        self.tray_init_attempted = true;
        self.tray_initialized = self
            .tray
            .initialize(
                self.model.core.hosting_enabled,
                tray_connection_state(self.model.core.telemetry.session.connection_state),
            )
            .is_ok();
        if !self.tray_initialized {
            self.model.notification = Some(
                "The system tray could not be initialized; the app remains available from the taskbar or Dock.".to_owned(),
            );
        }
    }

    fn hide_to_tray(&mut self) -> Task<Message> {
        self.initialize_tray_if_ready();
        let Some(id) = self.window_id else {
            return Task::none();
        };
        if !self.tray_initialized {
            return iced::window::minimize(id, true);
        }
        iced::window::run(id, |window| window_lifecycle::set_visible(window, false))
            .map(Message::NativeVisibility)
    }

    fn restore_window(&mut self) -> Task<Message> {
        self.apply_action(UserAction::SetVisible(true));
        let Some(id) = self.window_id else {
            return Task::none();
        };
        if !self.tray_initialized {
            return Task::batch([
                iced::window::minimize(id, false),
                iced::window::gain_focus(id),
            ]);
        }
        iced::window::run(id, |window| window_lifecycle::set_visible(window, true))
            .map(Message::NativeVisibility)
    }

    fn sync_settings_from_model(&mut self) {
        let before = self.settings.clone();
        self.settings.default_quality = preference_from_quality(self.model.default_quality);
        self.settings.device_sidebar_collapsed = self.model.device_sidebar_collapsed;
        self.settings.telemetry_sidebar_collapsed = self.model.telemetry_sidebar_collapsed;
        self.settings.hosting_enabled = self.model.core.hosting_enabled;
        self.settings.autostart_enabled = self.model.autostart_enabled;
        if let Some(device_id) = &self.model.core.selected_device {
            self.settings.last_device_id = Some(device_id.as_str().to_owned());
        }
        if let Some(display_id) = self.model.core.selected_display {
            self.settings.last_display_id = Some(display_id.get().to_string());
        }
        self.settings_dirty |= self.settings != before;
    }

    fn capture_window_geometry(&mut self) {
        if self.fullscreen
            || !self.window_x.is_finite()
            || !self.window_y.is_finite()
            || !self.window_width.is_finite()
            || !self.window_height.is_finite()
        {
            return;
        }
        let geometry = WindowGeometry {
            x: self.window_x.round().clamp(-16_384.0, 16_384.0) as i32,
            y: self.window_y.round().clamp(-16_384.0, 16_384.0) as i32,
            width: self.window_width.round().clamp(320.0, 8_192.0) as u32,
            height: self.window_height.round().clamp(320.0, 8_192.0) as u32,
        };
        if self.settings.window_geometry != Some(geometry) {
            self.settings.window_geometry = Some(geometry);
            self.settings_dirty = true;
        }
    }

    fn persist_settings_if_dirty(&mut self) {
        if !self.settings_dirty {
            return;
        }
        self.settings_dirty = false;
        match &self.settings_store {
            Some(store) if store.save(&self.settings).is_ok() => {}
            _ => {
                self.model.notification = Some(
                    "App preferences could not be saved; the last saved settings remain active."
                        .to_owned(),
                );
            }
        }
    }

    fn apply_action(&mut self, action: UserAction) {
        if let UserAction::SetAutostart(enabled) = &action {
            let enabled = *enabled;
            match apply_current_platform_autostart(enabled) {
                Ok(()) => {
                    let _ = self.model.reduce_action(UserAction::SetAutostart(enabled));
                    self.settings.autostart_enabled = enabled;
                    self.settings_dirty = true;
                    self.model.notification = Some(if enabled {
                        "This app will start when the current user signs in.".to_owned()
                    } else {
                        "This app will no longer start automatically.".to_owned()
                    });
                }
                Err(_) => {
                    self.model.notification =
                        Some("The current-user autostart setting could not be changed.".to_owned());
                }
            }
            return;
        }
        if let UserAction::SetHosting(enabled) = &action {
            if self.model.local_host_controls_available {
                self.queue_local_host_action(LocalHostCommand::SetHostingEnabled(*enabled));
                return;
            }
        }
        if self.model.local_host_controls_available {
            let host_command = match &action {
                UserAction::ApprovePeer(node_id) => {
                    Some(LocalHostCommand::Approve(node_id.as_str().to_owned()))
                }
                UserAction::RejectPeer(node_id) => {
                    Some(LocalHostCommand::Reject(node_id.as_str().to_owned()))
                }
                UserAction::RemovePeer(node_id) => {
                    Some(LocalHostCommand::Remove(node_id.as_str().to_owned()))
                }
                _ => None,
            };
            if let Some(command) = host_command {
                self.queue_local_host_action(command);
                return;
            }
        }
        let live = self.core.is_live();
        if live
            && matches!(
                &action,
                UserAction::ToggleRemoteDesktop
                    | UserAction::SetKeyboardCapture(true)
                    | UserAction::SetMouseCapture(true)
            )
            && !self.live_input_ready()
        {
            self.model.notification =
                Some("Remote capture is available after the video stream is ready.".to_owned());
            return;
        }
        if live
            && matches!(
                &action,
                UserAction::SetKeyboardCapture(false) | UserAction::SetMouseCapture(false)
            )
        {
            self.release_input_devices();
        }
        let disconnected = live && matches!(&action, UserAction::Disconnect);
        let commands = self.model.reduce_action(action);
        self.sync_viewer_input_state();
        self.sync_cursor_target();
        for command in commands {
            if live
                && matches!(
                    &command,
                    UiCommand::ToggleKeyboardCapture(_) | UiCommand::ToggleMouseCapture(_)
                )
            {
                continue;
            }
            self.send_or_defer_ui_command(command);
        }
        if disconnected {
            self.model.overlay = SessionOverlay::Error(
                "Session closed. Use --connect to start another session.".to_owned(),
            );
            self.sync_viewer_input_state();
        }
    }

    fn queue_local_host_action(&mut self, command: LocalHostCommand) {
        if !self.model.local_host_controls_available {
            self.model.notification =
                Some("Local host-agent controls are unavailable in fake mode.".to_owned());
            return;
        }
        if !self.model.local_host_connected {
            self.model.notification = Some(
                self.model
                    .local_host_error
                    .clone()
                    .unwrap_or_else(|| "The local host-agent is not connected.".to_owned()),
            );
            return;
        }
        match self
            .local_host_worker
            .as_ref()
            .map(|worker| worker.send(command))
        {
            Some(Ok(())) => self.model.notification = None,
            Some(Err(error)) => self.model.notification = Some(error.to_owned()),
            None => {
                self.model.notification =
                    Some("The local host-agent worker is unavailable.".to_owned())
            }
        }
    }

    fn poll_local_host_events(&mut self) {
        let mut received = Vec::new();
        if let Some(worker) = &self.local_host_worker {
            while let Ok(event) = worker.try_recv() {
                received.push(event);
            }
        }
        for event in received {
            match event {
                LocalHostEvent::Snapshot(snapshot) => {
                    let previous_error = self.model.local_host_error.clone();
                    self.model.apply_local_host_snapshot(snapshot);
                    self.tray.set_hosting(self.model.core.hosting_enabled);
                    if previous_error != self.model.local_host_error
                        && self.model.local_host_error.is_some()
                    {
                        self.model.notification = self.model.local_host_error.clone();
                    }
                    self.sync_settings_from_model();
                    self.persist_settings_if_dirty();
                }
                LocalHostEvent::PendingSnapshot(peers) => {
                    self.model.apply_local_host_pending_snapshot(peers);
                }
                LocalHostEvent::PendingEvent(event) => {
                    self.model.apply_local_host_pending_event(event);
                }
                LocalHostEvent::CommandFinished { result, .. } => {
                    self.model.notification = Some(match result {
                        Ok(message) | Err(message) => message,
                    });
                }
            }
        }
    }

    fn apply_viewer_event(&mut self, device_id: &DeviceId, event: racc_core::ViewerRuntimeEvent) {
        match event {
            racc_core::ViewerRuntimeEvent::CursorShape(shape) => {
                let accepted = self
                    .cursor_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert_shape(shape);
                if !accepted {
                    self.model.notification =
                        Some("The remote cursor shape was rejected.".to_owned());
                }
            }
            racc_core::ViewerRuntimeEvent::CursorPosition(position) => {
                let _ = self
                    .cursor_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .set_position(position);
            }
            event => {
                if matches!(
                    &event,
                    racc_core::ViewerRuntimeEvent::Connecting
                        | racc_core::ViewerRuntimeEvent::Connected(_)
                        | racc_core::ViewerRuntimeEvent::Disconnected
                        | racc_core::ViewerRuntimeEvent::Failed(_)
                        | racc_core::ViewerRuntimeEvent::Session(
                            racc_session::SessionEvent::SessionEnded
                                | racc_session::SessionEvent::ControlTimedOut,
                        )
                ) {
                    self.cursor_cache
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clear_session();
                }
                let enable_saved_clipboard = self.settings.clipboard_enabled
                    && matches!(&event, racc_core::ViewerRuntimeEvent::Connected(_));
                self.model.apply_viewer_event(device_id, event);
                if enable_saved_clipboard && self.model.core.clipboard_session_active {
                    match self.core.send(UiCommand::SetClipboardEnabled(true)) {
                        Ok(()) => self.model.core.clipboard_sync_enabled = true,
                        Err(error) => {
                            self.model.notification = Some(format!(
                                "Saved clipboard sync could not be enabled for this session: {error}"
                            ));
                        }
                    }
                }
            }
        }
    }

    fn sync_cursor_target(&self) {
        let target = if self.model.core.visible
            && self.model.core.telemetry.session.connection_state
                == racc_telemetry::ConnectionState::Connected
            && matches!(&self.model.overlay, SessionOverlay::None)
        {
            self.model
                .core
                .selected_device
                .as_ref()
                .zip(self.model.core.selected_display)
                .and_then(|(device_id, display_id)| {
                    let device = self
                        .model
                        .core
                        .devices
                        .iter()
                        .find(|device| &device.id == device_id)?;
                    let display = device
                        .displays
                        .iter()
                        .find(|display| display.id == display_id)?;
                    (display.available && display.width_px > 0 && display.height_px > 0).then_some(
                        viewer_cursor::CursorTarget {
                            epoch: self.model.core.telemetry.session.epoch,
                            display_id: display.id.get(),
                            display_width: display.width_px,
                            display_height: display.height_px,
                        },
                    )
                })
        } else {
            None
        };
        self.cursor_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_target(target);
    }

    fn live_input_ready(&self) -> bool {
        self.core.is_live()
            && self.model.core.visible
            && self.model.core.selected_device.is_some()
            && self.model.core.selected_display.is_some()
            && self.model.core.telemetry.session.connection_state
                == racc_telemetry::ConnectionState::Connected
            && matches!(&self.model.overlay, SessionOverlay::None)
    }

    fn sync_viewer_input_state(&mut self) {
        if !self.core.is_live() {
            self.model.keyboard_capture_active = false;
            self.model.mouse_capture_active = false;
            return;
        }

        let next_target = self
            .model
            .core
            .selected_device
            .clone()
            .zip(self.model.core.selected_display)
            .map(|(device_id, display_id)| InputTarget {
                device_id,
                epoch: self.model.core.telemetry.session.epoch,
                display_id: display_id.get(),
            });

        if self.input_target != next_target {
            let target_change = if let Some(reducer) = self.viewer_input.as_mut() {
                if let Some(target) = &next_target {
                    Some(reducer.set_stream_target(target.epoch, target.display_id))
                } else {
                    Some(reducer.set_mode(ViewerControlMode::ViewOnly))
                }
            } else {
                None
            };
            if let Some(result) = target_change {
                match result {
                    Ok(events) => self.apply_viewer_input_events(events),
                    Err(_) => {
                        self.model.notification = Some(
                            "Keyboard capture was released after a stream target change."
                                .to_owned(),
                        );
                    }
                }
            }
            self.input_target = next_target.clone();
            if next_target.is_some() && self.viewer_input.is_none() {
                if let Some(target) = &next_target {
                    match ViewerInputReducer::new(target.epoch, target.display_id) {
                        Ok(mut reducer) => {
                            let chord = viewer_keys::release_chord_from_setting(
                                &self.settings.capture_release_hotkey,
                            );
                            if reducer.set_release_chord(chord).is_ok() {
                                self.viewer_input = Some(reducer);
                            } else {
                                self.model.notification = Some(
                                    "The local release chord could not be initialized.".to_owned(),
                                );
                            }
                        }
                        Err(_) => {
                            self.model.keyboard_capture = false;
                            self.model.notification = Some(
                                "Keyboard capture could not initialize for this display."
                                    .to_owned(),
                            );
                        }
                    }
                }
            } else if next_target.is_none() {
                self.viewer_input = None;
            }
        }

        let connected = self.model.core.telemetry.session.connection_state
            == racc_telemetry::ConnectionState::Connected;
        let visible = self.model.core.visible;
        let paused = !matches!(&self.model.overlay, SessionOverlay::None);
        let focused = self.window_focused;
        let mode = if self.model.keyboard_capture || self.model.mouse_capture {
            ViewerControlMode::RemoteDesktop
        } else {
            ViewerControlMode::ViewOnly
        };
        let state_events = if let Some(reducer) = self.viewer_input.as_mut() {
            vec![
                reducer.set_connected(connected),
                reducer.set_visible(visible),
                reducer.set_paused(paused),
                reducer.set_focused(focused),
                reducer.set_mode(mode),
            ]
        } else {
            Vec::new()
        };
        for result in state_events {
            match result {
                Ok(events) => self.apply_viewer_input_events(events),
                Err(_) => {
                    self.model.keyboard_capture = false;
                    self.model.keyboard_capture_active = false;
                    self.model.notification = Some(
                        "Keyboard capture was disabled after an invalid session state.".to_owned(),
                    );
                }
            }
        }
        let reducer_active = self
            .viewer_input
            .as_ref()
            .is_some_and(ViewerInputReducer::is_capturing);
        self.model.keyboard_capture_active = reducer_active && self.model.keyboard_capture;
        self.model.mouse_capture_active = reducer_active && self.model.mouse_capture;
    }

    fn apply_viewer_input_events(&mut self, events: ViewerInputEvents) {
        for event in events.into_vec() {
            match event {
                ViewerInputEvent::Input(event) => {
                    if let Some(target) = &self.input_target {
                        let same_target = self.input_queue.back().is_some_and(|queued| {
                            queued.device_id == target.device_id
                                && queued.event.epoch == event.epoch
                                && queued.event.display_id == event.display_id
                        });
                        if same_target
                            && matches!(&event.event, InputEventKind::MouseMoveAbs { .. })
                            && self.input_queue.back().is_some_and(|queued| {
                                matches!(&queued.event.event, InputEventKind::MouseMoveAbs { .. })
                            })
                        {
                            if let Some(queued) = self.input_queue.back_mut() {
                                queued.event = event;
                            }
                        } else if self.input_queue.len() < INPUT_QUEUE_CAPACITY {
                            self.input_queue.push_back(QueuedInput {
                                device_id: target.device_id.clone(),
                                event,
                            });
                        } else {
                            self.model.keyboard_capture = false;
                            self.model.keyboard_capture_active = false;
                            self.model.mouse_capture = false;
                            self.model.mouse_capture_active = false;
                            self.model.notification = Some(
                                "Remote capture paused because its bounded input queue is full."
                                    .to_owned(),
                            );
                        }
                    }
                }
                ViewerInputEvent::CaptureChanged { captured } => {
                    self.model.keyboard_capture_active = captured && self.model.keyboard_capture;
                    self.model.mouse_capture_active = captured && self.model.mouse_capture;
                }
                ViewerInputEvent::ReleaseChordTriggered => {
                    self.model.keyboard_capture = false;
                    self.model.keyboard_capture_active = false;
                    self.model.mouse_capture = false;
                    self.model.mouse_capture_active = false;
                    self.model.notification =
                        Some("Remote capture released by the emergency shortcut.".to_owned());
                }
            }
        }
    }

    fn handle_physical_key(
        &mut self,
        code: Option<iced::keyboard::key::Code>,
        pressed: bool,
        modifiers: iced::keyboard::Modifiers,
    ) -> Task<Message> {
        let is_tab = code == Some(iced::keyboard::key::Code::Tab);
        let capturing = self
            .viewer_input
            .as_ref()
            .is_some_and(ViewerInputReducer::is_capturing);
        let keyboard_capturing = capturing && self.model.keyboard_capture;
        let chord_candidate = (self.model.keyboard_capture || self.model.mouse_capture)
            && code == Some(iced::keyboard::key::Code::Escape);

        if self.core.is_live() && (keyboard_capturing || chord_candidate) {
            if pressed && self.input_queue.len() >= INPUT_QUEUE_HIGH_WATER {
                self.model.keyboard_capture = false;
                self.model.mouse_capture = false;
                if let Some(reducer) = self.viewer_input.as_mut() {
                    if let Ok(events) = reducer.set_mode(ViewerControlMode::ViewOnly) {
                        self.apply_viewer_input_events(events);
                    }
                }
                self.model.notification = Some(
                    "Remote capture was released because the input queue is backed up.".to_owned(),
                );
                return Task::none();
            }
            if let Some(code) = code {
                let swap_ctrl_command =
                    cfg!(target_os = "macos") && self.settings.swap_ctrl_command;
                let result = self.viewer_input.as_mut().and_then(|reducer| {
                    viewer_keys::reduce_physical_key_with_swap(
                        reducer,
                        code,
                        pressed,
                        modifiers,
                        swap_ctrl_command,
                    )
                });
                match result {
                    Some(Ok(events)) => self.apply_viewer_input_events(events),
                    Some(Err(_)) => {
                        self.model.notification = Some(
                            "The physical key could not be forwarded by this session.".to_owned(),
                        );
                    }
                    None if keyboard_capturing => {
                        self.model.notification = Some(
                            "This physical key is not supported for remote capture.".to_owned(),
                        );
                    }
                    None => {}
                }
            } else if keyboard_capturing {
                self.model.notification =
                    Some("This physical key is not supported for remote capture.".to_owned());
            }
            return Task::none();
        }

        if pressed && is_tab {
            return if modifiers.shift() {
                iced::widget::operation::focus_previous()
            } else {
                iced::widget::operation::focus_next()
            };
        }
        Task::none()
    }

    fn handle_video_mouse(&mut self, input: ViewerMouseInput) {
        if !self.core.is_live() || !self.model.mouse_capture_active {
            return;
        }
        if self.input_queue.len() >= INPUT_QUEUE_HIGH_WATER {
            self.model.keyboard_capture = false;
            self.model.mouse_capture = false;
            if let Some(reducer) = self.viewer_input.as_mut() {
                if let Ok(events) = reducer.set_mode(ViewerControlMode::ViewOnly) {
                    self.apply_viewer_input_events(events);
                }
            }
            self.model.notification = Some(
                "Remote capture was released because the input queue is backed up.".to_owned(),
            );
            return;
        }
        let result = self
            .viewer_input
            .as_mut()
            .map(|reducer| reducer.mouse_event(input));
        match result {
            Some(Ok(events)) => self.apply_viewer_input_events(events),
            Some(Err(_)) => {
                self.model.notification =
                    Some("The pointer event could not be forwarded by this session.".to_owned());
            }
            None => {}
        }
    }

    fn release_input_devices(&mut self) {
        let events = self
            .viewer_input
            .as_mut()
            .map(|reducer| reducer.set_mode(ViewerControlMode::ViewOnly));
        if let Some(Ok(events)) = events {
            self.apply_viewer_input_events(events);
        }
    }

    fn send_or_defer_ui_command(&mut self, command: UiCommand) {
        if self.core.is_live()
            && (!self.input_queue.is_empty() || !self.pending_ui_commands.is_empty())
        {
            if self.pending_ui_commands.len() < PENDING_UI_COMMAND_LIMIT {
                self.pending_ui_commands.push_back(command);
            } else {
                self.model.notification = Some(
                    "A remote control action is waiting for the input queue to drain.".to_owned(),
                );
            }
            return;
        }
        let is_discovery = matches!(&command, UiCommand::DiscoverDevices);
        if let Err(error) = self.core.send(command) {
            if is_discovery {
                self.model.set_discovery_error(error.clone());
            }
            self.model.notification = Some(error);
        }
    }

    fn drain_input_queue(&mut self) {
        if self.core.is_live()
            && self.model.core.telemetry.session.connection_state
                != racc_telemetry::ConnectionState::Connected
        {
            self.input_queue.clear();
        }

        for _ in 0..INPUT_DRAIN_PER_TICK {
            let Some(queued) = self.input_queue.front().cloned() else {
                break;
            };
            match self.core.send_input(&queued.device_id, queued.event) {
                Ok(true) => {
                    self.input_queue.pop_front();
                }
                Ok(false) => break,
                Err(_) => {
                    self.input_queue.clear();
                    self.model.keyboard_capture = false;
                    self.model.keyboard_capture_active = false;
                    self.model.mouse_capture = false;
                    self.model.mouse_capture_active = false;
                    if let Some(reducer) = self.viewer_input.as_mut() {
                        let _ = reducer.set_mode(ViewerControlMode::ViewOnly);
                    }
                    self.model.notification = Some(
                        "Remote input could not be delivered; capture was released.".to_owned(),
                    );
                    break;
                }
            }
        }

        if self.input_queue.is_empty() {
            for _ in 0..INPUT_DRAIN_PER_TICK {
                let Some(command) = self.pending_ui_commands.pop_front() else {
                    break;
                };
                if let Err(error) = self.core.send(command) {
                    self.model.notification = Some(error);
                }
            }
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            iced::time::every(TELEMETRY_PERIOD).map(|_| Message::TelemetryTick),
            iced::window::resize_events().map(|(_, size)| Message::Resized(size)),
            iced::event::listen_with(|event, _status, id| match event {
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    physical_key,
                    modifiers,
                    ..
                }) => Some(Message::PhysicalKey {
                    code: match physical_key {
                        iced::keyboard::key::Physical::Code(code) => Some(code),
                        iced::keyboard::key::Physical::Unidentified(_) => None,
                    },
                    pressed: true,
                    modifiers,
                }),
                iced::Event::Keyboard(iced::keyboard::Event::KeyReleased {
                    physical_key,
                    modifiers,
                    ..
                }) => Some(Message::PhysicalKey {
                    code: match physical_key {
                        iced::keyboard::key::Physical::Code(code) => Some(code),
                        iced::keyboard::key::Physical::Unidentified(_) => None,
                    },
                    pressed: false,
                    modifiers,
                }),
                iced::Event::Window(iced::window::Event::Focused) => {
                    Some(Message::WindowFocused(true))
                }
                iced::Event::Window(iced::window::Event::Unfocused) => {
                    Some(Message::WindowFocused(false))
                }
                iced::Event::Window(iced::window::Event::Moved(position)) => {
                    Some(Message::Moved(position))
                }
                iced::Event::Window(iced::window::Event::CloseRequested) => {
                    Some(Message::CloseRequested(id))
                }
                _ => None,
            }),
        ];
        if !self.fake_idle && self.model.core.visible {
            subscriptions.push(iced::time::every(VIDEO_PERIOD).map(Message::FrameTick));
        }
        if !self.input_queue.is_empty() || !self.pending_ui_commands.is_empty() {
            subscriptions
                .push(iced::time::every(INPUT_DRAIN_PERIOD).map(|_| Message::InputDrainTick));
        }
        Subscription::batch(subscriptions)
    }

    fn view(&self) -> Element<'_, Message> {
        let widths = region_widths(
            self.window_width,
            self.model.device_sidebar_collapsed,
            self.model.telemetry_sidebar_collapsed,
        );
        let debug_latency_key = self.debug_latency.then(|| {
            let frame_key = self.frame_source.latest_frame().map(|frame| {
                (
                    frame.epoch,
                    frame.frame_id,
                    frame.capture_ts_us,
                    frame.decode_duration_us,
                )
            });
            hash_dependency(&(frame_key, self.last_observed_frame_interval_us))
        });
        let keys = region_cache_keys(
            &self.model,
            self.fullscreen,
            self.fake_idle,
            debug_latency_key,
        );
        row![
            iced::widget::lazy(keys.rail, |_| self.device_rail()),
            iced::widget::lazy(keys.device_sidebar, move |_| {
                self.device_sidebar().width(widths.device_sidebar)
            }),
            iced::widget::lazy(keys.workspace, |_| self.workspace().width(Fill)),
            iced::widget::lazy(keys.telemetry_sidebar, move |_| {
                self.telemetry_sidebar().width(widths.telemetry)
            })
        ]
        .height(Fill)
        .into()
    }

    fn device_rail(&self) -> Element<'static, Message> {
        let home = tooltip(
            action_button(
                "⌂",
                Some(Message::Action(UserAction::Home)),
                self.model.page == Page::Home,
            ),
            text("Home"),
            iced::widget::tooltip::Position::Right,
        );
        let mut rail = column![home, divider_label("DEVICES")]
            .spacing(tokens::SPACE_2)
            .align_x(iced::Alignment::Center);
        for device in &self.model.core.devices {
            let action = (device.online && device.host_capable)
                .then(|| Message::Action(UserAction::SelectDevice(device.id.clone())));
            let initial = device
                .name
                .chars()
                .next()
                .unwrap_or('D')
                .to_uppercase()
                .to_string();
            let status_color = if device.online {
                tokens::ONLINE
            } else {
                tokens::OFFLINE
            };
            let selected = self.model.core.selected_device.as_ref() == Some(&device.id);
            let indicator_color = if selected {
                tokens::ACCENT
            } else {
                Color::TRANSPARENT
            };
            let indicator = container(iced::widget::Space::new().width(Fill).height(Fill))
                .width(tokens::RAIL_INDICATOR_WIDTH)
                .height(tokens::DEVICE_TILE_SIZE)
                .style(move |_theme: &Theme| container::Style {
                    background: Some(Background::Color(indicator_color)),
                    ..Default::default()
                });
            let badge = container(
                column![
                    text(initial).size(tokens::HEADER_SIZE),
                    text("●").size(tokens::META_SIZE).color(status_color),
                ]
                .spacing(tokens::SPACE_1)
                .align_x(iced::Alignment::Center),
            )
            .width(tokens::DEVICE_TILE_SIZE - tokens::RAIL_INDICATOR_WIDTH)
            .height(tokens::DEVICE_TILE_SIZE)
            .center_x(Fill)
            .center_y(Fill)
            .style(panel_style(if selected {
                tokens::ACCENT_WASH
            } else {
                tokens::CARD
            }));
            let tile = row![indicator, badge].align_y(iced::Alignment::Center);
            rail = rail.push(tooltip(
                action_button_widget(tile, action, selected, format!("device:{:?}", device.id)),
                text(device.name.clone()),
                iced::widget::tooltip::Position::Right,
            ));
        }
        rail = rail
            .push(tooltip(
                action_button(
                    "＋",
                    Some(Message::Action(UserAction::DiscoverDevices)),
                    false,
                ),
                text("Discover devices"),
                iced::widget::tooltip::Position::Right,
            ))
            .push(iced::widget::Space::new().height(Fill))
            .push(tooltip(
                action_button(
                    "◉",
                    Some(Message::Action(UserAction::Session)),
                    self.model.page == Page::Session,
                ),
                text("Session"),
                iced::widget::tooltip::Position::Right,
            ));
        container(rail)
            .width(tokens::DEVICE_RAIL_WIDTH)
            .height(Fill)
            .padding(tokens::SPACE_2)
            .style(panel_style(tokens::RAIL))
            .into()
    }
    fn device_sidebar(&self) -> iced::widget::Container<'static, Message> {
        if self.model.device_sidebar_collapsed {
            return container(
                column![
                    tooltip(
                        action_button(
                            "›",
                            Some(Message::Action(UserAction::ToggleDeviceSidebar)),
                            false,
                        ),
                        text("Expand devices"),
                        iced::widget::tooltip::Position::Right,
                    ),
                    compact_local_panel(&self.model, self.core.is_live(), self.live_input_ready()),
                ]
                .spacing(tokens::SPACE_2),
            )
            .height(Fill)
            .padding(tokens::SPACE_2)
            .style(panel_style(tokens::SIDEBAR));
        }
        let selected = self.selected_device();
        let mut displays = column![section_label("STREAM")].spacing(tokens::SPACE_2);
        if let Some(device) = selected {
            if device.displays.is_empty() {
                let is_selected_connected = self.model.core.selected_device.as_ref()
                    == Some(&device.id)
                    && self.model.core.telemetry.session.connection_state
                        == racc_telemetry::ConnectionState::Connected;
                let (label, detail, action_label, action) = match topology_empty_status(
                    device.online,
                    device.host_capable,
                    is_selected_connected,
                ) {
                    TopologyEmptyStatus::Offline => (
                        "Computer is offline",
                        "Reconnect it to Tailscale, then refresh the device list.",
                        "Refresh devices",
                        Some(Message::Action(UserAction::DiscoverDevices)),
                    ),
                    TopologyEmptyStatus::HostUnavailable => (
                        "Host agent not responding",
                        "The computer is online, but its Racc Connect host agent has not answered.",
                        "Refresh devices",
                        Some(Message::Action(UserAction::DiscoverDevices)),
                    ),
                    TopologyEmptyStatus::AwaitingTopology => (
                        "Waiting for display information",
                        "The session is connected. The host has not reported its monitors yet.",
                        "",
                        None,
                    ),
                    TopologyEmptyStatus::RequestTopology => (
                        "No displays reported yet",
                        "Connect to this computer to request its current display topology.",
                        "Connect to computer",
                        Some(Message::Action(UserAction::Connect(device.id.clone()))),
                    ),
                };
                let mut prompt = column![
                    text(label).size(tokens::BODY_SIZE),
                    text(detail).size(tokens::META_SIZE).color(tokens::MUTED),
                ]
                .spacing(tokens::SPACE_2);
                if let Some(action) = action {
                    prompt = prompt.push(action_button(action_label, Some(action), false));
                }
                displays = displays.push(
                    container(prompt)
                        .padding(tokens::SPACE_3)
                        .style(panel_style(tokens::CARD)),
                );
            }
            for display in &device.displays {
                let is_selected = self.model.core.selected_display == Some(display.id);
                let availability = if display.available {
                    "Available"
                } else {
                    "Unavailable"
                };
                let info = format!(
                    "{}×{}  ·  {:.0} Hz  ·  {:.0}%  ·  {availability}",
                    display.width_px,
                    display.height_px,
                    display.refresh_mhz as f32 / 1_000.0,
                    100.0 * display.scale_milli as f32 / 1_000.0,
                );
                let marker_color = if is_selected {
                    tokens::ACCENT
                } else {
                    Color::TRANSPARENT
                };
                let marker = container(iced::widget::Space::new().width(Fill).height(Fill))
                    .width(tokens::RAIL_INDICATOR_WIDTH)
                    .height(tokens::DEVICE_TILE_SIZE)
                    .style(move |_theme: &Theme| container::Style {
                        background: Some(Background::Color(marker_color)),
                        ..Default::default()
                    });
                let item = row![
                    marker,
                    column![
                        text(display.name.clone()).size(tokens::BODY_SIZE),
                        text(info)
                            .size(tokens::META_SIZE)
                            .color(if display.available {
                                tokens::MUTED
                            } else {
                                tokens::OFFLINE
                            }),
                    ]
                    .spacing(tokens::SPACE_1),
                ]
                .spacing(tokens::SPACE_2)
                .align_y(iced::Alignment::Center);
                let action = display
                    .available
                    .then_some(Message::Action(UserAction::SelectDisplay(display.id)));
                displays = displays.push(action_button_widget(
                    item,
                    action,
                    is_selected,
                    format!("display:{:?}", display.id),
                ));
            }
        } else {
            displays = displays.push(muted_text("Choose a device from the rail"));
        }
        let selected_name = selected.map_or_else(
            || "No device selected".to_owned(),
            |device| device.name.clone(),
        );
        let is_online = selected.is_some_and(|device| device.online);
        let header = container(
            column![
                row![
                    section_label("DEVICE"),
                    iced::widget::Space::new().width(Fill),
                    status_pill(
                        if is_online { "ONLINE" } else { "OFFLINE" },
                        if is_online {
                            tokens::ONLINE
                        } else {
                            tokens::OFFLINE
                        },
                    ),
                    action_button(
                        "‹",
                        Some(Message::Action(UserAction::ToggleDeviceSidebar)),
                        false,
                    ),
                ]
                .spacing(tokens::SPACE_1)
                .align_y(iced::Alignment::Center),
                text(selected_name).size(tokens::HEADER_SIZE),
            ]
            .spacing(tokens::SPACE_1),
        )
        .padding(tokens::SPACE_3)
        .style(panel_style(tokens::SURFACE));
        let live = self.core.is_live();
        let can_capture = !live || self.live_input_ready();
        let remote_control_enabled =
            !live && self.model.keyboard_capture && self.model.mouse_capture;
        let keyboard_action_allowed = !live || can_capture || self.model.keyboard_capture;
        let keyboard_control_action = if live {
            keyboard_action_allowed.then_some(Message::Action(UserAction::SetKeyboardCapture(
                !self.model.keyboard_capture,
            )))
        } else {
            Some(Message::Action(UserAction::ToggleRemoteDesktop))
        };
        let clipboard_status = if live {
            if !self.core.clipboard_available() {
                "Clipboard adapter unavailable".to_owned()
            } else if self.model.core.clipboard_sync_enabled {
                "Text sync enabled for this session".to_owned()
            } else {
                "Text sync disabled for this session".to_owned()
            }
        } else {
            self.model.clipboard_status.clone()
        };
        let clipboard_last_sync = self.model.clipboard_last_sync_label();
        let control_items = column![
            action_button(
                if live {
                    if self.model.keyboard_capture_active {
                        "Keyboard capture  ·  active"
                    } else if self.model.keyboard_capture {
                        "Keyboard capture  ·  armed"
                    } else {
                        "Enable keyboard capture"
                    }
                } else if remote_control_enabled {
                    "Remote control  ·  enabled"
                } else {
                    "Remote control"
                },
                keyboard_control_action,
                if live {
                    self.model.keyboard_capture
                } else {
                    remote_control_enabled
                },
            ),
            action_button(
                if self.model.mouse_capture_active {
                    "Pointer capture  ·  active"
                } else if self.model.mouse_capture {
                    "Pointer capture  ·  armed"
                } else {
                    "Enable pointer capture"
                },
                (!live || can_capture || self.model.mouse_capture).then_some(Message::Action(
                    UserAction::SetMouseCapture(!self.model.mouse_capture)
                )),
                self.model.mouse_capture,
            ),
            row![
                status_pill(
                    if live {
                        if self.model.keyboard_capture_active {
                            "KEYS ON"
                        } else if self.model.keyboard_capture {
                            "KEYS ARMED"
                        } else {
                            "KEYS OFF"
                        }
                    } else if self.model.keyboard_capture {
                        "KEYS ON"
                    } else {
                        "KEYS OFF"
                    },
                    if self.model.keyboard_capture_active || (!live && self.model.keyboard_capture)
                    {
                        tokens::ONLINE
                    } else {
                        tokens::MUTED
                    },
                ),
                status_pill(
                    if live {
                        if self.model.mouse_capture_active {
                            "POINTER ON"
                        } else if self.model.mouse_capture {
                            "POINTER ARMED"
                        } else {
                            "POINTER OFF"
                        }
                    } else if self.model.mouse_capture {
                        "POINTER ON"
                    } else {
                        "POINTER OFF"
                    },
                    if self.model.mouse_capture_active || (!live && self.model.mouse_capture) {
                        tokens::ONLINE
                    } else {
                        tokens::MUTED
                    },
                ),
            ]
            .spacing(tokens::SPACE_1),
            action_button(
                if !self.core.clipboard_available() {
                    "Clipboard adapter unavailable"
                } else if self.model.core.clipboard_sync_enabled {
                    "Disable text clipboard sync"
                } else {
                    "Enable text clipboard sync"
                },
                (self.core.clipboard_available() && self.model.core.clipboard_session_active)
                    .then_some(Message::Action(UserAction::ToggleClipboardSync)),
                self.core.clipboard_available() && self.model.core.clipboard_sync_enabled,
            ),
            muted_text(&format!("Clipboard  ·  {clipboard_status}")),
            muted_text(&format!("Last sync  ·  {clipboard_last_sync}")),
        ]
        .spacing(tokens::SPACE_2);
        let controls = column![
            section_label("CONTROL"),
            container(control_items)
                .padding(tokens::SPACE_2)
                .style(panel_style(tokens::CARD)),
            section_label("SYSTEM"),
            column![
                action_button(
                    if self.model.telemetry_sidebar_collapsed {
                        "Show live performance"
                    } else {
                        "Hide live performance"
                    },
                    Some(Message::Action(UserAction::ToggleTelemetrySidebar)),
                    !self.model.telemetry_sidebar_collapsed,
                ),
                action_button(
                    "Settings",
                    Some(Message::Action(UserAction::Settings)),
                    matches!(self.model.page, Page::Settings | Page::About),
                ),
            ]
            .spacing(tokens::SPACE_1),
        ]
        .spacing(tokens::SPACE_2);
        container(
            column![
                header,
                scrollable(column![displays, controls].spacing(tokens::SPACE_4))
                    .height(Fill)
                    .style(scroll_style),
                local_panel(&self.model, self.core.is_live(), self.live_input_ready()),
            ]
            .spacing(tokens::SPACE_3)
            .height(Fill),
        )
        .height(Fill)
        .padding(tokens::SPACE_3)
        .style(panel_style(tokens::SIDEBAR))
    }
    fn selected_device(&self) -> Option<&racc_core::DeviceSnapshot> {
        let selected = self.model.core.selected_device.as_ref()?;
        self.model
            .core
            .devices
            .iter()
            .find(|device| &device.id == selected)
    }

    fn debug_latency_widget(&self) -> Element<'static, Message> {
        if !self.debug_latency {
            return iced::widget::Space::new().width(Fill).height(Fill).into();
        }
        let frame = self.frame_source.latest_frame();
        let summary = debug_latency_summary(frame.as_deref(), self.last_observed_frame_interval_us);
        container(text(summary).size(tokens::META_SIZE).color(tokens::TEXT))
            .padding(tokens::SPACE_2)
            .style(panel_style(tokens::SURFACE))
            .into()
    }

    fn workspace(&self) -> iced::widget::Container<'static, Message> {
        match self.model.page {
            Page::Home => self.home_page(),
            Page::Settings => self.settings_page(),
            Page::About => self.about_page(),
            Page::Session => self.session_page(),
        }
    }

    fn session_page(&self) -> iced::widget::Container<'static, Message> {
        let device = self.selected_device();
        let display = device.and_then(|device| {
            self.model
                .core
                .selected_display
                .and_then(|id| device.displays.iter().find(|display| display.id == id))
        });
        let title = match (device, display) {
            (Some(device), Some(display)) => format!("{}  /  {}", device.name, display.name),
            (Some(device), None) => format!("{}  /  Choose a display", device.name),
            _ => "Choose a remote device".to_owned(),
        };
        let session = self.model.core.telemetry.session;
        let subtitle = match (display, self.model.stream_dimensions) {
            (Some(display), Some((width, height))) => {
                let rtt = session.last_rtt_us.map_or_else(
                    || "RTT unavailable".to_owned(),
                    |us| format!("{} ms RTT", us / 1_000),
                );
                format!(
                    "{}×{} stream  ·  {:.0} Hz source  ·  {:?}  ·  {:.0} FPS  ·  {rtt}",
                    width,
                    height,
                    display.refresh_mhz as f32 / 1_000.0,
                    session.codec,
                    session.fps,
                )
            }
            (Some(display), None) => format!(
                "{}×{} source  ·  Waiting for stream",
                display.width_px, display.height_px
            ),
            _ => "Connect to a device to start a session".to_owned(),
        };
        let (status, status_color) = session_status(
            &self.model.overlay,
            device.is_some(),
            device.is_some_and(|device| device.online),
            display.is_some(),
            display.is_some_and(|display| display.available),
            self.model.stream_dimensions.is_some(),
            session.connection_state,
        );
        let header = container(
            column![
                row![
                    section_label("REMOTE SESSION"),
                    status_pill(status, status_color),
                ]
                .spacing(tokens::SPACE_2)
                .align_y(iced::Alignment::Center),
                text(title).size(tokens::TITLE_SIZE),
                muted_text(&subtitle),
            ]
            .spacing(tokens::SPACE_1),
        )
        .width(Fill)
        .padding(tokens::SPACE_3)
        .style(panel_style(tokens::SURFACE));
        let video: Element<'_, Message> = if self.fake_idle {
            container(
                column![
                    text("Stream paused for measurement").size(tokens::HEADER_SIZE),
                    muted_text("No video frames are being generated in idle mode"),
                ]
                .spacing(tokens::SPACE_2)
                .align_x(iced::Alignment::Center),
            )
            .width(Fill)
            .height(Fill)
            .center_x(Fill)
            .center_y(Fill)
            .into()
        } else {
            iced::widget::shader(VideoProgram {
                source: Arc::clone(&self.frame_source),
                cursor_cache: Arc::clone(&self.cursor_cache),
            })
            .width(Fill)
            .height(Fill)
            .into()
        };
        let pointer_area = iced::widget::canvas::Canvas::new(VideoInputProgram {
            source: Arc::clone(&self.frame_source),
            enabled: self.model.mouse_capture_active,
        })
        .width(Fill)
        .height(Fill);
        let empty_session = if !self.fake_idle
            && (self.frame_source.latest_frame().is_none() || display.is_none())
            && matches!(&self.model.overlay, SessionOverlay::None)
        {
            empty_session_widget(device, display, session.connection_state)
        } else {
            iced::widget::Space::new().width(Fill).height(Fill).into()
        };
        let layered = iced::widget::stack([
            Element::from(video),
            Element::from(pointer_area),
            empty_session,
            self.debug_latency_widget(),
            overlay_widget(&self.model.overlay),
        ])
        .width(Fill)
        .height(Fill);
        let video_frame = container(layered)
            .width(Fill)
            .height(Fill)
            .padding(tokens::SPACE_2)
            .style(panel_style(tokens::VIDEO_FRAME));

        let controls = container(
            column![
                scrollable(quality_controls(
                    self.model.core.selected_device.clone(),
                    self.model.session_quality,
                    false,
                ))
                .width(Fill)
                .height(iced::Length::Shrink)
                .direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::default(),
                ))
                .style(scroll_style),
                row![
                    action_button(
                        if self.model.keyboard_capture_active {
                            "⌨ Keys on"
                        } else if self.model.keyboard_capture {
                            "⌨ Keys armed"
                        } else {
                            "⌨ Keys"
                        },
                        (!self.core.is_live()
                            || self.live_input_ready()
                            || self.model.keyboard_capture)
                            .then_some(Message::Action(UserAction::SetKeyboardCapture(
                                !self.model.keyboard_capture,
                            ))),
                        self.model.keyboard_capture,
                    ),
                    action_button(
                        if self.model.mouse_capture_active {
                            "⌖ Pointer on"
                        } else if self.model.mouse_capture {
                            "⌖ Pointer armed"
                        } else {
                            "⌖ Pointer"
                        },
                        (!self.core.is_live()
                            || self.live_input_ready()
                            || self.model.mouse_capture)
                            .then_some(Message::Action(UserAction::SetMouseCapture(
                                !self.model.mouse_capture,
                            ))),
                        self.model.mouse_capture,
                    ),
                    iced::widget::Space::new().width(Fill),
                    action_button("⛶", Some(Message::ToggleFullscreen), self.fullscreen,),
                    danger_button("Disconnect", Some(Message::Action(UserAction::Disconnect)),),
                ]
                .spacing(tokens::SPACE_2)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(tokens::SPACE_2),
        )
        .width(Fill)
        .padding(tokens::SPACE_2)
        .style(panel_style(tokens::SURFACE));

        container(
            column![header, self.monitor_selector(), video_frame, controls,]
                .spacing(tokens::SPACE_3)
                .height(Fill),
        )
        .height(Fill)
        .padding(tokens::SPACE_3)
        .style(panel_style(tokens::MAIN))
    }
    fn monitor_selector(&self) -> Element<'static, Message> {
        let Some(device) = self.selected_device() else {
            return container(row![
                section_label("MONITORS"),
                muted_text("Select a device to choose a display"),
            ])
            .padding(tokens::SPACE_2)
            .style(panel_style(tokens::SURFACE))
            .into();
        };

        let mut monitors = row![section_label("MONITORS")]
            .spacing(tokens::SPACE_2)
            .align_y(iced::Alignment::Center);
        for display in &device.displays {
            let selected = self.model.core.selected_display == Some(display.id);
            let details = format!("{}×{}", display.width_px, display.height_px);
            let item = row![
                text(if display.available { "●" } else { "○" }).color(if display.available {
                    tokens::ONLINE
                } else {
                    tokens::OFFLINE
                }),
                column![
                    text(display.name.clone()).size(tokens::BODY_SIZE),
                    text(details).size(tokens::META_SIZE).color(tokens::MUTED),
                ]
                .spacing(tokens::SPACE_1),
            ]
            .spacing(tokens::SPACE_2)
            .align_y(iced::Alignment::Center);
            let action = display
                .available
                .then_some(Message::Action(UserAction::SelectDisplay(display.id)));
            monitors = monitors.push(action_button_widget(
                item,
                action,
                selected,
                format!("header-display:{:?}", display.id),
            ));
        }
        container(
            scrollable(monitors)
                .width(Fill)
                .height(iced::Length::Shrink)
                .direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::default(),
                ))
                .style(scroll_style),
        )
        .padding(tokens::SPACE_1)
        .style(panel_style(tokens::SURFACE))
        .into()
    }
    fn home_page(&self) -> iced::widget::Container<'static, Message> {
        let online_count = self
            .model
            .core
            .devices
            .iter()
            .filter(|device| device.online)
            .count();
        let host_count = self
            .model
            .core
            .devices
            .iter()
            .filter(|device| device.online && device.host_capable)
            .count();
        let local_name = if self.model.core.local_device_name.trim().is_empty() {
            "This computer".to_owned()
        } else {
            self.model.core.local_device_name.clone()
        };
        let (refresh_label, refresh_color) = match &self.model.discovery_state {
            view_model::DiscoveryState::Idle => ("Not refreshed yet", tokens::MUTED),
            view_model::DiscoveryState::Refreshing => ("Refreshing tailnet…", tokens::ACCENT),
            view_model::DiscoveryState::Updated { .. } => ("Device list updated", tokens::ONLINE),
            view_model::DiscoveryState::Failed(_) => ("Refresh failed", tokens::OFFLINE),
        };
        let refresh_detail = match &self.model.discovery_state {
            view_model::DiscoveryState::Idle => {
                "Use Discover to check the current Tailscale peer list".to_owned()
            }
            view_model::DiscoveryState::Refreshing => {
                "Checking Tailscale peers and host availability".to_owned()
            }
            view_model::DiscoveryState::Updated { peer_count: 0 } => {
                "Tailscale returned no peers; check the tailnet or refresh again".to_owned()
            }
            view_model::DiscoveryState::Updated { peer_count } => {
                format!("Tailscale returned {peer_count} peer records; status refreshes in the background")
            }
            view_model::DiscoveryState::Failed(_) => {
                "Check the app message and Tailscale connection".to_owned()
            }
        };
        let refreshing = matches!(
            &self.model.discovery_state,
            view_model::DiscoveryState::Refreshing
        );
        let header = container(
            row![
                column![
                    section_label("TAILNET OVERVIEW"),
                    text("Remote computers").size(tokens::TITLE_SIZE),
                    muted_text("Select an available host to open its live displays"),
                ]
                .spacing(tokens::SPACE_1),
                iced::widget::Space::new().width(Fill),
                status_pill(
                    &format!("{online_count} ONLINE · {host_count} HOSTS READY"),
                    if host_count > 0 {
                        tokens::ONLINE
                    } else if online_count > 0 {
                        tokens::ACCENT
                    } else {
                        tokens::OFFLINE
                    },
                ),
                action_button(
                    if refreshing {
                        "Refreshing…"
                    } else {
                        "↻ Discover"
                    },
                    (!refreshing).then_some(Message::Action(UserAction::DiscoverDevices)),
                    false,
                ),
            ]
            .spacing(tokens::SPACE_2)
            .align_y(iced::Alignment::Center),
        )
        .padding(tokens::SPACE_3)
        .style(panel_style(tokens::SURFACE));
        let local_device = container(
            row![
                column![
                    section_label("THIS COMPUTER"),
                    text(local_name).size(tokens::HEADER_SIZE),
                    muted_text(&local_host_status_text(&self.model)),
                ]
                .spacing(tokens::SPACE_1),
                iced::widget::Space::new().width(Fill),
                status_pill("LOCAL DEVICE", tokens::ACCENT),
            ]
            .align_y(iced::Alignment::Center),
        )
        .width(Fill)
        .padding(tokens::SPACE_3)
        .style(panel_style(tokens::CARD));
        let refresh_status = container(
            row![
                status_pill(refresh_label, refresh_color),
                muted_text(&refresh_detail),
                iced::widget::Space::new().width(Fill),
                muted_text(&format!("{online_count} online · {host_count} host-ready")),
            ]
            .spacing(tokens::SPACE_2)
            .align_y(iced::Alignment::Center),
        )
        .width(Fill)
        .padding(tokens::SPACE_2)
        .style(panel_style(tokens::SURFACE));
        let mut devices = column![].spacing(tokens::SPACE_2);
        if self.model.core.devices.is_empty() {
            devices = devices.push(
                container(
                    column![
                        text("No tailnet peers found yet").size(tokens::HEADER_SIZE),
                        muted_text(if matches!(
                            &self.model.discovery_state,
                            view_model::DiscoveryState::Updated { peer_count: 0 }
                        ) {
                            "Tailscale returned no peers. Check the tailnet connection, then refresh."
                        } else {
                            "Discovered computers will appear here, including offline peers and devices without a host agent."
                        }),
                    ]
                    .spacing(tokens::SPACE_2),
                )
                .padding(tokens::SPACE_4)
                .style(panel_style(tokens::CARD)),
            );
        }
        for device in &self.model.core.devices {
            let can_connect = device.online && device.host_capable;
            let selected = self.model.core.selected_device.as_ref() == Some(&device.id);
            let display_count = device.displays.len();
            let status = if can_connect {
                "HOST READY"
            } else if device.online {
                "AGENT NOT FOUND"
            } else {
                "OFFLINE"
            };
            let details = format!("{:?}  ·  {display_count} display(s)", device.os);
            let initials = device
                .name
                .chars()
                .take(2)
                .collect::<String>()
                .to_uppercase();
            let avatar = container(
                column![
                    text(initials).size(tokens::HEADER_SIZE),
                    text("●").size(tokens::META_SIZE).color(if can_connect {
                        tokens::ONLINE
                    } else {
                        tokens::OFFLINE
                    }),
                ]
                .spacing(tokens::SPACE_1)
                .align_x(iced::Alignment::Center),
            )
            .width(tokens::HOME_DEVICE_AVATAR_SIZE)
            .height(tokens::HOME_DEVICE_AVATAR_SIZE)
            .center_x(Fill)
            .center_y(Fill)
            .style(panel_style(if can_connect {
                tokens::ACCENT_WASH
            } else {
                tokens::SURFACE
            }));
            let summary = column![
                row![
                    text(device.name.clone()).size(tokens::HEADER_SIZE),
                    status_pill(
                        status,
                        if can_connect {
                            tokens::ONLINE
                        } else if device.online {
                            tokens::ACCENT
                        } else {
                            tokens::OFFLINE
                        },
                    ),
                ]
                .spacing(tokens::SPACE_2)
                .align_y(iced::Alignment::Center),
                muted_text(&details),
            ]
            .spacing(tokens::SPACE_1);
            let card = row![
                avatar,
                summary,
                iced::widget::Space::new().width(Fill),
                action_button(
                    if can_connect {
                        "Open session"
                    } else {
                        "Unavailable"
                    },
                    can_connect.then(|| Message::Action(UserAction::Connect(device.id.clone()))),
                    selected,
                ),
            ]
            .spacing(tokens::SPACE_3)
            .align_y(iced::Alignment::Center);
            devices = devices.push(container(card).width(Fill).padding(tokens::SPACE_3).style(
                panel_style(if selected {
                    tokens::SELECTED
                } else {
                    tokens::CARD
                }),
            ));
        }
        container(
            column![
                header,
                local_device,
                refresh_status,
                scrollable(devices).height(Fill).style(scroll_style),
            ]
            .spacing(tokens::SPACE_3)
            .height(Fill),
        )
        .height(Fill)
        .padding(tokens::SPACE_3)
        .style(panel_style(tokens::MAIN))
    }
    #[cfg(target_os = "macos")]
    fn mac_permission_controls(&self) -> iced::widget::Column<'static, Message> {
        let host_status = self.model.local_host_status.as_ref();
        let screen_recording_granted =
            host_status.and_then(|status| status.screen_recording_granted);
        let accessibility_granted = host_status.and_then(|status| status.accessibility_granted);
        let permission_row = |title: &'static str,
                              granted: Option<bool>,
                              pane: mac_permissions::PermissionPane| {
            let (state, purpose) = match granted {
                Some(true) => ("Granted", "Permission is available to the host agent."),
                Some(false) => ("Required", "Required by the host agent for this operation."),
                None => (
                    "Unknown",
                    "Host-agent permission status is unavailable; start the host agent to check it.",
                ),
            };
            row![
                column![
                    text(title),
                    muted_text(purpose),
                    text(state).size(tokens::META_SIZE).color(match granted {
                        Some(true) => tokens::ONLINE,
                        Some(false) => tokens::OFFLINE,
                        None => tokens::MUTED,
                    }),
                ]
                .spacing(tokens::SPACE_1),
                iced::widget::Space::new().width(Fill),
                action_button("Open Settings", Some(Message::OpenMacSettings(pane)), false,),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(tokens::SPACE_2)
        };

        column![
            section_label("MACOS PERMISSIONS"),
            muted_text(
                "These checks report the host agent that performs capture and input. Checks are read-only; Racc Connect never prompts automatically."
            ),
            permission_row(
                "Screen Recording",
                screen_recording_granted,
                mac_permissions::PermissionPane::ScreenRecording,
            ),
            permission_row(
                "Accessibility",
                accessibility_granted,
                mac_permissions::PermissionPane::Accessibility,
            ),
        ]
        .spacing(tokens::SPACE_2)
    }

    fn settings_page(&self) -> iced::widget::Container<'static, Message> {
        let host_controls = self.model.local_host_controls_available;
        let hosting = if host_controls {
            self.model
                .local_host_status
                .as_ref()
                .is_some_and(|status| status.hosting_enabled)
        } else {
            self.settings.hosting_enabled
        };
        #[cfg(target_os = "macos")]
        let permission_controls = self.mac_permission_controls();
        #[cfg(not(target_os = "macos"))]
        let permission_controls = column![];

        let mut allowlist = column![section_label("ALLOWLIST")].spacing(tokens::SPACE_2);
        if host_controls {
            for peer in &self.model.local_host_allowlist {
                let detail = if peer.login_name.is_empty() {
                    peer.node_id.clone()
                } else {
                    format!("{} · {}", peer.node_id, peer.login_name)
                };
                allowlist = allowlist.push(
                    row![
                        column![text(peer.display_name.clone()), muted_text(&detail),]
                            .spacing(tokens::SPACE_1),
                        iced::widget::Space::new().width(Fill),
                        action_button(
                            "Remove",
                            self.model.local_host_connected.then(|| {
                                Message::LocalHostAction(LocalHostCommand::Remove(
                                    peer.node_id.clone(),
                                ))
                            }),
                            false,
                        ),
                    ]
                    .align_y(iced::Alignment::Center),
                );
            }
            if self.model.local_host_allowlist.is_empty() {
                allowlist = allowlist.push(muted_text(
                    "No approved peers. Approve a pending peer to allow it to connect.",
                ));
            }
            allowlist = allowlist.push(section_label("PENDING APPROVALS"));
            for peer in &self.model.local_host_pending {
                let detail = if peer.login_name.is_empty() {
                    peer.node_id.clone()
                } else {
                    format!("{} · {}", peer.node_id, peer.login_name)
                };
                allowlist = allowlist.push(
                    row![
                        column![text(peer.display_name.clone()), muted_text(&detail),]
                            .spacing(tokens::SPACE_1),
                        iced::widget::Space::new().width(Fill),
                        action_button(
                            "Approve",
                            self.model.local_host_connected.then(|| {
                                Message::LocalHostAction(LocalHostCommand::Approve(
                                    peer.node_id.clone(),
                                ))
                            }),
                            false,
                        ),
                        action_button(
                            "Reject",
                            self.model.local_host_connected.then(|| {
                                Message::LocalHostAction(LocalHostCommand::Reject(
                                    peer.node_id.clone(),
                                ))
                            }),
                            false,
                        ),
                    ]
                    .spacing(tokens::SPACE_2)
                    .align_y(iced::Alignment::Center),
                );
            }
            if self.model.local_host_pending.is_empty() {
                allowlist = allowlist.push(muted_text("No peers are waiting for approval."));
            }
        } else {
            for device in &self.model.core.devices {
                let state = if device.host_capable {
                    "approved"
                } else {
                    "not approved"
                };
                let entry = row![
                    muted_text(&format!("{}  ·  {:?}  ·  {state}", device.name, device.os)),
                    iced::widget::Space::new().width(Fill),
                    action_button(
                        "Remove",
                        device
                            .host_capable
                            .then(|| Message::Action(UserAction::RemovePeer(device.id.clone()))),
                        false,
                    ),
                ]
                .align_y(iced::Alignment::Center);
                allowlist = allowlist.push(entry);
            }
            if let SessionOverlay::WaitingForApproval {
                device_id,
                device_name,
            } = &self.model.overlay
            {
                allowlist = allowlist.push(
                    row![
                        text(format!("Pending: {device_name}")),
                        action_button(
                            "Approve",
                            Some(Message::Action(UserAction::ApprovePeer(device_id.clone()))),
                            false
                        ),
                        action_button(
                            "Reject",
                            Some(Message::Action(UserAction::RejectPeer(device_id.clone()))),
                            false
                        )
                    ]
                    .spacing(tokens::SPACE_2),
                );
            }
        }

        let preferences = column![
            section_label("PREFERENCES"),
            muted_text(if host_controls {
                "Host state and peer permissions come from the local agent. Saved UI settings do not claim hosting is running."
            } else {
                "Host controls are simulated in fake mode; a host-agent is not connected."
            }),
            pick_list(
                settings::CAPTURE_RELEASE_HOTKEY_OPTIONS
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
                Some(self.settings.capture_release_hotkey.clone()),
                Message::SetCaptureReleaseHotkey,
            )
            .placeholder("Choose a local release chord"),
            muted_text("The built-in Ctrl+Alt+Shift+Escape emergency chord always remains available; release chords are consumed locally."),
            checkbox(self.settings.clipboard_enabled)
                .label("Enable text clipboard sync automatically after a host handshake")
                .on_toggle(Message::SetClipboardPreference),
        ]
        .spacing(tokens::SPACE_2);
        #[cfg(target_os = "macos")]
        let preferences = preferences.push(
            checkbox(self.settings.swap_ctrl_command)
                .label("Swap Control and Command for remote keyboard input")
                .on_toggle(Message::SetSwapCtrlCommand),
        );

        let host_status_text = local_host_status_text(&self.model);
        let hosting_action = if host_controls {
            self.model
                .local_host_connected
                .then_some(Message::LocalHostAction(
                    LocalHostCommand::SetHostingEnabled(!hosting),
                ))
        } else {
            Some(Message::Action(UserAction::SetHosting(!hosting)))
        };
        let hosting_label = if !host_controls {
            if hosting {
                "Simulated hosting is on · Turn off"
            } else {
                "Simulated hosting is off · Turn on"
            }
        } else if !self.model.local_host_connected {
            "Host agent unavailable"
        } else if hosting {
            "Hosting is enabled · Turn off"
        } else {
            "Hosting is disabled · Turn on"
        };
        container(
            scrollable(
                column![
                    text("Settings").size(tokens::HEADER_SIZE),
                    section_label("HOSTING"),
                    status_pill(
                        &host_status_text,
                        if self.model.local_host_status.as_ref().is_some_and(|status| {
                            status.helper_state == racc_core::ipc::HelperState::Running
                        }) {
                            tokens::ONLINE
                        } else {
                            tokens::MUTED
                        },
                    ),
                    action_button(hosting_label, hosting_action, hosting),
                    quality_controls(None, self.model.default_quality, true),
                    allowlist,
                    permission_controls,
                    preferences,
                    action_button(
                        if self.model.autostart_enabled {
                            "Disable start at sign-in"
                        } else {
                            "Start at sign-in"
                        },
                        Some(Message::Action(UserAction::SetAutostart(
                            !self.model.autostart_enabled,
                        ))),
                        self.model.autostart_enabled,
                    ),
                    muted_text("Adds or removes this app's current-user startup entry."),
                    action_button(
                        "About and third-party notices",
                        Some(Message::Action(UserAction::About)),
                        false,
                    ),
                ]
                .spacing(tokens::SPACE_3),
            )
            .height(Fill),
        )
        .height(Fill)
        .padding(tokens::SPACE_4)
        .style(panel_style(tokens::MAIN))
    }

    fn about_page(&self) -> iced::widget::Container<'static, Message> {
        let notices = include_str!("../../../THIRD_PARTY_LICENSES.md");
        let assets = include_str!("../../../docs/ASSETS.md");
        container(
            column![
                row![
                    text("About Racc Connect").size(tokens::HEADER_SIZE),
                    iced::widget::Space::new().width(Fill),
                    action_button("Back to settings", Some(Message::Action(UserAction::Settings)), false),
                ]
                .align_y(iced::Alignment::Center),
                muted_text("Native Rust remote desktop viewer and host controls."),
                section_label("DISTRIBUTION"),
                muted_text("Project license and distribution terms are undecided. No license file is published."),
                section_label("THIRD-PARTY NOTICES"),
                container(scrollable(text(notices).size(tokens::META_SIZE)).height(Fill).style(scroll_style))
                    .height(Fill)
                    .padding(tokens::SPACE_2)
                    .style(panel_style(tokens::CARD)),
                section_label("PROJECT ARTWORK AND ATTRIBUTION"),
                container(scrollable(text(assets).size(tokens::META_SIZE)).height(160).style(scroll_style))
                    .height(160)
                    .padding(tokens::SPACE_2)
                    .style(panel_style(tokens::CARD)),
            ]
            .spacing(tokens::SPACE_2)
            .height(Fill),
        )
        .height(Fill)
        .padding(tokens::SPACE_4)
        .style(panel_style(tokens::MAIN))
    }

    fn telemetry_sidebar(&self) -> iced::widget::Container<'static, Message> {
        if self.model.telemetry_sidebar_collapsed {
            return container(tooltip(
                action_button(
                    "‹",
                    Some(Message::Action(UserAction::ToggleTelemetrySidebar)),
                    false,
                ),
                text("Expand live telemetry"),
                iced::widget::tooltip::Position::Left,
            ))
            .height(Fill)
            .padding(tokens::SPACE_2)
            .style(panel_style(tokens::SIDEBAR));
        }
        let session = self.model.core.telemetry.session;
        let host = self.model.core.telemetry.host;
        let rtt = session
            .last_rtt_us
            .map_or_else(|| "—".to_owned(), |us| format!("{} ms", us / 1_000));
        let loss = format!("{:.2}%", session.loss_fraction * 100.0);
        let frame_loss = format!("{:.2}%", session.frame_loss_fraction * 100.0);
        let bitrate = format!("{:.1}", session.bitrate_bps as f64 / 1_000_000.0);
        let loss_color = if session.loss_fraction > tokens::LOSS_WARNING_FRACTION {
            tokens::OFFLINE
        } else {
            tokens::ONLINE
        };
        let state_label = format!("{:?}", session.connection_state);
        let state_color = if state_label.to_ascii_lowercase().contains("connected") {
            tokens::ONLINE
        } else {
            tokens::MUTED
        };
        let mut events = column![].spacing(tokens::SPACE_2);
        if let Some(notification) = &self.model.notification {
            events = events.push(
                container(
                    text(notification.clone())
                        .size(tokens::META_SIZE)
                        .color(tokens::OFFLINE),
                )
                .padding(tokens::SPACE_2)
                .style(panel_style(tokens::ACCENT_WASH)),
            );
        }
        for event in self
            .model
            .core
            .telemetry
            .events
            .events()
            .iter()
            .rev()
            .take(8)
        {
            let detail = row![
                text("•").color(tokens::ACCENT),
                column![
                    text(format!(
                        "{}  ·  {:?}",
                        event_timestamp_label(event.ts_us),
                        event.kind
                    ))
                    .size(tokens::META_SIZE),
                    muted_text(&event.detail),
                ]
                .spacing(tokens::SPACE_1),
            ]
            .spacing(tokens::SPACE_2)
            .align_y(iced::Alignment::Start);
            events = events.push(
                container(detail)
                    .width(Fill)
                    .padding(tokens::SPACE_2)
                    .style(panel_style(tokens::CARD)),
            );
        }
        if self.model.core.telemetry.events.events().is_empty() {
            events = events.push(muted_text("No recent session events"));
        }
        container(
            column![
                row![
                    section_label("LIVE TELEMETRY"),
                    iced::widget::Space::new().width(Fill),
                    action_button(
                        "›",
                        Some(Message::Action(UserAction::ToggleTelemetrySidebar)),
                        false,
                    ),
                ]
                .align_y(iced::Alignment::Center),
                section_label("SESSION"),
                status_pill(&state_label.to_uppercase(), state_color),
                row![
                    metric_card("RTT", rtt, tokens::TEXT),
                    metric_card("FRAME LOSS", frame_loss, loss_color),
                ]
                .spacing(tokens::SPACE_2),
                row![
                    metric_card("PACKET LOSS", loss, loss_color),
                    metric_card("RX · Mbps", bitrate, tokens::ACCENT),
                ]
                .spacing(tokens::SPACE_2),
                row![
                    metric_card("FPS", format!("{:.0}", session.fps), tokens::TEXT),
                    metric_card("EPOCH", session.epoch.to_string(), tokens::TEXT),
                ]
                .spacing(tokens::SPACE_2),
                section_label("HOST"),
                container(
                    column![
                        telemetry_line("CPU", format!("{:.1}%", host.cpu_pct_x10 as f32 / 10.0)),
                        telemetry_line(
                            "Process",
                            host.process_cpu_pct_x10.map_or_else(
                                || "Unknown".to_owned(),
                                |percent| format!("{:.1}%", percent as f32 / 10.0),
                            ),
                        ),
                        telemetry_line("Capture", format!("{:?}", host.capture_backend)),
                        telemetry_line("Encoder", format!("{:?}", host.encoder)),
                        telemetry_line("Resolution", format!("{}×{}", host.width, host.height)),
                        telemetry_line(
                            "Refresh",
                            format!("{:.0} Hz", host.refresh_mhz as f32 / 1_000.0)
                        ),
                        telemetry_line(
                            "Target",
                            format!("{:.1} Mbps", host.target_bitrate_kbps as f32 / 1_000.0),
                        ),
                        telemetry_line(
                            "Host TX",
                            format!("{:.1} Mbps", host.actual_bitrate_kbps as f32 / 1_000.0),
                        ),
                        telemetry_line("Codec", format!("{:?}", session.codec)),
                        telemetry_line("Decoder", format!("{:?}", session.decoder)),
                        telemetry_line("Path", format!("{:?}", session.path)),
                    ]
                    .spacing(tokens::SPACE_2),
                )
                .padding(tokens::SPACE_2)
                .style(panel_style(tokens::SURFACE)),
                section_label("EVENTS"),
                scrollable(events).height(Fill).style(scroll_style),
            ]
            .spacing(tokens::SPACE_2)
            .height(Fill),
        )
        .height(Fill)
        .padding(tokens::SPACE_3)
        .style(panel_style(tokens::SIDEBAR))
    }
    fn print_measurement(&self) {
        let mut samples = self.frame_intervals_ms.clone();
        samples.sort_by(f64::total_cmp);
        if samples.is_empty() {
            println!("frame_pacing_measurement seconds={} telemetry_collapsed={} fake_idle={} frame_samples=0 median_ms=unavailable p95_ms=unavailable over_40ms=0 presentation_note=frame-source-update-proxy-not-physical-presents", self.measurement_secs.unwrap_or_default(), self.model.telemetry_sidebar_collapsed, self.fake_idle);
            return;
        }
        let median = samples[samples.len() / 2];
        let p95 = samples[(samples.len() - 1) * 95 / 100];
        let over_40 = samples.iter().filter(|sample| **sample > 40.0).count();
        println!("frame_pacing_measurement seconds={} telemetry_collapsed={} fake_idle={} frame_samples={} median_ms={median:.2} p95_ms={p95:.2} over_40ms={over_40} presentation_note=frame-source-update-proxy-not-physical-presents", self.measurement_secs.unwrap_or_default(), self.model.telemetry_sidebar_collapsed, self.fake_idle, samples.len());
    }
}

fn local_host_status_text(model: &ViewModel) -> String {
    if !model.local_host_controls_available {
        return format!(
            "Fake mode · simulated hosting {}",
            if model.core.hosting_enabled {
                "on"
            } else {
                "off"
            }
        );
    }
    let Some(status) = &model.local_host_status else {
        return model
            .local_host_error
            .clone()
            .unwrap_or_else(|| "Local host-agent status is unavailable".to_owned());
    };
    let helper = match status.helper_state {
        racc_core::ipc::HelperState::Stopped => "Stopped",
        racc_core::ipc::HelperState::Starting => "Starting",
        racc_core::ipc::HelperState::Running => "Running",
        racc_core::ipc::HelperState::Recovering => "Recovering",
        racc_core::ipc::HelperState::NeedsAttention => "Needs attention",
    };
    format!(
        "{helper} · hosting {} · {} viewer(s) · {} pending approval(s)",
        if status.hosting_enabled {
            "enabled"
        } else {
            "disabled"
        },
        status.connected_viewers,
        status.pending_approvals,
    )
}

fn local_host_badge_text(model: &ViewModel) -> String {
    if !model.local_host_controls_available {
        return if model.core.hosting_enabled {
            "HOST DEMO ON".to_owned()
        } else {
            "VIEWER DEMO".to_owned()
        };
    }
    let Some(status) = &model.local_host_status else {
        return "AGENT OFFLINE".to_owned();
    };
    match status.helper_state {
        racc_core::ipc::HelperState::Stopped => "HOST STOPPED".to_owned(),
        racc_core::ipc::HelperState::Starting => "HOST STARTING".to_owned(),
        racc_core::ipc::HelperState::Running => "HOST RUNNING".to_owned(),
        racc_core::ipc::HelperState::Recovering => "HOST RECOVERING".to_owned(),
        racc_core::ipc::HelperState::NeedsAttention => "HOST ATTENTION".to_owned(),
    }
}

fn local_panel(model: &ViewModel, live: bool, live_input_ready: bool) -> Element<'static, Message> {
    let hosting = model.core.hosting_enabled;
    let local_status = local_host_badge_text(model);
    let status_color = if model
        .local_host_status
        .as_ref()
        .is_some_and(|status| status.helper_state == racc_core::ipc::HelperState::Running)
        || (!model.local_host_controls_available && hosting)
    {
        tokens::ONLINE
    } else {
        tokens::MUTED
    };
    container(
        column![
            row![
                section_label("THIS DEVICE"),
                iced::widget::Space::new().width(Fill),
                status_pill(&local_status, status_color),
            ]
            .align_y(iced::Alignment::Center),
            text(model.core.local_device_name.clone()).size(tokens::BODY_SIZE),
            row![
                action_button(
                    if model.keyboard_capture_active {
                        "⌨ Keys on"
                    } else if model.keyboard_capture {
                        "⌨ Keys armed"
                    } else {
                        "⌨ Keys"
                    },
                    (!live || live_input_ready || model.keyboard_capture).then_some(
                        Message::Action(UserAction::SetKeyboardCapture(!model.keyboard_capture,)),
                    ),
                    model.keyboard_capture,
                ),
                action_button(
                    if model.mouse_capture_active {
                        "⌖ Pointer on"
                    } else if model.mouse_capture {
                        "⌖ Pointer armed"
                    } else {
                        "⌖ Pointer"
                    },
                    (!live || live_input_ready || model.mouse_capture).then_some(Message::Action(
                        UserAction::SetMouseCapture(!model.mouse_capture,)
                    ),),
                    model.mouse_capture,
                ),
            ]
            .spacing(tokens::SPACE_1),
            row![
                tooltip(
                    button(text("Audio  ·  not supported").size(tokens::META_SIZE))
                        .padding(tokens::SPACE_1)
                        .style(|_theme: &Theme, _status| button::Style {
                            background: Some(Background::Color(tokens::SURFACE)),
                            text_color: tokens::MUTED,
                            border: Border {
                                color: tokens::BORDER,
                                width: tokens::BORDER_WIDTH,
                                radius: tokens::RADIUS_SMALL.into(),
                            },
                            ..Default::default()
                        }),
                    text("Audio is not supported"),
                    iced::widget::tooltip::Position::Top,
                ),
                iced::widget::Space::new().width(Fill),
                action_button(
                    "Settings",
                    Some(Message::Action(UserAction::Settings)),
                    model.page == Page::Settings,
                ),
            ]
            .align_y(iced::Alignment::Center),
        ]
        .spacing(tokens::SPACE_2),
    )
    .padding(tokens::SPACE_2)
    .style(panel_style(tokens::CARD))
    .into()
}

fn compact_local_panel(
    model: &ViewModel,
    live: bool,
    live_input_ready: bool,
) -> Element<'static, Message> {
    let local_label = format!(
        "{} · {}",
        model.core.local_device_name,
        local_host_status_text(model)
    );
    let audio = tooltip(
        button(text("♪"))
            .padding(tokens::SPACE_1)
            .style(|_theme: &Theme, _status| button::Style {
                text_color: tokens::MUTED,
                ..Default::default()
            }),
        text("Audio is not supported"),
        iced::widget::tooltip::Position::Right,
    );
    column![
        tooltip(
            action_button("▣", None, false),
            text(local_label),
            iced::widget::tooltip::Position::Right,
        ),
        tooltip(
            action_button(
                "⌨",
                (!live || live_input_ready || model.keyboard_capture).then_some(Message::Action(
                    UserAction::SetKeyboardCapture(!model.keyboard_capture)
                )),
                model.keyboard_capture,
            ),
            text(if model.keyboard_capture_active {
                "Keyboard capture active"
            } else if model.keyboard_capture {
                "Keyboard capture armed; focus the viewer to resume"
            } else if live && !live_input_ready {
                "Keyboard capture is available when the stream is ready"
            } else {
                "Keyboard capture"
            }),
            iced::widget::tooltip::Position::Right,
        ),
        tooltip(
            action_button(
                "⌖",
                (!live || live_input_ready || model.mouse_capture).then_some(Message::Action(
                    UserAction::SetMouseCapture(!model.mouse_capture)
                )),
                model.mouse_capture,
            ),
            text(if live {
                "Pointer capture uses the rendered video surface bounds"
            } else {
                "Pointer capture"
            }),
            iced::widget::tooltip::Position::Right,
        ),
        audio,
        tooltip(
            action_button(
                "⚙",
                Some(Message::Action(UserAction::Settings)),
                model.page == Page::Settings,
            ),
            text("Settings"),
            iced::widget::tooltip::Position::Right,
        ),
    ]
    .spacing(tokens::SPACE_1)
    .into()
}
fn quality_controls(
    device_id: Option<DeviceId>,
    selected_quality: QualityPreset,
    is_default: bool,
) -> Element<'static, Message> {
    let heading = if is_default {
        "DEFAULT QUALITY"
    } else {
        "QUALITY"
    };
    let mut controls = row![section_label(heading)]
        .spacing(tokens::SPACE_1)
        .align_y(iced::Alignment::Center);
    for (quality, label) in [
        (QualityPreset::P480, "480p"),
        (QualityPreset::P720, "720p"),
        (QualityPreset::P1080, "1080p"),
        (QualityPreset::Auto, "Auto"),
    ] {
        let action = if is_default {
            Some(Message::Action(UserAction::SetDefaultQuality(quality)))
        } else {
            device_id
                .as_ref()
                .map(|_| Message::Action(UserAction::SetQuality(quality)))
        };
        let selected = selected_quality == quality;
        controls = controls.push(action_button(label, action, selected));
    }
    controls.into()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TopologyEmptyStatus {
    Offline,
    HostUnavailable,
    AwaitingTopology,
    RequestTopology,
}

fn topology_empty_status(online: bool, host_capable: bool, connected: bool) -> TopologyEmptyStatus {
    if !online {
        TopologyEmptyStatus::Offline
    } else if !host_capable {
        TopologyEmptyStatus::HostUnavailable
    } else if connected {
        TopologyEmptyStatus::AwaitingTopology
    } else {
        TopologyEmptyStatus::RequestTopology
    }
}

fn empty_session_widget(
    device: Option<&racc_core::DeviceSnapshot>,
    display: Option<&racc_core::DisplaySnapshot>,
    connection_state: racc_telemetry::ConnectionState,
) -> Element<'static, Message> {
    let connected = connection_state == racc_telemetry::ConnectionState::Connected;
    let (state, color, title, detail, action) = match (device, display) {
        (None, _) => (
            "READY TO VIEW",
            tokens::ACCENT,
            "Choose a computer",
            "Select a computer from the rail, or browse your tailnet to find an available host.",
            Some(("Browse computers", Message::Action(UserAction::Home))),
        ),
        (Some(device), None) if device.displays.is_empty() => {
            match topology_empty_status(device.online, device.host_capable, connected) {
                TopologyEmptyStatus::Offline => (
                    "COMPUTER OFFLINE",
                    tokens::OFFLINE,
                    "This computer is offline",
                    "Reconnect it to Tailscale, then refresh the device list to request its displays.",
                    Some((
                        "Refresh devices",
                        Message::Action(UserAction::DiscoverDevices),
                    )),
                ),
                TopologyEmptyStatus::HostUnavailable => (
                    "HOST UNAVAILABLE",
                    tokens::MUTED,
                    "Host agent not responding",
                    "The computer is online, but its Racc Connect host agent has not answered yet.",
                    Some((
                        "Refresh devices",
                        Message::Action(UserAction::DiscoverDevices),
                    )),
                ),
                TopologyEmptyStatus::AwaitingTopology => (
                    "TOPOLOGY PENDING",
                    tokens::ACCENT,
                    "Waiting for display information",
                    "The session is connected. The host has not reported its monitors yet.",
                    None,
                ),
                TopologyEmptyStatus::RequestTopology => (
                    "TOPOLOGY PENDING",
                    tokens::ACCENT,
                    "Request this computer’s displays",
                    "Connect to ask the host for its current monitors. The display list will appear here when the host responds.",
                    Some((
                        "Connect to computer",
                        Message::Action(UserAction::Connect(device.id.clone())),
                    )),
                ),
            }
        }
        (Some(_), None) => (
            "DISPLAY REQUIRED",
            tokens::MUTED,
            "Choose a display",
            "Select one of this computer’s displays from the monitor list above.",
            None,
        ),
        (Some(device), Some(_)) if !device.online => (
            "COMPUTER OFFLINE",
            tokens::OFFLINE,
            "This computer is offline",
            "Reconnect it to Tailscale, then refresh the device list to try again.",
            Some((
                "Refresh devices",
                Message::Action(UserAction::DiscoverDevices),
            )),
        ),
        (Some(device), Some(_)) if !device.host_capable => (
            "HOST UNAVAILABLE",
            tokens::MUTED,
            "Host agent not found",
            "The computer is online, but its Racc Connect host agent did not answer.",
            Some((
                "Refresh devices",
                Message::Action(UserAction::DiscoverDevices),
            )),
        ),
        (Some(_), Some(display)) if !display.available => (
            "DISPLAY OFFLINE",
            tokens::OFFLINE,
            "This display is unavailable",
            "Choose another active display from the monitor list above.",
            None,
        ),
        (Some(_), Some(_)) if connected => (
            "STARTING STREAM",
            tokens::ACCENT,
            "Waiting for the first frame",
            "The connection is ready. The live display will appear here as soon as video arrives.",
            None,
        ),
        (Some(device), Some(_)) => (
            "READY TO CONNECT",
            tokens::ACCENT,
            "Your session is ready",
            "Connect to begin viewing this display. Video and controls stay inside this workspace.",
            Some((
                "Connect to computer",
                Message::Action(UserAction::Connect(device.id.clone())),
            )),
        ),
    };
    let mut content = column![
        status_pill(state, color),
        text(title).size(tokens::HEADER_SIZE),
        text(detail).size(tokens::BODY_SIZE).color(tokens::MUTED),
    ]
    .spacing(tokens::SPACE_2)
    .align_x(iced::Alignment::Center);
    if let Some((label, message)) = action {
        content = content.push(action_button(label, Some(message), false));
    }
    let card = container(content)
        .max_width(tokens::EMPTY_STATE_MAX_WIDTH)
        .padding(tokens::SPACE_4)
        .style(panel_style(tokens::SURFACE));
    container(card)
        .width(Fill)
        .height(Fill)
        .center_x(Fill)
        .center_y(Fill)
        .into()
}

fn overlay_message(overlay: &SessionOverlay) -> Option<String> {
    match overlay {
        SessionOverlay::None => None,
        SessionOverlay::Connecting => Some("Connecting…".to_owned()),
        SessionOverlay::Switching => {
            Some("Switching display  ·  holding the last frame".to_owned())
        }
        SessionOverlay::Paused => Some("Video paused while the window is hidden".to_owned()),
        SessionOverlay::Reconnecting => {
            Some("Connection lost  ·  retrying automatically".to_owned())
        }
        SessionOverlay::WaitingForApproval { device_name, .. } => {
            Some(format!("Waiting for approval from {device_name}"))
        }
        SessionOverlay::Error(detail) => Some(format!("Stream error  ·  {detail}")),
    }
}

fn debug_latency_summary(frame: Option<&VideoFrame>, observed_interval_us: Option<u64>) -> String {
    let Some(frame) = frame else {
        return "DEBUG · waiting for a decoded frame · capture age unavailable (host/viewer clocks are not synchronized)".to_owned();
    };
    let capture = frame.capture_ts_us.map_or_else(
        || "unavailable".to_owned(),
        |timestamp| format!("{timestamp} µs in host clock"),
    );
    let decode = frame.decode_duration_us.map_or_else(
        || "unavailable".to_owned(),
        |duration| format!("{duration} µs decoder call"),
    );
    let interval = observed_interval_us.map_or_else(
        || "waiting for next frame".to_owned(),
        |duration| {
            format!(
                "{:.2} ms app-observed frame interval",
                duration as f64 / 1_000.0
            )
        },
    );
    format!(
        "DEBUG · frame {} · capture age unavailable (host/viewer clocks are not synchronized) · capture {capture} · decode {decode} · {interval} (not a physical present interval)",
        frame.frame_id
    )
}

fn overlay_widget(overlay: &SessionOverlay) -> Element<'static, Message> {
    let Some(message) = overlay_message(overlay) else {
        return iced::widget::Space::new().width(Fill).height(Fill).into();
    };
    container(text(message).size(tokens::BODY_SIZE))
        .padding(tokens::SPACE_3)
        .style(panel_style(tokens::CARD))
        .center_x(Fill)
        .center_y(Fill)
        .into()
}

fn session_status(
    overlay: &SessionOverlay,
    has_device: bool,
    device_online: bool,
    has_display: bool,
    display_available: bool,
    has_stream: bool,
    connection_state: racc_telemetry::ConnectionState,
) -> (&'static str, Color) {
    let connected = connection_state == racc_telemetry::ConnectionState::Connected;
    match overlay {
        SessionOverlay::Connecting => ("CONNECTING", tokens::ACCENT),
        SessionOverlay::Switching => ("SWITCHING", tokens::ACCENT),
        SessionOverlay::Paused => ("PAUSED", tokens::MUTED),
        SessionOverlay::Reconnecting => ("RECONNECTING", tokens::OFFLINE),
        SessionOverlay::WaitingForApproval { .. } => ("APPROVAL", tokens::ACCENT),
        SessionOverlay::Error(_) => ("ATTENTION", tokens::OFFLINE),
        SessionOverlay::None if !has_device => ("NO DEVICE", tokens::MUTED),
        SessionOverlay::None if !device_online => ("OFFLINE", tokens::OFFLINE),
        SessionOverlay::None if !has_display => ("SELECT DISPLAY", tokens::MUTED),
        SessionOverlay::None if !display_available => ("DISPLAY OFFLINE", tokens::OFFLINE),
        SessionOverlay::None if connected && has_stream => ("LIVE", tokens::ONLINE),
        SessionOverlay::None if connected => ("STARTING", tokens::ACCENT),
        SessionOverlay::None => ("READY", tokens::ACCENT),
    }
}
fn status_pill(label: &str, color: Color) -> Element<'static, Message> {
    container(
        row![
            text("●").size(tokens::META_SIZE).color(color),
            text(label.to_owned())
                .size(tokens::META_SIZE)
                .color(tokens::TEXT),
        ]
        .spacing(tokens::SPACE_1)
        .align_y(iced::Alignment::Center),
    )
    .padding(tokens::SPACE_1)
    .style(panel_style(tokens::ACCENT_WASH))
    .into()
}

fn metric_card(label: &str, value: String, color: Color) -> Element<'static, Message> {
    container(
        column![
            muted_text(label),
            text(value).size(tokens::METRIC_SIZE).color(color),
        ]
        .spacing(tokens::SPACE_1),
    )
    .width(Fill)
    .padding(tokens::SPACE_2)
    .style(panel_style(tokens::CARD))
    .into()
}

fn telemetry_line(label: &str, value: String) -> Element<'static, Message> {
    row![
        muted_text(label),
        iced::widget::Space::new().width(Fill),
        text(value).size(tokens::META_SIZE)
    ]
    .spacing(tokens::SPACE_2)
    .align_y(iced::Alignment::Center)
    .into()
}
fn divider_label(label: &str) -> Element<'static, Message> {
    text(label.to_owned())
        .size(tokens::SECTION_SIZE)
        .color(tokens::MUTED)
        .into()
}
fn section_label(label: &str) -> Element<'static, Message> {
    text(label.to_owned())
        .size(tokens::SECTION_SIZE)
        .color(tokens::ACCENT)
        .into()
}
fn muted_text(label: &str) -> Element<'static, Message> {
    text(label.to_owned())
        .size(tokens::SECTION_SIZE)
        .color(tokens::MUTED)
        .into()
}
#[track_caller]
fn action_button(
    label: &str,
    action: Option<Message>,
    selected: bool,
) -> Element<'static, Message> {
    let caller = std::panic::Location::caller();
    let focus_id = format!(
        "{}:{}:{}:{label}:{action:?}",
        caller.file(),
        caller.line(),
        caller.column()
    );
    action_button_widget(text(label.to_owned()), action, selected, focus_id)
}

fn action_button_widget<'a>(
    content: impl Into<Element<'a, Message>>,
    action: Option<Message>,
    selected: bool,
    focus_id: String,
) -> Element<'a, Message> {
    let accessible_action = action.clone();
    FocusableButton::wrap(
        button(content)
            .padding(tokens::SPACE_2)
            .style(move |_theme: &Theme, status| {
                let (background, border_color, border_width) = match status {
                    button::Status::Hovered => {
                        (tokens::HOVER, tokens::ACCENT, tokens::BORDER_WIDTH)
                    }
                    button::Status::Pressed => {
                        (tokens::ACCENT_FILL, tokens::ACCENT, tokens::BORDER_WIDTH)
                    }
                    button::Status::Disabled if selected => {
                        (tokens::SELECTED, tokens::ACCENT, tokens::BORDER_WIDTH)
                    }
                    _ if selected => (tokens::SELECTED, tokens::ACCENT, tokens::BORDER_WIDTH),
                    _ => (Color::TRANSPARENT, Color::TRANSPARENT, 0.0),
                };
                button::Style {
                    background: Some(Background::Color(background)),
                    text_color: if status == button::Status::Disabled {
                        tokens::MUTED
                    } else {
                        tokens::TEXT
                    },
                    border: Border {
                        color: border_color,
                        width: border_width,
                        radius: if selected {
                            tokens::RADIUS_MEDIUM
                        } else {
                            tokens::RADIUS_SMALL
                        }
                        .into(),
                    },
                    ..Default::default()
                }
            })
            .on_press_maybe(action),
        accessible_action,
        iced::advanced::widget::Id::from(focus_id),
    )
    .into()
}

fn danger_button(label: &str, action: Option<Message>) -> Element<'static, Message> {
    let action_for_focus = action.clone();
    FocusableButton::wrap(
        button(text(label.to_owned()))
            .padding(tokens::SPACE_2)
            .style(|_theme: &Theme, status| {
                let background = match status {
                    button::Status::Hovered | button::Status::Pressed => tokens::DANGER,
                    _ => tokens::ACCENT_WASH,
                };
                button::Style {
                    background: Some(Background::Color(background)),
                    text_color: tokens::TEXT,
                    border: Border {
                        color: tokens::DANGER,
                        width: tokens::BORDER_WIDTH,
                        radius: tokens::RADIUS_MEDIUM.into(),
                    },
                    ..Default::default()
                }
            })
            .on_press_maybe(action),
        action_for_focus,
        iced::advanced::widget::Id::from(format!("danger:{label}")),
    )
    .into()
}
fn scroll_style(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let mut style = scrollable::default(theme, status);
    for rail in [&mut style.vertical_rail, &mut style.horizontal_rail] {
        rail.background = Some(Background::Color(tokens::MAIN));
        rail.border.color = tokens::BORDER;
        rail.scroller.background = Background::Color(tokens::MUTED);
        rail.scroller.border.color = tokens::BORDER;
    }
    style
}

fn panel_style(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_theme| container::Style {
        background: Some(Background::Color(color)),
        text_color: Some(tokens::TEXT),
        border: Border {
            color: tokens::BORDER,
            width: tokens::BORDER_WIDTH,
            radius: tokens::RADIUS_LARGE.into(),
        },
        ..Default::default()
    }
}

#[derive(Default)]
struct VideoInputState {
    buttons: std::collections::BTreeSet<u8>,
    last_position: Option<(i64, i64)>,
    active: bool,
}

struct VideoInputProgram {
    source: Arc<dyn FrameSource>,
    enabled: bool,
}

impl iced::widget::canvas::Program<Message> for VideoInputProgram {
    type State = VideoInputState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> Option<iced::widget::canvas::Action<Message>> {
        if state.active != self.enabled {
            state.buttons.clear();
            state.last_position = None;
            state.active = self.enabled;
        }
        if !self.enabled {
            return None;
        }
        let frame = self.source.latest_frame();
        let (stream_width, stream_height) = frame
            .as_deref()
            .map(|frame| (u32::from(frame.width), u32::from(frame.height)))
            .unwrap_or((1_280, 720));
        let rect = viewer_pointer::rendered_video_rect(bounds, stream_width, stream_height)?;
        let global_position = cursor.position();
        let local_position =
            global_position.map(|point| viewer_pointer::local_point(bounds, point));
        if let Some(position) = local_position {
            state.last_position = Some(position);
        }
        let inside_bounds = global_position.is_some_and(|point| bounds.contains(point));
        let (px, py) = local_position.or(state.last_position)?;

        let input = match event {
            iced::Event::Mouse(iced::mouse::Event::CursorMoved { .. })
                if inside_bounds || !state.buttons.is_empty() =>
            {
                Some(ViewerMouseInput::Move {
                    px,
                    py,
                    rendered_rect: rect,
                })
            }
            iced::Event::Mouse(iced::mouse::Event::ButtonPressed(button)) if inside_bounds => {
                let button = iced_mouse_button(*button)?;
                state.buttons.insert(button);
                Some(ViewerMouseInput::Button {
                    button,
                    pressed: true,
                    px,
                    py,
                    rendered_rect: rect,
                })
            }
            iced::Event::Mouse(iced::mouse::Event::ButtonReleased(button)) => {
                let button = iced_mouse_button(*button)?;
                if state.buttons.remove(&button) {
                    Some(ViewerMouseInput::Button {
                        button,
                        pressed: false,
                        px,
                        py,
                        rendered_rect: rect,
                    })
                } else {
                    None
                }
            }
            iced::Event::Mouse(iced::mouse::Event::WheelScrolled { delta }) if inside_bounds => {
                let (dx, dy) = match delta {
                    iced::mouse::ScrollDelta::Lines { x, y } => {
                        (wheel_units(*x, 120.0), wheel_units(*y, 120.0))
                    }
                    iced::mouse::ScrollDelta::Pixels { x, y } => {
                        (wheel_units(*x, 1.0), wheel_units(*y, 1.0))
                    }
                };
                Some(ViewerMouseInput::Wheel { dx, dy })
            }
            _ => None,
        }?;
        Some(iced::widget::canvas::Action::publish(Message::VideoMouse(input)).and_capture())
    }

    fn draw(
        &self,
        _state: &Self::State,
        _renderer: &iced::Renderer,
        _theme: &Theme,
        _bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<iced::widget::canvas::Geometry> {
        Vec::new()
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> iced::mouse::Interaction {
        if self.enabled && cursor.is_over(bounds) {
            iced::mouse::Interaction::Pointer
        } else {
            iced::mouse::Interaction::default()
        }
    }
}

fn iced_mouse_button(button: iced::mouse::Button) -> Option<u8> {
    match button {
        iced::mouse::Button::Left => Some(1),
        iced::mouse::Button::Right => Some(2),
        iced::mouse::Button::Middle => Some(3),
        iced::mouse::Button::Back => Some(4),
        iced::mouse::Button::Forward => Some(5),
        iced::mouse::Button::Other(_) => None,
    }
}

fn wheel_units(value: f32, scale: f32) -> i16 {
    if value.is_finite() {
        (value * scale)
            .round()
            .clamp(i16::MIN as f32, i16::MAX as f32) as i16
    } else {
        0
    }
}

struct VideoProgram {
    source: Arc<dyn FrameSource>,
    cursor_cache: Arc<Mutex<viewer_cursor::CursorOverlayCache>>,
}
impl std::fmt::Debug for VideoProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VideoProgram")
    }
}
impl iced::widget::shader::Program<Message> for VideoProgram {
    type State = ();
    type Primitive = VideoPrimitive;
    fn draw(&self, _state: &Self::State, _cursor: Cursor, _bounds: Rectangle) -> Self::Primitive {
        VideoPrimitive {
            source: Arc::clone(&self.source),
            cursor_cache: Arc::clone(&self.cursor_cache),
        }
    }
}

struct VideoPrimitive {
    source: Arc<dyn FrameSource>,
    cursor_cache: Arc<Mutex<viewer_cursor::CursorOverlayCache>>,
}
impl std::fmt::Debug for VideoPrimitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VideoPrimitive")
    }
}
impl iced::widget::shader::Primitive for VideoPrimitive {
    type Pipeline = VideoPipeline;
    fn prepare(
        &self,
        pipeline: &mut Self::Pipeline,
        _device: &iced::wgpu::Device,
        queue: &iced::wgpu::Queue,
        bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
        let frame = self.source.latest_frame();
        let (width, height, frame_number, seed) =
            frame
                .as_deref()
                .map_or((1_280, 720, 0_u64, 0_u64), |frame| {
                    let (number, seed) = match &frame.payload {
                        FramePayload::SyntheticPattern { seed, frame_number } => {
                            (*frame_number, *seed)
                        }
                        FramePayload::Nv12(_) => (u64::from(frame.frame_id), 0),
                    };
                    (
                        u32::from(frame.width),
                        u32::from(frame.height),
                        number,
                        seed,
                    )
                });

        let is_synthetic = frame
            .as_deref()
            .is_some_and(|frame| matches!(&frame.payload, FramePayload::SyntheticPattern { .. }));
        let mut has_nv12 = false;
        if let Some(frame) = frame.as_deref() {
            if frame.validate().is_ok()
                && frame.width.is_multiple_of(2)
                && frame.height.is_multiple_of(2)
                && u32::from(frame.width) <= VIDEO_MAX_WIDTH
                && u32::from(frame.height) <= VIDEO_MAX_HEIGHT
            {
                if let FramePayload::Nv12(nv12) = &frame.payload {
                    queue.write_texture(
                        iced::wgpu::TexelCopyTextureInfo {
                            texture: &pipeline.y_texture,
                            mip_level: 0,
                            origin: iced::wgpu::Origin3d::ZERO,
                            aspect: iced::wgpu::TextureAspect::All,
                        },
                        nv12.y.as_ref(),
                        iced::wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(nv12.y_stride),
                            rows_per_image: Some(u32::from(frame.height)),
                        },
                        iced::wgpu::Extent3d {
                            width: u32::from(frame.width),
                            height: u32::from(frame.height),
                            depth_or_array_layers: 1,
                        },
                    );
                    queue.write_texture(
                        iced::wgpu::TexelCopyTextureInfo {
                            texture: &pipeline.uv_texture,
                            mip_level: 0,
                            origin: iced::wgpu::Origin3d::ZERO,
                            aspect: iced::wgpu::TextureAspect::All,
                        },
                        nv12.uv.as_ref(),
                        iced::wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(nv12.uv_stride),
                            rows_per_image: Some(u32::from(frame.height / 2)),
                        },
                        iced::wgpu::Extent3d {
                            width: u32::from(frame.width / 2),
                            height: u32::from(frame.height / 2),
                            depth_or_array_layers: 1,
                        },
                    );
                    has_nv12 = true;
                }
            }
        }

        let panel_width = bounds.width.round().max(1.0) as u32;
        let panel_height = bounds.height.round().max(1.0) as u32;
        let content = design::video_rect(panel_width, panel_height, width.max(1), height.max(1))
            .ok()
            .flatten();
        let content_rect = content
            .or_else(|| racc_topology::RenderedRect::new(0, 0, panel_width, panel_height).ok());
        let (x, y, content_width, content_height) = content_rect.map_or(
            (0.0, 0.0, panel_width as f32, panel_height as f32),
            |rect| {
                let (x, y) = rect.origin();
                let (width, height) = rect.size();
                (x as f32, y as f32, width as f32, height as f32)
            },
        );
        let cursor = frame.as_deref().and_then(|frame| {
            let display_id = frame.display_id?;
            self.cursor_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .for_frame(frame.epoch, display_id.get())
        });
        let cursor_rect = cursor
            .as_ref()
            .zip(content_rect)
            .and_then(|(cursor, content)| viewer_cursor::rendered_cursor_rect(cursor, content));
        if let Some(cursor) = &cursor {
            let texture_key = (cursor.shape_id, cursor.revision);
            if pipeline.uploaded_cursor != Some(texture_key) {
                queue.write_texture(
                    iced::wgpu::TexelCopyTextureInfo {
                        texture: &pipeline.cursor_texture,
                        mip_level: 0,
                        origin: iced::wgpu::Origin3d::ZERO,
                        aspect: iced::wgpu::TextureAspect::All,
                    },
                    cursor.rgba.as_ref(),
                    iced::wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(cursor.width * 4),
                        rows_per_image: Some(cursor.height),
                    },
                    iced::wgpu::Extent3d {
                        width: cursor.width,
                        height: cursor.height,
                        depth_or_array_layers: 1,
                    },
                );
                pipeline.uploaded_cursor = Some(texture_key);
            }
        }
        let frame_kind = if has_nv12 {
            1
        } else if is_synthetic {
            2
        } else {
            0
        };
        let mut values = [0_u32; 28];
        values[..16].copy_from_slice(&[
            bounds.width.to_bits(),
            bounds.height.to_bits(),
            frame_number as u32,
            seed as u32,
            x.to_bits(),
            y.to_bits(),
            content_width.to_bits(),
            content_height.to_bits(),
            width,
            height,
            frame_kind,
            0,
            0,
            0,
            0,
            0,
        ]);
        if let (Some(cursor), Some(rect)) = (cursor.as_ref(), cursor_rect) {
            values[16..20].copy_from_slice(&[
                rect.left.to_bits(),
                rect.top.to_bits(),
                rect.width.to_bits(),
                rect.height.to_bits(),
            ]);
            values[20..24].copy_from_slice(&[
                cursor.width,
                cursor.height,
                cursor.hotspot_x,
                cursor.hotspot_y,
            ]);
            values[24] = cursor.blend_mode as u32;
            values[25] = 1;
        }
        let mut bytes = [0_u8; 112];
        for (index, value) in values.iter().enumerate() {
            bytes[index * 4..(index + 1) * 4].copy_from_slice(&value.to_ne_bytes());
        }
        queue.write_buffer(&pipeline.uniform_buffer, 0, &bytes);
    }
    fn draw(&self, pipeline: &Self::Pipeline, pass: &mut iced::wgpu::RenderPass<'_>) -> bool {
        pass.set_pipeline(&pipeline.render_pipeline);
        pass.set_bind_group(0, &pipeline.bind_group, &[]);
        pass.draw(0..3, 0..1);
        true
    }
}

const VIDEO_MAX_WIDTH: u32 = 1920;
const VIDEO_MAX_HEIGHT: u32 = 1080;

struct VideoPipeline {
    render_pipeline: iced::wgpu::RenderPipeline,
    bind_group: iced::wgpu::BindGroup,
    uniform_buffer: iced::wgpu::Buffer,
    y_texture: iced::wgpu::Texture,
    uv_texture: iced::wgpu::Texture,
    cursor_texture: iced::wgpu::Texture,
    uploaded_cursor: Option<(u32, u64)>,
}
impl iced::widget::shader::Pipeline for VideoPipeline {
    fn new(
        device: &iced::wgpu::Device,
        _queue: &iced::wgpu::Queue,
        format: iced::wgpu::TextureFormat,
    ) -> Self {
        let uniform_buffer = device.create_buffer(&iced::wgpu::BufferDescriptor {
            label: Some("video frame metadata"),
            size: 112,
            usage: iced::wgpu::BufferUsages::UNIFORM | iced::wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&iced::wgpu::BindGroupLayoutDescriptor {
            label: Some("NV12 video bindings"),
            entries: &[
                iced::wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: iced::wgpu::ShaderStages::FRAGMENT,
                    ty: iced::wgpu::BindingType::Buffer {
                        ty: iced::wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                iced::wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: iced::wgpu::ShaderStages::FRAGMENT,
                    ty: iced::wgpu::BindingType::Texture {
                        sample_type: iced::wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: iced::wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                iced::wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: iced::wgpu::ShaderStages::FRAGMENT,
                    ty: iced::wgpu::BindingType::Texture {
                        sample_type: iced::wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: iced::wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                iced::wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: iced::wgpu::ShaderStages::FRAGMENT,
                    ty: iced::wgpu::BindingType::Texture {
                        sample_type: iced::wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: iced::wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let y_texture = device.create_texture(&iced::wgpu::TextureDescriptor {
            label: Some("NV12 luma plane"),
            size: iced::wgpu::Extent3d {
                width: VIDEO_MAX_WIDTH,
                height: VIDEO_MAX_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: iced::wgpu::TextureDimension::D2,
            format: iced::wgpu::TextureFormat::R8Unorm,
            usage: iced::wgpu::TextureUsages::TEXTURE_BINDING | iced::wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let uv_texture = device.create_texture(&iced::wgpu::TextureDescriptor {
            label: Some("NV12 chroma plane"),
            size: iced::wgpu::Extent3d {
                width: VIDEO_MAX_WIDTH / 2,
                height: VIDEO_MAX_HEIGHT / 2,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: iced::wgpu::TextureDimension::D2,
            format: iced::wgpu::TextureFormat::Rg8Unorm,
            usage: iced::wgpu::TextureUsages::TEXTURE_BINDING | iced::wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let cursor_texture = device.create_texture(&iced::wgpu::TextureDescriptor {
            label: Some("remote cursor BGRA bitmap"),
            size: iced::wgpu::Extent3d {
                width: racc_proto::MAX_CURSOR_DIM as u32,
                height: racc_proto::MAX_CURSOR_DIM as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: iced::wgpu::TextureDimension::D2,
            format: iced::wgpu::TextureFormat::Rgba8Unorm,
            usage: iced::wgpu::TextureUsages::TEXTURE_BINDING | iced::wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let y_view = y_texture.create_view(&iced::wgpu::TextureViewDescriptor::default());
        let uv_view = uv_texture.create_view(&iced::wgpu::TextureViewDescriptor::default());
        let cursor_view = cursor_texture.create_view(&iced::wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&iced::wgpu::BindGroupDescriptor {
            label: Some("NV12 video bind group"),
            layout: &layout,
            entries: &[
                iced::wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                iced::wgpu::BindGroupEntry {
                    binding: 1,
                    resource: iced::wgpu::BindingResource::TextureView(&y_view),
                },
                iced::wgpu::BindGroupEntry {
                    binding: 2,
                    resource: iced::wgpu::BindingResource::TextureView(&uv_view),
                },
                iced::wgpu::BindGroupEntry {
                    binding: 3,
                    resource: iced::wgpu::BindingResource::TextureView(&cursor_view),
                },
            ],
        });
        let shader = device.create_shader_module(iced::wgpu::ShaderModuleDescriptor {
            label: Some("native NV12 video shader"),
            source: iced::wgpu::ShaderSource::Wgsl(VIDEO_SHADER.into()),
        });
        let pipeline_layout =
            device.create_pipeline_layout(&iced::wgpu::PipelineLayoutDescriptor {
                label: Some("video layout"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
        let render_pipeline =
            device.create_render_pipeline(&iced::wgpu::RenderPipelineDescriptor {
                label: Some("native NV12 video surface"),
                layout: Some(&pipeline_layout),
                vertex: iced::wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(iced::wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(iced::wgpu::ColorTargetState {
                        format,
                        blend: Some(iced::wgpu::BlendState::REPLACE),
                        write_mask: iced::wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: iced::wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: iced::wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            });
        Self {
            render_pipeline,
            bind_group,
            uniform_buffer,
            y_texture,
            uv_texture,
            cursor_texture,
            uploaded_cursor: None,
        }
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<LaunchOptions, String> {
        parse_launch_options(arguments.iter().map(|value| (*value).to_owned()))
    }

    #[test]
    fn quality_preference_mapping_round_trips_every_tier() {
        for preference in [
            QualityPreference::Auto,
            QualityPreference::P480,
            QualityPreference::P720,
            QualityPreference::P1080,
        ] {
            assert_eq!(
                preference_from_quality(quality_from_preference(preference)),
                preference
            );
        }
    }

    #[test]
    fn debug_latency_summary_keeps_clock_domains_and_present_limits_explicit() {
        let frame = VideoFrame {
            epoch: 3,
            frame_id: 41,
            display_id: None,
            width: 4,
            height: 4,
            fps: 30,
            capture_ts_us: Some(123_456),
            decode_duration_us: Some(789),
            payload: FramePayload::SyntheticPattern {
                seed: 0,
                frame_number: 41,
            },
        };
        let summary = debug_latency_summary(Some(&frame), Some(33_333));
        assert!(summary.contains("frame 41"));
        assert!(summary.contains("123456 µs in host clock"));
        assert!(summary.contains("789 µs decoder call"));
        assert!(summary.contains("33.33 ms app-observed frame interval"));
        assert!(summary.contains("capture age unavailable"));
        assert!(summary.contains("not a physical present interval"));
    }

    #[test]
    fn debug_latency_summary_handles_missing_frame_timing() {
        let summary = debug_latency_summary(None, None);
        assert!(summary.contains("waiting for a decoded frame"));
        assert!(summary.contains("clocks are not synchronized"));
    }

    #[test]
    fn settings_notices_never_show_personal_file_paths() {
        let notice = LoadNotice::CorruptFileQuarantined {
            quarantined_path: std::path::PathBuf::from(r"C:\RaccTest\settings.json"),
        };
        let display = load_notice_text(notice);
        assert!(!display.contains("owner"));
        assert!(!display.contains("AppData"));
    }

    #[test]
    fn about_page_embeds_generated_dependency_and_artwork_notices() {
        assert!(include_str!("../../../THIRD_PARTY_LICENSES.md")
            .contains("Third-party license notices"));
        assert!(include_str!("../../../docs/ASSETS.md").contains("Raccoon mark"));
    }

    #[test]
    fn normal_launch_defaults_to_discovery_and_explicit_discovery_is_supported() {
        assert!(matches!(parse(&[]).unwrap().mode, LaunchMode::Discover));
        assert!(matches!(
            parse(&["--discover"]).unwrap().mode,
            LaunchMode::Discover
        ));
        assert!(matches!(
            parse(&["--discover", "--telemetry-collapsed"])
                .unwrap()
                .mode,
            LaunchMode::Discover
        ));
        assert!(parse(&["--discover", "--bind", "100.100.10.21"]).is_err());
        assert!(parse(&["--discover", "--fake"]).is_err());
    }

    #[test]
    fn fake_mode_keeps_its_existing_flags() {
        let options = parse(&["--fake", "--fake-idle", "--telemetry-collapsed"]).unwrap();
        assert!(matches!(options.mode, LaunchMode::Fake { idle: true }));
        assert!(options.telemetry_collapsed);
    }

    #[test]
    fn live_mode_supports_frame_pacing_measurements_and_sidebar_override() {
        let options = parse(&[
            "--connect",
            "100.100.10.20:54831",
            "--bind",
            "100.100.10.21",
            "--measure-secs=60",
            "--telemetry-expanded",
        ])
        .unwrap();
        assert_eq!(options.measure_secs, Some(60));
        assert!(options.telemetry_expanded);
        assert!(parse(&[
            "--connect",
            "100.100.10.20:54831",
            "--bind",
            "100.100.10.21",
            "--measure-secs=60",
            "--telemetry-expanded",
            "--telemetry-collapsed",
        ])
        .is_err());
        assert!(parse(&["--discover", "--measure-secs=60"]).is_err());
        assert!(parse(&[
            "--connect",
            "100.100.10.20:54831",
            "--bind",
            "100.100.10.21",
            "--measure-secs=0",
        ])
        .is_err());
    }

    #[test]
    fn debug_latency_overlay_is_opt_in_and_can_be_enabled_for_live_or_fake_mode() {
        assert!(!parse(&[]).unwrap().debug_latency);
        assert!(parse(&["--fake", "--debug-latency"]).unwrap().debug_latency);
        assert!(
            parse(&[
                "--connect",
                "100.100.10.20:54831",
                "--bind",
                "100.100.10.21",
                "--debug-latency",
            ])
            .unwrap()
            .debug_latency
        );
    }

    #[test]
    fn live_mode_accepts_tailnet_endpoints_only() {
        let options = parse(&[
            "--connect",
            "100.100.10.20:54831",
            "--bind",
            "100.100.10.21",
        ])
        .unwrap();
        assert!(matches!(
            options.mode,
            LaunchMode::Live {
                host_addr,
                local_bind_ip: IpAddr::V4(_),
            } if host_addr == "100.100.10.20:54831".parse().unwrap()
        ));

        let options = parse(&[
            "--connect",
            "[fd7a:115c:a1e0::20]:54831",
            "--bind",
            "fd7a:115c:a1e0::21",
        ])
        .unwrap();
        assert!(matches!(options.mode, LaunchMode::Live { .. }));
    }

    #[test]
    fn live_mode_rejects_untrusted_or_incomplete_endpoints() {
        for args in [
            &[
                "--connect",
                "viewer.example:54831",
                "--bind",
                "100.100.10.21",
            ][..],
            &["--connect", "192.0.2.20:54831", "--bind", "100.100.10.21"][..],
            &["--connect", "100.100.10.20:0", "--bind", "100.100.10.21"][..],
            &["--connect", "100.100.10.20:54831"][..],
            &[
                "--connect",
                "100.100.10.20:54831",
                "--bind",
                "fd7a:115c:a1e0::21",
            ][..],
        ] {
            assert!(parse(args).is_err(), "accepted invalid args: {args:?}");
        }
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn session_status_distinguishes_ready_live_and_unavailable_states() {
        let none = SessionOverlay::None;
        assert_eq!(
            session_status(
                &none,
                false,
                false,
                false,
                false,
                false,
                racc_telemetry::ConnectionState::Disconnected,
            )
            .0,
            "NO DEVICE"
        );
        assert_eq!(
            session_status(
                &none,
                true,
                false,
                true,
                true,
                false,
                racc_telemetry::ConnectionState::Disconnected,
            )
            .0,
            "OFFLINE"
        );
        assert_eq!(
            session_status(
                &none,
                true,
                true,
                false,
                false,
                false,
                racc_telemetry::ConnectionState::Disconnected,
            )
            .0,
            "SELECT DISPLAY"
        );
        assert_eq!(
            session_status(
                &none,
                true,
                true,
                true,
                true,
                false,
                racc_telemetry::ConnectionState::Disconnected,
            )
            .0,
            "READY"
        );
        assert_eq!(
            session_status(
                &none,
                true,
                true,
                true,
                true,
                true,
                racc_telemetry::ConnectionState::Connected,
            )
            .0,
            "LIVE"
        );
        assert_eq!(
            session_status(
                &SessionOverlay::Paused,
                true,
                true,
                true,
                true,
                true,
                racc_telemetry::ConnectionState::Connected,
            )
            .0,
            "PAUSED"
        );
        assert_eq!(
            session_status(
                &none,
                true,
                true,
                true,
                true,
                true,
                racc_telemetry::ConnectionState::Disconnected,
            )
            .0,
            "READY"
        );
    }

    #[test]
    fn topology_empty_status_distinguishes_offline_host_pending_and_connected() {
        assert_eq!(
            topology_empty_status(false, false, false),
            TopologyEmptyStatus::Offline
        );
        assert_eq!(
            topology_empty_status(true, false, false),
            TopologyEmptyStatus::HostUnavailable
        );
        assert_eq!(
            topology_empty_status(true, true, false),
            TopologyEmptyStatus::RequestTopology
        );
        assert_eq!(
            topology_empty_status(true, true, true),
            TopologyEmptyStatus::AwaitingTopology
        );
    }

    #[test]
    fn telemetry_event_timestamp_uses_monotonic_elapsed_time() {
        assert_eq!(event_timestamp_label(12_345_678), "T+00:12");
        assert_eq!(event_timestamp_label(3_661_000_000), "T+1:01:01");
    }

    #[test]
    fn telemetry_changes_rebuild_only_dependent_view_regions() {
        let fake = FakeCore::new(DEFAULT_FAKE_SEED);
        let snapshot = fake.snapshot().expect("fake snapshot");
        let mut model = ViewModel::new(snapshot);
        let initial = region_cache_keys(&model, false, false, None);

        model.core.telemetry.session.fps += 0.5;
        let stats_changed = region_cache_keys(&model, false, false, None);
        assert_eq!(initial.rail, stats_changed.rail);
        assert_eq!(initial.device_sidebar, stats_changed.device_sidebar);
        assert_eq!(initial.workspace, stats_changed.workspace);
        assert_ne!(initial.telemetry_sidebar, stats_changed.telemetry_sidebar);

        model.core.telemetry.session.last_rtt_us = Some(12_000);
        let rtt_changed = region_cache_keys(&model, false, false, None);
        assert_eq!(stats_changed.rail, rtt_changed.rail);
        assert_eq!(stats_changed.device_sidebar, rtt_changed.device_sidebar);
        assert_ne!(stats_changed.workspace, rtt_changed.workspace);
        assert_ne!(
            stats_changed.telemetry_sidebar,
            rtt_changed.telemetry_sidebar
        );

        let debug_overlay_updated = region_cache_keys(&model, false, false, Some(17));
        assert_eq!(rtt_changed.rail, debug_overlay_updated.rail);
        assert_eq!(
            rtt_changed.device_sidebar,
            debug_overlay_updated.device_sidebar
        );
        assert_eq!(
            rtt_changed.telemetry_sidebar,
            debug_overlay_updated.telemetry_sidebar
        );
        assert_ne!(rtt_changed.workspace, debug_overlay_updated.workspace);
    }
}

#[cfg(test)]
mod viewer_input_ui_tests {
    use super::*;

    #[test]
    fn reconnecting_overlay_copy_says_retry_is_automatic() {
        assert_eq!(
            overlay_message(&SessionOverlay::Reconnecting).as_deref(),
            Some("Connection lost  ·  retrying automatically")
        );
    }

    #[test]
    fn iced_mouse_buttons_map_to_the_protocol_button_ids() {
        assert_eq!(iced_mouse_button(iced::mouse::Button::Left), Some(1));
        assert_eq!(iced_mouse_button(iced::mouse::Button::Right), Some(2));
        assert_eq!(iced_mouse_button(iced::mouse::Button::Middle), Some(3));
        assert_eq!(iced_mouse_button(iced::mouse::Button::Back), Some(4));
        assert_eq!(iced_mouse_button(iced::mouse::Button::Forward), Some(5));
        assert_eq!(iced_mouse_button(iced::mouse::Button::Other(7)), None);
    }

    #[test]
    fn wheel_delta_conversion_is_bounded_and_rejects_non_finite_values() {
        assert_eq!(wheel_units(2.0, 120.0), 240);
        assert_eq!(wheel_units(-1.5, 1.0), -2);
        assert_eq!(wheel_units(f32::INFINITY, 120.0), 0);
        assert_eq!(wheel_units(100_000.0, 120.0), i16::MAX);
    }
}
