//! Deterministic fake devices, telemetry, events, and latest-frame video for the app shell.
//!
//! The fake has an injected clock: call FakeCore::advance_to from the app timer
//! or tests. It never opens sockets or sends pixels through the core event bus.
use racc_core::{
    CoreError, CoreEvent, CoreHandle, CoreSnapshot, DeviceId, DeviceSnapshot, DisplayId,
    DisplaySnapshot, FramePayload, FrameSource, Notification, NotificationLevel, QualityPreset,
    SessionEndReason, UiCommand, VideoFrame,
};
use racc_proto::OsType;
use racc_telemetry::{
    CaptureBackendKind, CodecKind, ConnectionState, DecoderKind, EncoderKind, EventKind, EventLog,
    HostSnapshot, PathKind, SessionSnapshot, TelemetrySnapshot,
};
use racc_topology::{Display, DisplayFlags, Topology};
use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Default seed used by FakeCore::default.
pub const DEFAULT_FAKE_SEED: u64 = 0x5241_4343_4641_4b45;
const SWITCH_HOLD_US: u64 = 150_000;
const EVENT_INTERVAL_US: u64 = 2_000_000;
const TELEMETRY_INTERVAL_US: u64 = 250_000;
const H480: u16 = 480;
const H720: u16 = 720;
const H1080: u16 = 1080;

/// Deterministic core and 30 fps synthetic frame source for UI development.
///
/// Time advances only when advance_to is called, making event order, stream
/// switches, and telemetry reproducible in tests.
pub struct FakeCore {
    seed: u64,
    elapsed_us: u64,
    last_telemetry_us: u64,
    devices: Vec<DeviceSnapshot>,
    snapshot: CoreSnapshot,
    events: VecDeque<CoreEvent>,
    event_log: EventLog,
    frame_source: Arc<FakeFrameSource>,
    connected: bool,
    quality: QualityPreset,
    epoch: u16,
    next_event_index: u64,
    pending_switch: Option<PendingSwitch>,
    staged_frame: Option<StagedFrame>,
    pending_authorization: Option<DeviceId>,
    keyboard_capture: bool,
    mouse_capture: bool,
    loss_burst_until_us: Option<u64>,
    last_frame_id: u32,
}

#[derive(Clone, Debug)]
struct PendingSwitch {
    device_id: DeviceId,
    display_id: DisplayId,
    completes_at_us: u64,
}

#[derive(Clone, Debug)]
struct StagedFrame {
    device_id: DeviceId,
    epoch: u16,
    frame: Arc<VideoFrame>,
    stream_reset_returned: bool,
}

/// Latest-frame-only renderer handoff owned by FakeCore.
#[derive(Default)]
pub struct FakeFrameSource {
    latest: RwLock<Option<Arc<VideoFrame>>>,
}

impl FakeFrameSource {
    fn replace(&self, frame: Arc<VideoFrame>) {
        match self.latest.write() {
            Ok(mut latest) => *latest = Some(frame),
            Err(poisoned) => *poisoned.into_inner() = Some(frame),
        }
    }
}

impl FrameSource for FakeFrameSource {
    fn latest_frame(&self) -> Option<Arc<VideoFrame>> {
        match self.latest.read() {
            Ok(latest) => latest.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl Default for FakeCore {
    fn default() -> Self {
        Self::new(DEFAULT_FAKE_SEED)
    }
}

impl FakeCore {
    /// Creates the standard three-device scenario using the supplied seed.
    ///
    /// It contains two Windows peers and one macOS peer, with one offline peer.
    /// A timed authorization request is later raised for the online Mac peer.
    pub fn new(seed: u64) -> Self {
        let devices = demo_devices();
        let selected_device = devices[0].id.clone();
        let selected_display = devices[0].displays.first().map(|display| display.id);
        let frame_source = Arc::new(FakeFrameSource::default());
        let mut fake = Self {
            seed,
            elapsed_us: 0,
            last_telemetry_us: 0,
            devices: devices.clone(),
            snapshot: CoreSnapshot {
                devices: devices.clone(),
                selected_device: Some(selected_device.clone()),
                selected_display,
                local_device_name: "WIN10-GAMING-PC".to_owned(),
                hosting_enabled: true,
                visible: true,
                clipboard_session_active: true,
                clipboard_sync_enabled: false,
                telemetry: TelemetrySnapshot::default(),
            },
            events: VecDeque::new(),
            event_log: EventLog::new(),
            frame_source,
            connected: true,
            quality: QualityPreset::P720,
            epoch: 1,
            next_event_index: 0,
            pending_switch: None,
            staged_frame: None,
            pending_authorization: None,
            keyboard_capture: false,
            mouse_capture: false,
            loss_burst_until_us: None,
            last_frame_id: 0,
        };

        for device in fake.devices.iter().filter(|device| device.online) {
            fake.events
                .push_back(CoreEvent::DeviceDiscovered(device.clone()));
        }
        if let Some(device) = fake.devices.iter().find(|device| !device.online) {
            fake.events.push_back(CoreEvent::DeviceOffline {
                device_id: device.id.clone(),
            });
        }
        if let (Some(display_id), Some((width, height))) = (
            selected_display,
            selected_display.and_then(|id| {
                stream_dimensions(&fake.devices, &selected_device, id, fake.quality)
            }),
        ) {
            fake.events.push_back(CoreEvent::StreamStarted {
                device_id: selected_device.clone(),
                display_id,
                width,
                height,
                fps: 30,
            });
            fake.events.push_back(CoreEvent::DecoderReady {
                device_id: selected_device,
            });
        }
        fake.record_event(
            EventKind::ConnectionEstablished,
            "The fake viewer connected.",
        );
        fake.update_telemetry();
        fake.publish_frame(0, selected_display, fake.snapshot.selected_device.as_ref());
        fake
    }

    /// Returns an Arc renderer handle that reads only the newest frame.
    pub fn frame_source(&self) -> Arc<dyn FrameSource> {
        self.frame_source.clone()
    }

    /// Returns the current deterministic clock in microseconds.
    pub const fn elapsed_us(&self) -> u64 {
        self.elapsed_us
    }

    /// Advances simulation time monotonically and processes due actions in order.
    ///
    /// The source retains only the newest frame, so time jumps do not create a
    /// backlog. Call at 30 Hz from the app timer for a visibly moving pattern.
    pub fn advance_to(&mut self, target_us: u64) {
        let target_us = target_us.max(self.elapsed_us);
        loop {
            let next_event = self.next_event_time();
            let next_switch = self
                .pending_switch
                .as_ref()
                .map(|pending| pending.completes_at_us);
            let next_due = match (next_event, next_switch) {
                (Some(event), Some(switch)) => Some(event.min(switch)),
                (Some(event), None) => Some(event),
                (None, Some(switch)) => Some(switch),
                (None, None) => None,
            };
            let Some(next_due) = next_due.filter(|due| *due <= target_us) else {
                break;
            };
            self.elapsed_us = self.elapsed_us.max(next_due);
            if self
                .pending_switch
                .as_ref()
                .is_some_and(|pending| pending.completes_at_us <= self.elapsed_us)
            {
                self.complete_switch();
            }
            if self.next_event_time() == Some(next_due) {
                self.run_scheduled_event();
            }
        }
        self.elapsed_us = target_us;
        if target_us.saturating_sub(self.last_telemetry_us) >= TELEMETRY_INTERVAL_US {
            self.update_telemetry();
        }
        if self.connected
            && self.snapshot.visible
            && self.pending_switch.is_none()
            && self.staged_frame.is_none()
        {
            let frame_number = target_us.saturating_mul(30) / 1_000_000;
            let frame_id = frame_number as u32;
            if frame_id != self.last_frame_id || target_us == 0 {
                self.last_frame_id = frame_id;
                self.publish_frame(
                    frame_number,
                    self.snapshot.selected_display,
                    self.snapshot.selected_device.as_ref(),
                );
            }
        }
    }

    /// Advances the injected clock by a duration.
    pub fn advance_by(&mut self, duration: Duration) {
        let delta = duration.as_micros().min(u128::from(u64::MAX)) as u64;
        self.advance_to(self.elapsed_us.saturating_add(delta));
    }

    fn next_event_time(&self) -> Option<u64> {
        // Switch, decoder reset, quality, and loss repeat every eight seconds.
        // The ninth event also requests authorization from the Mac peer.
        let ordinal = self.next_event_index.checked_add(1)?;
        let phase = mix64(self.seed) % 250_000;
        ordinal.checked_mul(EVENT_INTERVAL_US)?.checked_add(phase)
    }
    fn run_scheduled_event(&mut self) {
        let index = self.next_event_index;
        self.next_event_index = self.next_event_index.saturating_add(1);
        match index % 4 {
            0 => self.begin_next_display_switch(),
            1 => self.simulate_decoder_reset(),
            2 => self.simulate_quality_adjustment(),
            _ => self.simulate_packet_loss(),
        }
        if index == 8 {
            self.simulate_authorization_request();
        }
    }

    fn begin_next_display_switch(&mut self) {
        if self.pending_switch.is_some() || !self.connected {
            return;
        }
        let Some(device_id) = self.snapshot.selected_device.clone() else {
            return;
        };
        let Some(device) = self.devices.iter().find(|device| device.id == device_id) else {
            return;
        };
        let available: Vec<_> = device
            .displays
            .iter()
            .filter(|display| display.available)
            .map(|display| display.id)
            .collect();
        if available.len() < 2 {
            return;
        }
        let current = self.snapshot.selected_display;
        let next = available
            .iter()
            .copied()
            .find(|display| Some(*display) != current)
            .unwrap_or(available[0]);
        self.select_display(device_id, next);
    }

    fn select_device(&mut self, device_id: DeviceId) {
        let Some(device) = self.device_snapshot(&device_id) else {
            return;
        };
        let previous_device = self.snapshot.selected_device.clone();
        if let Some(previous) = previous_device
            .as_ref()
            .filter(|previous| *previous != &device_id)
        {
            self.update_streamed_display(previous, None);
        }
        self.pending_switch = None;
        self.snapshot.selected_device = Some(device_id.clone());
        let display = device
            .displays
            .iter()
            .find(|display| display.available && display.primary)
            .or_else(|| device.displays.iter().find(|display| display.available));
        self.snapshot.selected_display = display.map(|display| display.id);
        if device.online && device.host_capable {
            self.connected = self.snapshot.selected_display.is_some();
            if let (Some(display_id), Some(topology)) = (
                self.snapshot.selected_display,
                self.topology_for(&device_id, self.snapshot.selected_display),
            ) {
                self.events.push_back(CoreEvent::TopologyChanged {
                    device_id: device_id.clone(),
                    topology,
                });
                self.events.push_back(CoreEvent::DisplaySelected {
                    device_id: device_id.clone(),
                    display_id,
                });
                self.record_event(
                    EventKind::DisplaySwitch,
                    format!(
                        "Connecting to {} while holding the previous frame.",
                        device.name
                    ),
                );
                self.staged_frame = None;
                self.pending_switch = Some(PendingSwitch {
                    device_id,
                    display_id,
                    completes_at_us: self.elapsed_us.saturating_add(SWITCH_HOLD_US),
                });
            } else if let Some(device_id) = self.snapshot.selected_device.clone() {
                self.events
                    .push_back(CoreEvent::VideoUnavailable { device_id });
            }
        } else {
            self.connected = false;
            if !device.online {
                self.events
                    .push_back(CoreEvent::DeviceOffline { device_id });
            } else {
                self.events
                    .push_back(CoreEvent::VideoUnavailable { device_id });
            }
        }
        self.update_telemetry();
    }

    fn select_display(&mut self, device_id: DeviceId, display_id: DisplayId) {
        let Some(device) = self.devices.iter().find(|device| device.id == device_id) else {
            return;
        };
        if !device
            .displays
            .iter()
            .any(|display| display.id == display_id && display.available)
        {
            return;
        }
        self.snapshot.selected_device = Some(device_id.clone());
        self.snapshot.selected_display = Some(display_id);
        self.events.push_back(CoreEvent::DisplaySelected {
            device_id: device_id.clone(),
            display_id,
        });
        self.record_event(
            EventKind::DisplaySwitch,
            format!("Switching to display {display_id:?}."),
        );
        self.staged_frame = None;
        self.pending_switch = Some(PendingSwitch {
            device_id,
            display_id,
            completes_at_us: self.elapsed_us.saturating_add(SWITCH_HOLD_US),
        });
    }

    fn complete_switch(&mut self) {
        let Some(pending) = self.pending_switch.take() else {
            return;
        };
        self.epoch = self.epoch.wrapping_add(1);
        self.last_frame_id = 0;
        self.update_streamed_display(&pending.device_id, Some(pending.display_id));
        if let Some((width, height)) = stream_dimensions(
            &self.devices,
            &pending.device_id,
            pending.display_id,
            self.quality,
        ) {
            self.stage_frame(
                pending.device_id.clone(),
                pending.display_id,
                self.elapsed_us.saturating_mul(30) / 1_000_000,
            );
            self.record_event(EventKind::StreamReset, "Display stream reset completed.");
            self.events.push_back(CoreEvent::StreamReset {
                device_id: pending.device_id.clone(),
                epoch: self.epoch,
                width,
                height,
            });
            self.events.push_back(CoreEvent::StreamStarted {
                device_id: pending.device_id.clone(),
                display_id: pending.display_id,
                width,
                height,
                fps: 30,
            });
            self.events.push_back(CoreEvent::DecoderReady {
                device_id: pending.device_id,
            });
        }
    }
    fn update_streamed_display(&mut self, device_id: &DeviceId, display_id: Option<DisplayId>) {
        if let Some(device) = self
            .devices
            .iter_mut()
            .find(|device| device.id == *device_id)
        {
            device.streamed_display = display_id;
        }
        self.snapshot.devices = self.devices.clone();
    }
    fn simulate_decoder_reset(&mut self) {
        if !self.connected {
            return;
        }
        let Some(device_id) = self.snapshot.selected_device.clone() else {
            return;
        };
        let Some(display_id) = self.snapshot.selected_display else {
            return;
        };
        self.epoch = self.epoch.wrapping_add(1);
        self.last_frame_id = 0;
        if let Some((width, height)) =
            stream_dimensions(&self.devices, &device_id, display_id, self.quality)
        {
            self.stage_frame(
                device_id.clone(),
                display_id,
                self.elapsed_us.saturating_mul(30) / 1_000_000,
            );
            self.events.push_back(CoreEvent::StreamReset {
                device_id: device_id.clone(),
                epoch: self.epoch,
                width,
                height,
            });
            self.events.push_back(CoreEvent::DecoderReady { device_id });
        }
        self.record_event(EventKind::DecoderReset, "The fake decoder recovered.");
        self.notify(
            NotificationLevel::Warning,
            "Decoder reset",
            "The simulated decoder recovered and resumed the latest frame.",
        );
    }

    fn simulate_quality_adjustment(&mut self) {
        if !self.connected {
            return;
        }
        self.quality = match self.quality {
            QualityPreset::P480 => QualityPreset::P720,
            QualityPreset::P720 | QualityPreset::Auto => QualityPreset::P1080,
            QualityPreset::P1080 => QualityPreset::P480,
        };
        self.update_telemetry();
        self.record_event(
            EventKind::QualityAdjustment,
            format!("The host selected the {:?} demo tier.", self.quality),
        );
        self.notify(
            NotificationLevel::Info,
            "Quality adjusted",
            format!("The host selected the {:?} demo tier.", self.quality),
        );
    }

    fn simulate_packet_loss(&mut self) {
        if !self.connected {
            return;
        }
        self.loss_burst_until_us = Some(self.elapsed_us.saturating_add(1_500_000));
        self.update_telemetry();
        self.record_event(
            EventKind::PacketLossEvent,
            "The fake session injected a short loss burst.",
        );
        self.notify(
            NotificationLevel::Warning,
            "Packet loss",
            "The fake session injected a short loss burst; video stays latest-frame-wins.",
        );
    }

    fn simulate_authorization_request(&mut self) {
        let Some(mac) = self
            .devices
            .iter()
            .find(|device| device.os == OsType::MacOs)
        else {
            return;
        };
        self.pending_authorization = Some(mac.id.clone());
        self.events.push_back(CoreEvent::PendingAuthorization {
            device_id: mac.id.clone(),
            device_name: mac.name.clone(),
        });
        self.notify(
            NotificationLevel::Warning,
            "Peer approval required",
            format!("{} is waiting for host approval.", mac.name),
        );
    }

    fn stage_frame(&mut self, device_id: DeviceId, display_id: DisplayId, frame_number: u64) {
        self.staged_frame = Some(StagedFrame {
            device_id: device_id.clone(),
            epoch: self.epoch,
            frame: self.make_frame(frame_number, Some(display_id), Some(&device_id)),
            stream_reset_returned: false,
        });
    }

    fn note_event_returned(&mut self, event: &CoreEvent) {
        let Some(staged) = self.staged_frame.as_mut() else {
            return;
        };
        match event {
            CoreEvent::StreamReset {
                device_id, epoch, ..
            } if device_id == &staged.device_id && *epoch == staged.epoch => {
                staged.stream_reset_returned = true;
            }
            CoreEvent::DecoderReady { device_id }
                if device_id == &staged.device_id && staged.stream_reset_returned =>
            {
                let staged = self.staged_frame.take();
                if let Some(staged) = staged {
                    self.frame_source.replace(staged.frame);
                }
            }
            _ => {}
        }
    }

    fn publish_frame(
        &self,
        frame_number: u64,
        display_id: Option<DisplayId>,
        device_id: Option<&DeviceId>,
    ) {
        self.frame_source
            .replace(self.make_frame(frame_number, display_id, device_id));
    }

    fn make_frame(
        &self,
        frame_number: u64,
        display_id: Option<DisplayId>,
        device_id: Option<&DeviceId>,
    ) -> Arc<VideoFrame> {
        let dimensions = display_id
            .and_then(|id| {
                device_id
                    .and_then(|device| stream_dimensions(&self.devices, device, id, self.quality))
            })
            .unwrap_or((1280, H720));
        Arc::new(VideoFrame {
            epoch: self.epoch,
            frame_id: frame_number as u32,
            display_id,
            width: dimensions.0,
            height: dimensions.1,
            fps: 30,
            capture_ts_us: None,
            decode_duration_us: None,
            payload: FramePayload::SyntheticPattern {
                seed: mix64(self.seed ^ u64::from(display_id.map_or(0, DisplayId::get))),
                frame_number,
            },
        })
    }

    fn update_telemetry(&mut self) {
        self.last_telemetry_us = self.elapsed_us;
        let frame_number = self.elapsed_us.saturating_mul(30) / 1_000_000;
        let jitter = mix64(self.seed ^ (frame_number / 8)) % 17;
        let rtt_us = 7_000 + jitter * 400;
        let loss_fraction = if self
            .loss_burst_until_us
            .is_some_and(|until| self.elapsed_us < until)
        {
            0.027
        } else {
            ((mix64(self.seed ^ (frame_number / 15)) % 10) as f64) / 10_000.0
        };
        let selected = self
            .snapshot
            .selected_device
            .as_ref()
            .and_then(|id| self.devices.iter().find(|device| device.id == *id));
        let (width, height) = selected
            .zip(self.snapshot.selected_display)
            .and_then(|(device, display)| {
                stream_dimensions(&self.devices, &device.id, display, self.quality)
            })
            .unwrap_or((1280, H720));
        let display_refresh = selected
            .and_then(|device| {
                self.snapshot
                    .selected_display
                    .and_then(|id| device.displays.iter().find(|display| display.id == id))
            })
            .map_or(60_000, |display| display.refresh_mhz);
        let is_mac = selected.is_some_and(|device| device.os == OsType::MacOs);
        let target_kbps: u32 = match self.quality {
            QualityPreset::P480 => 1_500,
            QualityPreset::P720 | QualityPreset::Auto => 3_500,
            QualityPreset::P1080 => 7_000,
        };
        let actual_kbps = target_kbps.saturating_sub((jitter as u32).saturating_mul(9));
        let cpu = 180 + (mix64(self.seed ^ (frame_number / 5)) % 370) as u16;
        let connected = self.connected;
        self.snapshot.clipboard_session_active = connected;
        if !connected {
            self.snapshot.clipboard_sync_enabled = false;
        }
        self.snapshot.telemetry = TelemetrySnapshot {
            session: SessionSnapshot {
                connection_state: if connected {
                    ConnectionState::Connected
                } else {
                    ConnectionState::Disconnected
                },
                path: if (frame_number / 240).is_multiple_of(2) {
                    PathKind::Direct
                } else {
                    PathKind::Derp
                },
                last_rtt_us: connected.then_some(rtt_us),
                min_rtt_us: connected.then_some(6_800),
                srtt_us: connected.then_some(rtt_us as f64 + 150.0),
                rttvar_us: connected.then_some(jitter as f64 * 100.0),
                jitter_us: connected.then_some(jitter as f64 * 80.0),
                loss_fraction: if connected { loss_fraction } else { 0.0 },
                frame_loss_fraction: if connected { loss_fraction / 2.0 } else { 0.0 },
                bitrate_bps: if connected {
                    u64::from(actual_kbps) * 1_000
                } else {
                    0
                },
                fps: if connected && self.snapshot.visible {
                    30.0
                } else {
                    0.0
                },
                codec: if connected {
                    CodecKind::H264
                } else {
                    CodecKind::Unknown
                },
                decoder: if !connected {
                    DecoderKind::Unknown
                } else if is_mac {
                    DecoderKind::VideoToolbox
                } else {
                    DecoderKind::MediaFoundation
                },
                epoch: self.epoch,
            },
            host: HostSnapshot {
                cpu_pct_x10: cpu,
                process_cpu_pct_x10: Some(cpu / 2),
                capture_backend: if is_mac {
                    CaptureBackendKind::ScreenCaptureKit
                } else {
                    CaptureBackendKind::Dxgi
                },
                encoder: if is_mac {
                    EncoderKind::VideoToolbox
                } else {
                    EncoderKind::MediaFoundationHw
                },
                width,
                height,
                refresh_mhz: display_refresh,
                target_bitrate_kbps: target_kbps,
                actual_bitrate_kbps: actual_kbps,
            },
            events: self.event_log.snapshot(),
        };
    }

    fn record_event(&mut self, kind: EventKind, detail: impl Into<String>) {
        let _ = self.event_log.push(self.elapsed_us, kind, detail);
    }

    fn notify(&mut self, level: NotificationLevel, title: &str, message: impl Into<String>) {
        self.events.push_back(CoreEvent::Notification(Notification {
            level,
            title: title.to_owned(),
            message: message.into(),
        }));
    }

    fn device_snapshot(&self, id: &DeviceId) -> Option<DeviceSnapshot> {
        self.devices.iter().find(|device| device.id == *id).cloned()
    }

    fn topology_for(&self, device_id: &DeviceId, active: Option<DisplayId>) -> Option<Topology> {
        let device = self.devices.iter().find(|device| device.id == *device_id)?;
        let displays = device
            .displays
            .iter()
            .enumerate()
            .map(|(index, display)| {
                Display::new(
                    display.id,
                    display.name.clone(),
                    (index as i32)
                        .saturating_mul(i32::try_from(display.width_px).unwrap_or(i32::MAX)),
                    0,
                    display.width_px,
                    display.height_px,
                    if display.name.contains("Retina") {
                        2_000
                    } else {
                        1_000
                    },
                    display.refresh_mhz,
                    DisplayFlags::new(display.primary, device.online, display.available, false),
                )
            })
            .collect();
        Topology::new(1, displays, active).ok()
    }
}

impl CoreHandle for FakeCore {
    fn send(&mut self, command: UiCommand) -> Result<(), CoreError> {
        match command {
            UiCommand::DiscoverDevices => {
                let devices = self.devices.clone();
                let mut online_hosts = 0;
                for device in devices {
                    if device.online && device.host_capable {
                        online_hosts += 1;
                        self.events.push_back(CoreEvent::DeviceDiscovered(device));
                    } else if !device.online {
                        self.events.push_back(CoreEvent::DeviceOffline {
                            device_id: device.id,
                        });
                    }
                }
                self.record_event(
                    EventKind::ConnectionEstablished,
                    format!("Fake device scan found {online_hosts} online host(s)."),
                );
                self.notify(
                    NotificationLevel::Info,
                    "Device scan complete",
                    format!("Found {online_hosts} online host(s) in the fake scenario."),
                );
            }
            UiCommand::SelectDevice(device_id) => self.select_device(device_id),
            UiCommand::SelectDisplay {
                device_id,
                display_id,
            } => self.select_display(device_id, display_id),
            UiCommand::SetQuality { device_id, quality } => {
                if self.connected && self.snapshot.selected_device.as_ref() == Some(&device_id) {
                    if let Some(display_id) = self.snapshot.selected_display {
                        self.quality = quality;
                        if self.pending_switch.is_none() {
                            self.staged_frame = None;
                            self.epoch = self.epoch.wrapping_add(1);
                            self.last_frame_id = 0;
                            if let Some((width, height)) = stream_dimensions(
                                &self.devices,
                                &device_id,
                                display_id,
                                self.quality,
                            ) {
                                self.stage_frame(
                                    device_id.clone(),
                                    display_id,
                                    self.elapsed_us.saturating_mul(30) / 1_000_000,
                                );
                                self.events.push_back(CoreEvent::StreamReset {
                                    device_id: device_id.clone(),
                                    epoch: self.epoch,
                                    width,
                                    height,
                                });
                                self.events.push_back(CoreEvent::StreamStarted {
                                    device_id: device_id.clone(),
                                    display_id,
                                    width,
                                    height,
                                    fps: 30,
                                });
                                self.events.push_back(CoreEvent::DecoderReady { device_id });
                            }
                        }
                        self.record_event(
                            EventKind::QualityAdjustment,
                            format!("The fake stream changed to the {:?} tier.", self.quality),
                        );
                        self.update_telemetry();
                    }
                }
            }
            UiCommand::SetVisible(visible) => {
                let was_visible = self.snapshot.visible;
                self.snapshot.visible = visible;
                if was_visible != visible {
                    self.record_event(
                        if visible {
                            EventKind::Resumed
                        } else {
                            EventKind::Paused
                        },
                        if visible {
                            "Video resumed."
                        } else {
                            "Video paused while hidden."
                        },
                    );
                }
                self.update_telemetry();
                if visible && !was_visible && self.connected && self.staged_frame.is_none() {
                    self.publish_frame(
                        self.elapsed_us.saturating_mul(30) / 1_000_000,
                        self.snapshot.selected_display,
                        self.snapshot.selected_device.as_ref(),
                    );
                }
            }
            UiCommand::ToggleKeyboardCapture(enabled) => {
                self.keyboard_capture = enabled;
                self.events.push_back(CoreEvent::InputCaptureChanged {
                    keyboard: self.keyboard_capture,
                    mouse: self.mouse_capture,
                });
            }
            UiCommand::ToggleMouseCapture(enabled) => {
                self.mouse_capture = enabled;
                self.events.push_back(CoreEvent::InputCaptureChanged {
                    keyboard: self.keyboard_capture,
                    mouse: self.mouse_capture,
                });
            }
            UiCommand::SetClipboardEnabled(enabled) => {
                self.snapshot.clipboard_sync_enabled = self.connected && enabled;
            }
            UiCommand::Disconnect => {
                self.snapshot.clipboard_sync_enabled = false;
                if self.connected {
                    self.connected = false;
                    self.events.push_back(CoreEvent::SessionEnded {
                        device_id: self.snapshot.selected_device.clone(),
                        reason: SessionEndReason::Disconnected,
                    });
                }
                self.update_telemetry();
            }
            UiCommand::Connect(device_id) => self.select_device(device_id),
            UiCommand::ApprovePeer(device_id) => {
                if self.pending_authorization.as_ref() == Some(&device_id) {
                    self.pending_authorization = None;
                    if let Some(device) = self
                        .devices
                        .iter_mut()
                        .find(|device| device.id == device_id)
                    {
                        device.host_capable = true;
                    }
                    self.snapshot.devices = self.devices.clone();
                    if let Some(device) = self.device_snapshot(&device_id) {
                        self.events.push_back(CoreEvent::DeviceDiscovered(device));
                    }
                    self.notify(
                        NotificationLevel::Info,
                        "Peer approved",
                        "The fake peer is now host-capable.",
                    );
                }
            }
            UiCommand::RejectPeer(device_id) => {
                if self.pending_authorization.as_ref() == Some(&device_id) {
                    self.pending_authorization = None;
                    self.notify(
                        NotificationLevel::Info,
                        "Peer rejected",
                        "The fake peer remains blocked.",
                    );
                }
            }
            UiCommand::RemovePeer(device_id) => {
                let mut removed = false;
                if let Some(device) = self
                    .devices
                    .iter_mut()
                    .find(|device| device.id == device_id)
                {
                    removed = device.host_capable;
                    device.host_capable = false;
                }
                if removed {
                    self.snapshot.devices = self.devices.clone();
                    if self.snapshot.selected_device.as_ref() == Some(&device_id) && self.connected
                    {
                        self.connected = false;
                        self.events.push_back(CoreEvent::SessionEnded {
                            device_id: Some(device_id),
                            reason: SessionEndReason::RemoteClosed,
                        });
                        self.update_telemetry();
                    }
                    self.notify(
                        NotificationLevel::Info,
                        "Peer removed",
                        "The peer must be approved again before it can connect.",
                    );
                }
            }
            UiCommand::SetHosting(enabled) => self.snapshot.hosting_enabled = enabled,
        }
        Ok(())
    }

    fn poll_events(&mut self, max_events: usize) -> Result<Vec<CoreEvent>, CoreError> {
        let count = max_events.min(self.events.len());
        let events: Vec<_> = self.events.drain(..count).collect();
        for event in &events {
            self.note_event_returned(event);
        }
        Ok(events)
    }

    fn snapshot(&self) -> Result<CoreSnapshot, CoreError> {
        Ok(self.snapshot.clone())
    }
}
fn demo_devices() -> Vec<DeviceSnapshot> {
    vec![
        DeviceSnapshot {
            id: device_id("win10-gaming"),
            name: "WIN10-GAMING-PC".to_owned(),
            os: OsType::Windows,
            online: true,
            host_capable: true,
            displays: vec![
                display(101, "Display 1", 1920, 1080, 144_000, true, true),
                display(102, "Display 2", 2560, 1440, 60_000, false, true),
                display(103, "Portrait Display", 1080, 1920, 60_000, false, true),
            ],
            streamed_display: Some(display_id(101)),
        },
        DeviceSnapshot {
            id: device_id("win10-office"),
            name: "WIN10-OFFICE-PC".to_owned(),
            os: OsType::Windows,
            online: false,
            host_capable: false,
            displays: vec![display(201, "Display 1", 1920, 1080, 60_000, true, false)],
            streamed_display: None,
        },
        DeviceSnapshot {
            id: device_id("macbook-2015"),
            name: "INTEL-MAC-2015".to_owned(),
            os: OsType::MacOs,
            online: true,
            host_capable: false,
            displays: vec![
                display(301, "Retina Display", 2560, 1600, 60_000, true, true),
                display(302, "External Display", 1920, 1080, 60_000, false, true),
            ],
            streamed_display: None,
        },
    ]
}

fn display(
    id: u32,
    name: &str,
    width_px: u32,
    height_px: u32,
    refresh_mhz: u32,
    primary: bool,
    available: bool,
) -> DisplaySnapshot {
    DisplaySnapshot {
        id: display_id(id),
        name: name.to_owned(),
        width_px,
        height_px,
        refresh_mhz,
        scale_milli: match id % 3 {
            0 => 1_000,
            1 => 1_250,
            _ => 1_500,
        },
        available,
        primary,
    }
}

fn device_id(value: &str) -> DeviceId {
    match DeviceId::new(value) {
        Ok(id) => id,
        Err(_) => unreachable!("static fake device identifiers are nonempty"),
    }
}

fn display_id(value: u32) -> DisplayId {
    match DisplayId::new(value) {
        Some(id) => id,
        None => unreachable!("static fake display identifiers are nonzero"),
    }
}

fn stream_dimensions(
    devices: &[DeviceSnapshot],
    device_id: &DeviceId,
    display_id: DisplayId,
    quality: QualityPreset,
) -> Option<(u16, u16)> {
    let device = devices.iter().find(|device| device.id == *device_id)?;
    let display = device
        .displays
        .iter()
        .find(|display| display.id == display_id)?;
    let requested_height = match quality {
        QualityPreset::P480 => H480,
        QualityPreset::P720 | QualityPreset::Auto => H720,
        QualityPreset::P1080 if device.os == OsType::MacOs => H720,
        QualityPreset::P1080 => H1080,
    };
    let height = requested_height.min(u16::try_from(display.height_px).unwrap_or(u16::MAX));
    let width = (u32::from(height) * display.width_px / display.height_px.max(1))
        .min(u32::from(u16::MAX)) as u16;
    Some((width.max(1), height.max(1)))
}

fn mix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_has_three_devices_two_windows_one_mac_and_one_offline() {
        let fake = FakeCore::default();
        let snapshot = fake.snapshot().unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(snapshot.devices.len(), 3);
        assert_eq!(
            snapshot
                .devices
                .iter()
                .filter(|device| device.os == OsType::Windows)
                .count(),
            2
        );
        assert_eq!(
            snapshot
                .devices
                .iter()
                .filter(|device| device.os == OsType::MacOs)
                .count(),
            1
        );
        assert_eq!(
            snapshot
                .devices
                .iter()
                .filter(|device| !device.online)
                .count(),
            1
        );
        assert!(snapshot
            .devices
            .iter()
            .all(|device| (1..=3).contains(&device.displays.len())));
    }

    #[test]
    fn telemetry_event_log_retains_scripted_events_across_snapshot_updates() {
        let mut fake = FakeCore::new(77);
        fake.advance_to(12_500_000);
        let snapshot = fake.snapshot().expect("fake snapshot");
        let kinds: Vec<_> = snapshot
            .telemetry
            .events
            .events()
            .iter()
            .map(|event| event.kind)
            .collect();
        assert!(kinds.contains(&EventKind::DisplaySwitch));
        assert!(kinds.contains(&EventKind::DecoderReset));
        assert!(kinds.contains(&EventKind::QualityAdjustment));
        assert!(kinds.contains(&EventKind::PacketLossEvent));
    }

    #[test]
    fn removing_an_approved_peer_removes_allowlist_access() {
        let mut fake = FakeCore::new(41);
        let selected = fake
            .snapshot()
            .expect("snapshot")
            .selected_device
            .expect("selected");
        fake.send(UiCommand::RemovePeer(selected.clone()))
            .expect("remove peer");
        let snapshot = fake.snapshot().expect("snapshot after removal");
        let peer = snapshot
            .devices
            .iter()
            .find(|device| device.id == selected)
            .expect("peer");
        assert!(!peer.host_capable);
        assert_eq!(
            snapshot.telemetry.session.connection_state,
            ConnectionState::Disconnected
        );
    }

    #[test]
    fn connect_command_uses_the_held_frame_and_decoder_ready_lifecycle() {
        let mut fake = FakeCore::new(41);
        let snapshot = fake.snapshot().expect("fake snapshot");
        let device_id = snapshot.selected_device.expect("selected host");
        let display_id = snapshot.selected_display.expect("selected display");
        let source = fake.frame_source();
        let previous_frame = source.latest_frame().expect("initial frame");
        let _ = fake.poll_events(128);

        fake.send(UiCommand::Connect(device_id.clone()))
            .expect("connect command");
        assert_eq!(source.latest_frame(), Some(previous_frame.clone()));
        fake.advance_by(Duration::from_millis(150));
        let events = fake.poll_events(128).expect("connect events");
        let replacement = source.latest_frame().expect("replacement frame");
        assert_eq!(replacement.display_id, Some(display_id));
        assert!(replacement.frame_id > previous_frame.frame_id);
        assert!(events.iter().any(|event| matches!(
            event,
            CoreEvent::StreamStarted { device_id: event_device, display_id: event_display, .. }
                if event_device == &device_id && *event_display == display_id
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            CoreEvent::DecoderReady { device_id: event_device } if event_device == &device_id
        )));
    }

    #[test]
    fn selecting_another_device_holds_the_old_frame_until_stream_ready() {
        let mut fake = FakeCore::default();
        // The scenario deliberately starts with only one approved online host.
        // Advance to the scripted Mac approval request, then approve that peer
        // so this test exercises switching between two eligible hosts.
        fake.advance_to(19_000_000);
        let mac_id = fake
            .snapshot()
            .expect("fake snapshot")
            .devices
            .into_iter()
            .find(|device| device.os == OsType::MacOs)
            .expect("Mac peer")
            .id;
        fake.send(UiCommand::ApprovePeer(mac_id))
            .expect("approve Mac peer");
        let source = fake.frame_source();
        let snapshot = fake.snapshot().expect("fake snapshot");
        let old_device = snapshot.selected_device.expect("selected device");
        let new_device = snapshot
            .devices
            .iter()
            .find(|device| device.online && device.host_capable && device.id != old_device)
            .expect("second online host")
            .clone();
        let new_display = new_device
            .displays
            .iter()
            .find(|display| display.available && display.primary)
            .or_else(|| new_device.displays.iter().find(|display| display.available))
            .expect("available display")
            .id;
        let _ = fake.poll_events(128);
        let old_frame = source
            .latest_frame()
            .expect("current frame after pending fake events");
        assert!(fake
            .send(UiCommand::SelectDevice(new_device.id.clone()))
            .is_ok());
        assert!(fake
            .send(UiCommand::SetQuality {
                device_id: new_device.id.clone(),
                quality: QualityPreset::P720,
            })
            .is_ok());
        assert_eq!(source.latest_frame(), Some(old_frame.clone()));
        fake.advance_by(Duration::from_millis(149));
        assert_eq!(source.latest_frame(), Some(old_frame.clone()));
        fake.advance_by(Duration::from_millis(1));
        assert_eq!(source.latest_frame(), Some(old_frame.clone()));
        let events = fake.poll_events(128).expect("switch events");
        let frame = source
            .latest_frame()
            .expect("replacement frame after decoder-ready events");
        assert_eq!(frame.display_id, Some(new_display));
        assert!(events.iter().any(|event| matches!(
            event,
            CoreEvent::StreamStarted { device_id, display_id, .. }
                if device_id == &new_device.id && *display_id == new_display
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            CoreEvent::DecoderReady { device_id } if device_id == &new_device.id
        )));
    }

    #[test]
    fn discover_devices_reannounces_online_hosts_and_offline_peers() {
        let mut fake = FakeCore::default();
        let snapshot = fake.snapshot().expect("fake snapshot");
        let expected_online = snapshot
            .devices
            .iter()
            .filter(|device| device.online && device.host_capable)
            .count();
        let _ = fake.poll_events(128);
        assert!(fake.send(UiCommand::DiscoverDevices).is_ok());
        let events = fake.poll_events(128).expect("discovery events");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, CoreEvent::DeviceDiscovered(_)))
                .count(),
            expected_online
        );
        assert!(events
            .iter()
            .any(|event| matches!(event, CoreEvent::DeviceOffline { .. })));
    }

    #[test]
    fn same_seed_replays_event_order_and_frame_metadata() {
        let mut first = FakeCore::new(77);
        let mut second = FakeCore::new(77);
        first.advance_to(19_000_000);
        second.advance_to(19_000_000);
        let first_events = first.poll_events(128).unwrap_or_default();
        let second_events = second.poll_events(128).unwrap_or_default();
        assert_eq!(format!("{first_events:?}"), format!("{second_events:?}"));
        assert_eq!(
            first.frame_source().latest_frame(),
            second.frame_source().latest_frame()
        );
        assert_eq!(first.elapsed_us(), second.elapsed_us());
    }

    #[test]
    fn display_switch_holds_old_frame_for_150_ms_then_replaces_it() {
        let mut fake = FakeCore::default();
        let source = fake.frame_source();
        let before = source.latest_frame().expect("initial frame is published");
        let snapshot = fake.snapshot().unwrap_or_else(|error| panic!("{error}"));
        let device_id = snapshot
            .selected_device
            .expect("default device is selected");
        let old_display = snapshot
            .selected_display
            .expect("default display is selected");
        let new_display = snapshot.devices[0]
            .displays
            .iter()
            .find(|display| display.id != old_display)
            .map(|display| display.id)
            .expect("fake host has multiple displays");
        let _ = fake.poll_events(64);
        assert!(fake
            .send(UiCommand::SelectDisplay {
                device_id,
                display_id: new_display
            })
            .is_ok());
        fake.advance_by(Duration::from_millis(149));
        assert_eq!(source.latest_frame(), Some(before.clone()));
        fake.advance_by(Duration::from_millis(1));
        assert_eq!(source.latest_frame(), Some(before.clone()));
        let initial_switch_events = fake.poll_events(2).unwrap_or_default();
        assert!(initial_switch_events
            .iter()
            .any(|event| matches!(event, CoreEvent::StreamReset { .. })));
        assert_eq!(source.latest_frame(), Some(before.clone()));
        let stream_started = fake.poll_events(1).unwrap_or_default();
        assert!(matches!(
            stream_started.as_slice(),
            [CoreEvent::StreamStarted { .. }]
        ));
        assert_eq!(source.latest_frame(), Some(before.clone()));
        let decoder_ready = fake.poll_events(1).unwrap_or_default();
        assert!(matches!(
            decoder_ready.as_slice(),
            [CoreEvent::DecoderReady { .. }]
        ));
        let after = source
            .latest_frame()
            .expect("replacement frame is published after decoder readiness");
        assert_eq!(after.display_id, Some(new_display));
        assert_ne!(after.epoch, before.epoch);
    }

    #[test]
    fn quality_change_keeps_old_frame_until_decoder_ready_and_updates_dimensions() {
        let mut fake = FakeCore::default();
        let source = fake.frame_source();
        let old = source.latest_frame().expect("initial frame");
        let device_id = fake
            .snapshot()
            .expect("snapshot")
            .selected_device
            .expect("selected device");
        fake.send(UiCommand::SetQuality {
            device_id: device_id.clone(),
            quality: QualityPreset::P480,
        })
        .expect("set quality");
        assert_eq!(source.latest_frame(), Some(old.clone()));
        let events = fake.poll_events(128).expect("quality stream events");
        assert!(events.iter().any(|event| matches!(
            event,
            CoreEvent::StreamReset { device_id: id, height: 480, .. } if id == &device_id
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            CoreEvent::DecoderReady { device_id: id } if id == &device_id
        )));
        let new = source.latest_frame().expect("new frame");
        assert_eq!(new.height, 480);
        assert_ne!(new.epoch, old.epoch);
        let snapshot = fake.snapshot().expect("snapshot after quality update");
        assert_eq!(snapshot.telemetry.host.height, 480);
    }

    #[test]
    fn synthetic_frames_animate_at_thirty_fps_and_telemetry_changes() {
        let mut fake = FakeCore::new(19);
        let source = fake.frame_source();
        let initial = source.latest_frame().expect("initial frame is published");
        fake.advance_to(1_000_000);
        let later = source
            .latest_frame()
            .expect("latest frame remains available");
        assert_eq!(later.fps, 30);
        assert!(matches!(
            later.payload,
            FramePayload::SyntheticPattern {
                frame_number: 30,
                ..
            }
        ));
        assert_eq!(initial.display_id, later.display_id);
        let initial_telemetry = fake
            .snapshot()
            .unwrap_or_default()
            .telemetry
            .session
            .last_rtt_us;
        fake.advance_to(1_250_000);
        let later_telemetry = fake
            .snapshot()
            .unwrap_or_default()
            .telemetry
            .session
            .last_rtt_us;
        assert_ne!(initial_telemetry, later_telemetry);
    }

    #[test]
    fn switch_decoder_quality_and_loss_events_repeat_during_long_runs() {
        let mut fake = FakeCore::new(11);
        fake.advance_to(35_000_000);
        let events = fake.poll_events(256).unwrap_or_default();
        let switches = events
            .iter()
            .filter(|event| matches!(event, CoreEvent::DisplaySelected { .. }))
            .count();
        let resets = events
            .iter()
            .filter(|event| matches!(event, CoreEvent::StreamReset { .. }))
            .count();
        let notifications = events
            .iter()
            .filter(|event| matches!(event, CoreEvent::Notification(_)))
            .count();
        assert!(switches >= 4);
        assert!(resets >= 4);
        assert!(notifications >= 8);
    }
    #[test]
    fn authorization_prompt_can_be_approved() {
        let mut fake = FakeCore::new(3);
        fake.advance_to(20_000_000);
        let request = fake
            .poll_events(256)
            .unwrap_or_default()
            .into_iter()
            .find_map(|event| match event {
                CoreEvent::PendingAuthorization { device_id, .. } => Some(device_id),
                _ => None,
            })
            .expect("scheduled authorization scenario should appear");
        assert!(fake.send(UiCommand::ApprovePeer(request.clone())).is_ok());
        let mac = fake
            .snapshot()
            .unwrap_or_default()
            .devices
            .into_iter()
            .find(|device| device.id == request);
        assert!(mac.is_some_and(|device| device.host_capable));
    }

    #[test]
    fn clipboard_toggle_requires_active_session_and_clears_on_disconnect() {
        let mut fake = FakeCore::new(83);
        assert!(fake.snapshot().expect("snapshot").clipboard_session_active);

        fake.send(UiCommand::SetClipboardEnabled(true))
            .expect("enable clipboard");
        assert!(fake.snapshot().expect("snapshot").clipboard_sync_enabled);

        fake.send(UiCommand::Disconnect).expect("disconnect");
        let disconnected = fake.snapshot().expect("snapshot");
        assert!(!disconnected.clipboard_session_active);
        assert!(!disconnected.clipboard_sync_enabled);

        fake.send(UiCommand::SetClipboardEnabled(true))
            .expect("attempt to enable while disconnected");
        assert!(!fake.snapshot().expect("snapshot").clipboard_sync_enabled);
    }

    #[test]
    fn event_polling_respects_limit_and_preserves_order() {
        let mut fake = FakeCore::default();
        let first = fake.poll_events(1).unwrap_or_default();
        let rest = fake.poll_events(32).unwrap_or_default();
        assert_eq!(first.len(), 1);
        assert!(matches!(first[0], CoreEvent::DeviceDiscovered(_)));
        assert!(matches!(rest[0], CoreEvent::DeviceDiscovered(_)));
        assert!(rest
            .iter()
            .any(|event| matches!(event, CoreEvent::DeviceOffline { .. })));
    }
}
