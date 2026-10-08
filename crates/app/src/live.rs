//! Live viewer adapter between the UI shell and the platform-neutral core.
#[cfg(windows)]
#[path = "clipboard_worker.rs"]
mod clipboard_worker;
#[cfg(target_os = "macos")]
#[path = "clipboard_worker_macos.rs"]
mod clipboard_worker_macos;

use racc_core::{
    ClipboardMetadata, CoreDeviceDiscovery, CoreError, CoreEvent, CoreHandle, CoreSnapshot,
    DeviceDiscoveryUpdate, DeviceId, DeviceSnapshot, DiscoveredPeer, FrameSink, FrameSource,
    Notification, NotificationLevel, QualityPreset, UiCommand, VideoFrame, ViewerCommand,
    ViewerRuntime, ViewerRuntimeConfig, ViewerRuntimeEvent,
};
#[cfg(any(windows, target_os = "macos"))]
use racc_core::{ClipboardPortAction, ClipboardTransferDirection, ClipboardTransferStatus};
use racc_session::SessionEvent;

use racc_proto::{InputEvent, OsType, SetQuality};
use racc_testkit::{FakeCore, DEFAULT_FAKE_SEED};
use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

/// Events returned by one bounded app polling cycle.
pub(crate) enum BackendEvents {
    /// Events from the deterministic fake core.
    Core(Vec<CoreEvent>),
    /// Events from one explicitly configured viewer connection.
    Viewer {
        /// Stable local identifier for the configured remote endpoint.
        device_id: DeviceId,
        /// Bounded metadata events from the viewer runtime.
        events: Vec<ViewerRuntimeEvent>,
        /// Latest immutable telemetry sample, when the worker has published one.
        telemetry: Option<Box<racc_telemetry::TelemetrySnapshot>>,
        /// Content-free clipboard status metadata.
        clipboard_metadata: Vec<ClipboardMetadata>,
    },
    /// No active backend is available.
    Empty,
}

/// App-side core selection; fake mode remains the default test harness.
pub(crate) enum AppBackend {
    /// Deterministic fake backend.
    Fake(Box<FakeCore>),
    /// Live Tailscale-only viewer.
    Live(Box<LiveViewer>),
    /// A startup failure retained for display in the UI.
    Unavailable(String),
}

impl AppBackend {
    /// Creates the existing fake backend and its separate frame source.
    pub(crate) fn fake() -> (Self, CoreSnapshot, Arc<dyn FrameSource>) {
        let fake = FakeCore::new(DEFAULT_FAKE_SEED);
        let snapshot = fake.snapshot().unwrap_or_default();
        let source = fake.frame_source();
        (Self::Fake(Box::new(fake)), snapshot, source)
    }

    /// Starts a discovery-first live viewer without selecting a peer.
    pub(crate) fn discover() -> Result<(Self, CoreSnapshot, Arc<dyn FrameSource>), String> {
        let live = LiveViewer::discover()?;
        let snapshot = live.snapshot();
        let source: Arc<dyn FrameSource> = live.frames.clone();
        Ok((Self::Live(Box::new(live)), snapshot, source))
    }

    /// Starts a live viewer for one explicitly supplied tailnet endpoint.
    pub(crate) fn connect(
        host_addr: SocketAddr,
        local_bind_ip: IpAddr,
    ) -> Result<(Self, CoreSnapshot, Arc<dyn FrameSource>), String> {
        let live = LiveViewer::connect(host_addr, local_bind_ip)?;
        let snapshot = live.snapshot();
        let source: Arc<dyn FrameSource> = live.frames.clone();
        Ok((Self::Live(Box::new(live)), snapshot, source))
    }

    /// Returns the current validated live peer catalogue for the device selector.
    #[allow(dead_code)]
    pub(crate) fn discovered_peers(&self) -> &[DiscoveredPeer] {
        match self {
            Self::Live(live) => &live.discovered_peers,
            Self::Fake(_) | Self::Unavailable(_) => &[],
        }
    }

    /// Returns validated local Tailscale addresses for family-matched viewer binds.
    #[allow(dead_code)]
    pub(crate) fn local_bind_addresses(&self) -> &[IpAddr] {
        match self {
            Self::Live(live) => &live.local_bind_addresses,
            Self::Fake(_) | Self::Unavailable(_) => &[],
        }
    }

    /// Retains a startup error so the user can read it in the app.
    pub(crate) fn unavailable(message: String) -> (Self, CoreSnapshot, Arc<dyn FrameSource>) {
        (
            Self::Unavailable(message),
            CoreSnapshot::default(),
            Arc::new(EmptyFrameSource),
        )
    }

    /// Submits one UI command to the selected backend.
    pub(crate) fn send(&mut self, command: UiCommand) -> Result<(), String> {
        match self {
            Self::Fake(core) => core.send(command).map_err(|error| error.to_string()),
            Self::Live(live) => live.send(command),
            Self::Unavailable(message) => Err(message.clone()),
        }
    }

    /// Tries to submit one validated keyboard event to the explicitly configured live peer.
    /// Returns `Ok(false)` when the bounded runtime command queue is full.
    pub(crate) fn send_input(
        &mut self,
        expected_device_id: &DeviceId,
        input: InputEvent,
    ) -> Result<bool, String> {
        match self {
            Self::Live(live) => live.send_input(expected_device_id, input),
            Self::Fake(_) => Err("Live input is unavailable in fake mode.".to_owned()),
            Self::Unavailable(message) => Err(message.clone()),
        }
    }

    /// Drains backend metadata without moving pixels through the event bus.
    pub(crate) fn poll_events(&mut self, max_events: usize) -> Result<BackendEvents, String> {
        match self {
            Self::Fake(core) => core
                .poll_events(max_events)
                .map(BackendEvents::Core)
                .map_err(|error| error.to_string()),
            Self::Live(live) => {
                if let Some(events) = live.poll_discovery_events(max_events)? {
                    return Ok(BackendEvents::Core(events));
                }
                let Some(device_id) = live.device_id.clone() else {
                    return Ok(BackendEvents::Core(Vec::new()));
                };
                let (events, telemetry, mut clipboard_metadata) = {
                    let Some(runtime) = live.runtime.as_ref() else {
                        return Ok(BackendEvents::Core(Vec::new()));
                    };
                    (
                        runtime
                            .poll_events(max_events)
                            .map_err(|error| error.to_string())?,
                        runtime
                            .poll_telemetry()
                            .map_err(|error| error.to_string())?,
                        runtime
                            .poll_clipboard_metadata(max_events)
                            .map_err(|error| error.to_string())?,
                    )
                };
                for event in &events {
                    match event {
                        ViewerRuntimeEvent::Connected(ack) => {
                            live.record_handshake(ack);
                            live.clipboard_session_active = true;
                            #[cfg(windows)]
                            if live.clipboard_enabled {
                                if let Some(worker) = live.clipboard_worker.as_ref() {
                                    worker.set_enabled(true);
                                }
                            }
                            #[cfg(target_os = "macos")]
                            if live.clipboard_enabled {
                                if let Some(worker) = live.clipboard_worker.as_ref() {
                                    worker.set_enabled(true);
                                }
                            }
                        }
                        ViewerRuntimeEvent::Disconnected
                        | ViewerRuntimeEvent::Failed(_)
                        | ViewerRuntimeEvent::Session(SessionEvent::Reconnecting) => {
                            live.clipboard_session_active = false;
                            live.remote_clipboard_capable = false;
                            #[cfg(windows)]
                            {
                                live.pending_clipboard_action = None;
                                if let Some(worker) = live.clipboard_worker.as_ref() {
                                    worker.set_enabled(false);
                                }
                            }
                            #[cfg(target_os = "macos")]
                            {
                                live.pending_clipboard_action = None;
                                if let Some(worker) = live.clipboard_worker.as_ref() {
                                    worker.set_enabled(false);
                                }
                            }
                        }
                        ViewerRuntimeEvent::Session(SessionEvent::SessionEnded) => {
                            live.clipboard_session_active = false;
                            live.clipboard_enabled = false;
                            live.remote_clipboard_capable = false;
                            #[cfg(windows)]
                            {
                                live.pending_clipboard_action = None;
                                if let Some(worker) = live.clipboard_worker.as_ref() {
                                    worker.set_enabled(false);
                                }
                            }
                            #[cfg(target_os = "macos")]
                            {
                                live.pending_clipboard_action = None;
                                if let Some(worker) = live.clipboard_worker.as_ref() {
                                    worker.set_enabled(false);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                #[cfg(windows)]
                if let (Some(runtime), Some(worker)) =
                    (live.runtime.as_ref(), live.clipboard_worker.as_ref())
                {
                    poll_clipboard_worker(
                        runtime,
                        worker,
                        live.clipboard_session_active,
                        &mut live.clipboard_enabled,
                        &mut live.pending_clipboard_action,
                        &mut clipboard_metadata,
                        max_events,
                    );
                }
                #[cfg(target_os = "macos")]
                if let (Some(runtime), Some(worker)) =
                    (live.runtime.as_ref(), live.clipboard_worker.as_ref())
                {
                    poll_macos_clipboard_worker(
                        runtime,
                        worker,
                        live.clipboard_session_active,
                        &mut live.clipboard_enabled,
                        &mut live.pending_clipboard_action,
                        &mut clipboard_metadata,
                        max_events,
                    );
                }
                Ok(BackendEvents::Viewer {
                    device_id,
                    events,
                    telemetry: telemetry.map(Box::new),
                    clipboard_metadata,
                })
            }
            Self::Unavailable(_) => Ok(BackendEvents::Empty),
        }
    }

    /// Returns a refreshed fake snapshot when the fake backend is active.
    pub(crate) fn snapshot(&self) -> Option<Result<CoreSnapshot, String>> {
        match self {
            Self::Fake(core) => Some(core.snapshot().map_err(|error| error.to_string())),
            Self::Live(_) | Self::Unavailable(_) => None,
        }
    }

    /// Advances deterministic fake time; live workers own their clocks.
    pub(crate) fn advance_to(&mut self, elapsed_us: u64) {
        if let Self::Fake(core) = self {
            core.advance_to(elapsed_us);
        }
    }

    /// Whether remote input capture is connected to this backend.
    pub(crate) fn is_live(&self) -> bool {
        matches!(self, Self::Live(_))
    }

    /// Whether this backend can read and write local clipboard text.
    pub(crate) fn clipboard_available(&self) -> bool {
        match self {
            Self::Fake(_) => true,
            Self::Live(live) => live.clipboard_available(),
            Self::Unavailable(_) => false,
        }
    }
}

/// One live adapter with metadata-only events and a latest-frame store.
pub(crate) struct LiveViewer {
    runtime: Option<ViewerRuntime>,
    device_id: Option<DeviceId>,
    frames: Arc<LatestFrameStore>,
    discovery: Option<DiscoveryWorker>,
    pending_core_events: VecDeque<CoreEvent>,
    discovered_peers: Vec<DiscoveredPeer>,
    local_bind_addresses: Vec<IpAddr>,
    discovered_local_name: Option<String>,
    visible: bool,
    clipboard_enabled: bool,
    clipboard_session_active: bool,
    remote_clipboard_capable: bool,
    #[cfg(windows)]
    clipboard_worker: Option<clipboard_worker::WindowsClipboardWorker>,
    #[cfg(target_os = "macos")]
    clipboard_worker: Option<clipboard_worker_macos::MacClipboardWorker>,
    #[cfg(any(windows, target_os = "macos"))]
    pending_clipboard_action: Option<ClipboardPortAction>,
}

impl LiveViewer {
    fn discover() -> Result<Self, String> {
        let discovery = DiscoveryWorker::spawn()?;
        let live = Self::empty(Some(discovery));
        live.request_discovery()?;
        Ok(live)
    }

    fn empty(discovery: Option<DiscoveryWorker>) -> Self {
        Self {
            runtime: None,
            device_id: None,
            frames: Arc::new(LatestFrameStore::default()),
            discovery,
            pending_core_events: VecDeque::with_capacity(MAX_PENDING_CORE_EVENTS),
            discovered_peers: Vec::new(),
            local_bind_addresses: Vec::new(),
            discovered_local_name: None,
            visible: true,
            clipboard_enabled: false,
            clipboard_session_active: false,
            remote_clipboard_capable: false,
            #[cfg(windows)]
            clipboard_worker: None,
            #[cfg(target_os = "macos")]
            clipboard_worker: None,
            #[cfg(any(windows, target_os = "macos"))]
            pending_clipboard_action: None,
        }
    }

    #[cfg(windows)]
    fn connect(host_addr: SocketAddr, local_bind_ip: IpAddr) -> Result<Self, String> {
        Self::connect_with_decoder_factory(
            host_addr,
            local_bind_ip,
            OsType::Windows,
            racc_decode::MediaFoundationDecoder::new,
        )
    }

    #[cfg(target_os = "macos")]
    fn connect(host_addr: SocketAddr, local_bind_ip: IpAddr) -> Result<Self, String> {
        Self::connect_with_decoder_factory(host_addr, local_bind_ip, OsType::MacOs, || {
            Ok(racc_decode::VideoToolboxDecoder::new())
        })
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn connect_with_decoder_factory<D, F>(
        host_addr: SocketAddr,
        local_bind_ip: IpAddr,
        local_os: OsType,
        decoder_factory: F,
    ) -> Result<Self, String>
    where
        D: racc_decode::Decoder + 'static,
        F: FnOnce() -> Result<D, racc_decode::DecodeError> + Send + 'static,
    {
        if !racc_core::is_tailscale_address(host_addr.ip())
            || !racc_core::is_tailscale_address(local_bind_ip)
        {
            return Err("Viewer addresses must be on the Tailscale interface.".to_owned());
        }
        let device_id = DeviceId::new(format!("tailnet-{}", host_addr.ip()))
            .map_err(|error| error.to_string())?;
        let frames = Arc::new(LatestFrameStore::default());
        let runtime = create_runtime(
            host_addr,
            local_bind_ip,
            local_device_name(),
            local_os,
            decoder_factory,
            Arc::clone(&frames),
        )?;
        let discovery = DiscoveryWorker::spawn().ok();
        let mut live = Self {
            runtime: Some(runtime),
            device_id: Some(device_id),
            frames,
            discovery,
            pending_core_events: VecDeque::with_capacity(MAX_PENDING_CORE_EVENTS),
            discovered_peers: Vec::new(),
            local_bind_addresses: Vec::new(),
            discovered_local_name: None,
            visible: true,
            clipboard_enabled: false,
            clipboard_session_active: false,
            remote_clipboard_capable: false,
            #[cfg(windows)]
            clipboard_worker: None,
            #[cfg(target_os = "macos")]
            clipboard_worker: None,
            #[cfg(any(windows, target_os = "macos"))]
            pending_clipboard_action: None,
        };
        #[cfg(windows)]
        live.ensure_clipboard_worker();
        #[cfg(target_os = "macos")]
        live.ensure_clipboard_worker();
        Ok(live)
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn connect(_host_addr: SocketAddr, _local_bind_ip: IpAddr) -> Result<Self, String> {
        Err("Live H.264 viewing is not supported on this platform.".to_owned())
    }

    fn snapshot(&self) -> CoreSnapshot {
        let connection_state = if self.runtime.is_some() {
            racc_telemetry::ConnectionState::Connecting
        } else {
            racc_telemetry::ConnectionState::Disconnected
        };
        CoreSnapshot {
            devices: self
                .discovered_peers
                .iter()
                .map(|peer| peer.device.clone())
                .collect(),
            selected_device: self.device_id.clone(),
            selected_display: None,
            local_device_name: self
                .discovered_local_name
                .clone()
                .unwrap_or_else(local_device_name),
            hosting_enabled: false,
            visible: self.visible,
            clipboard_session_active: false,
            clipboard_sync_enabled: false,
            telemetry: racc_telemetry::TelemetrySnapshot {
                session: racc_telemetry::SessionSnapshot {
                    connection_state,
                    ..racc_telemetry::SessionSnapshot::default()
                },
                ..racc_telemetry::TelemetrySnapshot::default()
            },
        }
    }

    fn request_discovery(&self) -> Result<(), String> {
        self.discovery
            .as_ref()
            .ok_or_else(|| "Device discovery worker could not start.".to_owned())?
            .request_refresh()
    }

    fn poll_discovery_events(
        &mut self,
        max_events: usize,
    ) -> Result<Option<Vec<CoreEvent>>, String> {
        if let Some(result) = self.discovery.as_ref().and_then(DiscoveryWorker::try_recv) {
            match result {
                Ok(update) => {
                    let peer_count = update.peers.len();
                    let local_device_name = update.local_device_name.clone();
                    self.discovered_peers = update.peers;
                    self.local_bind_addresses = update.local_addresses;
                    if let (Some(device_id), Some(runtime)) =
                        (self.device_id.as_ref(), self.runtime.as_ref())
                    {
                        let path = self
                            .discovered_peers
                            .iter()
                            .find(|peer| &peer.device.id == device_id)
                            .map(|peer| peer.path)
                            .unwrap_or(racc_telemetry::PathKind::Unknown);
                        let _ = runtime.send(ViewerCommand::SetPath(path));
                    }
                    if let Some(local_name) = local_device_name.as_ref() {
                        self.discovered_local_name = Some(local_name.clone());
                    }
                    for event in update.events {
                        self.push_core_event(event);
                    }
                    self.push_core_event(CoreEvent::DeviceDiscoveryCompleted {
                        peer_count,
                        local_device_name,
                    });
                }
                Err(message) => self.push_core_event(CoreEvent::Notification(Notification {
                    level: NotificationLevel::Warning,
                    title: "Device discovery failed".to_owned(),
                    message,
                })),
            }
        }
        if max_events == 0 || self.pending_core_events.is_empty() {
            return Ok(None);
        }
        let count = max_events.min(self.pending_core_events.len());
        Ok(Some(self.pending_core_events.drain(..count).collect()))
    }

    fn push_core_event(&mut self, event: CoreEvent) {
        if self.pending_core_events.len() == MAX_PENDING_CORE_EVENTS {
            self.pending_core_events.pop_front();
        }
        self.pending_core_events.push_back(event);
    }

    fn record_handshake(&mut self, ack: &racc_proto::HelloAck) {
        let Some(device_id) = self.device_id.clone() else {
            return;
        };
        self.remote_clipboard_capable = ack.features & racc_proto::FEATURE_TEXT_CLIPBOARD != 0;
        let device = DeviceSnapshot {
            id: device_id,
            name: ack.device_name.chars().take(128).collect(),
            os: ack.os,
            online: true,
            host_capable: true,
            displays: Vec::new(),
            streamed_display: None,
        };
        self.push_core_event(CoreEvent::DeviceDiscovered(device));
    }

    fn clipboard_available(&self) -> bool {
        #[cfg(windows)]
        {
            self.clipboard_worker.is_some() && self.remote_clipboard_capable
        }
        #[cfg(target_os = "macos")]
        {
            self.clipboard_worker.is_some() && self.remote_clipboard_capable
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            false
        }
    }

    #[cfg(windows)]
    fn ensure_clipboard_worker(&mut self) {
        if self.clipboard_worker.is_none() {
            self.clipboard_worker = clipboard_worker::WindowsClipboardWorker::start().ok();
        }
    }

    #[cfg(target_os = "macos")]
    fn ensure_clipboard_worker(&mut self) {
        if self.clipboard_worker.is_none() {
            self.clipboard_worker = clipboard_worker_macos::MacClipboardWorker::start().ok();
        }
    }

    fn set_clipboard_enabled(&mut self, enabled: bool) -> Result<(), String> {
        if enabled && !self.clipboard_available() {
            return Err("The local platform clipboard adapter is unavailable.".to_owned());
        }
        if enabled && !self.remote_clipboard_capable {
            return Err("The selected host does not advertise text clipboard support.".to_owned());
        }
        if enabled && !self.clipboard_session_active {
            return Err("Clipboard sync can only be enabled for an active session.".to_owned());
        }
        if !enabled {
            self.clipboard_enabled = false;
            #[cfg(windows)]
            {
                self.pending_clipboard_action = None;
                if let Some(worker) = self.clipboard_worker.as_ref() {
                    worker.set_enabled(false);
                }
            }
            #[cfg(target_os = "macos")]
            {
                self.pending_clipboard_action = None;
                if let Some(worker) = self.clipboard_worker.as_ref() {
                    worker.set_enabled(false);
                }
            }
        }
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| "No live viewer session is connected.".to_owned())?;
        runtime
            .send(ViewerCommand::SetClipboardEnabled(enabled))
            .map_err(|error| error.to_string())?;
        if enabled {
            self.clipboard_enabled = true;
            if self.clipboard_session_active {
                #[cfg(windows)]
                if let Some(worker) = self.clipboard_worker.as_ref() {
                    worker.set_enabled(true);
                }
                #[cfg(target_os = "macos")]
                if let Some(worker) = self.clipboard_worker.as_ref() {
                    worker.set_enabled(true);
                }
            }
        }
        Ok(())
    }

    fn send_input(
        &mut self,
        expected_device_id: &DeviceId,
        input: InputEvent,
    ) -> Result<bool, String> {
        if Some(expected_device_id) != self.device_id.as_ref() {
            return Err("Input target does not match the active Tailscale peer.".to_owned());
        }
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| "No live viewer session is connected.".to_owned())?;
        match runtime.send(ViewerCommand::SendInput(input)) {
            Ok(()) => Ok(true),
            Err(racc_core::ViewerRuntimeError::QueueFull) => Ok(false),
            Err(error) => Err(error.to_string()),
        }
    }

    fn send(&mut self, command: UiCommand) -> Result<(), String> {
        match command {
            UiCommand::Connect(device_id) | UiCommand::SelectDevice(device_id) => {
                self.connect_peer(&device_id)
            }
            UiCommand::SelectDisplay {
                device_id,
                display_id,
            } if Some(&device_id) == self.device_id.as_ref() => self
                .runtime
                .as_ref()
                .ok_or_else(|| "No live viewer session is connected.".to_owned())?
                .send(ViewerCommand::SelectDisplay(display_id.get()))
                .map_err(|error| error.to_string()),
            UiCommand::SetQuality { device_id, quality }
                if Some(&device_id) == self.device_id.as_ref() =>
            {
                self.runtime
                    .as_ref()
                    .ok_or_else(|| "No live viewer session is connected.".to_owned())?
                    .send(ViewerCommand::SetQuality(quality_message(quality)))
                    .map_err(|error| error.to_string())
            }
            UiCommand::SetVisible(visible) => {
                self.visible = visible;
                if let Some(runtime) = self.runtime.as_ref() {
                    runtime
                        .send(ViewerCommand::SetVisible(visible))
                        .map_err(|error| error.to_string())
                } else {
                    Ok(())
                }
            }
            UiCommand::Disconnect => {
                self.device_id = None;
                self.clipboard_enabled = false;
                self.clipboard_session_active = false;
                #[cfg(windows)]
                {
                    self.pending_clipboard_action = None;
                    if let Some(worker) = self.clipboard_worker.as_ref() {
                        worker.set_enabled(false);
                    }
                }
                #[cfg(target_os = "macos")]
                {
                    self.pending_clipboard_action = None;
                    if let Some(worker) = self.clipboard_worker.as_ref() {
                        worker.set_enabled(false);
                    }
                }
                if let Some(mut runtime) = self.runtime.take() {
                    runtime.close().map_err(|error| error.to_string())?;
                }
                Ok(())
            }
            UiCommand::DiscoverDevices => self.request_discovery(),
            UiCommand::SetClipboardEnabled(enabled) => self.set_clipboard_enabled(enabled),
            UiCommand::SelectDisplay { .. } | UiCommand::SetQuality { .. } => {
                Err("The selected peer does not match the active live session.".to_owned())
            }
            UiCommand::ToggleKeyboardCapture(_)
            | UiCommand::ToggleMouseCapture(_)
            | UiCommand::ApprovePeer(_)
            | UiCommand::RejectPeer(_)
            | UiCommand::RemovePeer(_)
            | UiCommand::SetHosting(_) => {
                Err("This live viewer control is not connected yet.".to_owned())
            }
        }
    }

    fn connect_peer(&mut self, device_id: &DeviceId) -> Result<(), String> {
        if self.device_id.as_ref() == Some(device_id) && self.runtime.is_some() {
            return Ok(());
        }
        let peer = self
            .discovered_peers
            .iter()
            .find(|peer| &peer.device.id == device_id)
            .cloned()
            .ok_or_else(|| {
                "The selected peer is no longer in the Tailscale device list.".to_owned()
            })?;
        let (host_addr, local_ip) = peer_endpoint(&peer, &self.local_bind_addresses)?;
        self.clipboard_enabled = false;
        self.clipboard_session_active = false;
        self.remote_clipboard_capable = false;
        #[cfg(windows)]
        {
            self.pending_clipboard_action = None;
            if let Some(worker) = self.clipboard_worker.as_ref() {
                worker.set_enabled(false);
            }
        }
        #[cfg(target_os = "macos")]
        {
            self.pending_clipboard_action = None;
            if let Some(worker) = self.clipboard_worker.as_ref() {
                worker.set_enabled(false);
            }
        }
        let runtime = self.spawn_runtime(host_addr, local_ip)?;
        #[cfg(windows)]
        self.ensure_clipboard_worker();
        #[cfg(target_os = "macos")]
        self.ensure_clipboard_worker();
        let _ = runtime.send(ViewerCommand::SetPath(peer.path));
        if let Some(mut previous) = self.runtime.replace(runtime) {
            previous.close().map_err(|error| error.to_string())?;
        }
        self.device_id = Some(device_id.clone());
        self.push_core_event(CoreEvent::DeviceDiscovered(peer.device));
        Ok(())
    }

    #[cfg(windows)]
    fn spawn_runtime(&self, host: SocketAddr, local: IpAddr) -> Result<ViewerRuntime, String> {
        create_runtime(
            host,
            local,
            local_device_name(),
            OsType::Windows,
            racc_decode::MediaFoundationDecoder::new,
            Arc::clone(&self.frames),
        )
    }

    #[cfg(target_os = "macos")]
    fn spawn_runtime(&self, host: SocketAddr, local: IpAddr) -> Result<ViewerRuntime, String> {
        create_runtime(
            host,
            local,
            local_device_name(),
            OsType::MacOs,
            || Ok(racc_decode::VideoToolboxDecoder::new()),
            Arc::clone(&self.frames),
        )
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn spawn_runtime(&self, _host: SocketAddr, _local: IpAddr) -> Result<ViewerRuntime, String> {
        Err("Live H.264 viewing is not supported on this platform.".to_owned())
    }
}

fn peer_endpoint(
    peer: &DiscoveredPeer,
    local_addresses: &[IpAddr],
) -> Result<(SocketAddr, IpAddr), String> {
    if !peer.device.online || !peer.device.host_capable {
        return Err("The selected peer is offline or is not running a compatible host.".to_owned());
    }
    let (host_ip, local_ip) =
        select_address_pair(&peer.addresses, local_addresses).ok_or_else(|| {
            "No matching Tailscale address pair is available for this peer.".to_owned()
        })?;
    Ok((
        SocketAddr::new(host_ip, racc_core::DEFAULT_CONTROL_PORT),
        local_ip,
    ))
}

fn select_address_pair(peers: &[IpAddr], locals: &[IpAddr]) -> Option<(IpAddr, IpAddr)> {
    for ipv4 in [true, false] {
        if let Some(local) = locals
            .iter()
            .copied()
            .find(|address| racc_core::is_tailscale_address(*address) && address.is_ipv4() == ipv4)
        {
            if let Some(peer) = peers.iter().copied().find(|address| {
                racc_core::is_tailscale_address(*address) && address.is_ipv4() == ipv4
            }) {
                return Some((peer, local));
            }
        }
    }
    None
}

#[cfg(any(windows, target_os = "macos"))]
#[cfg(windows)]
fn poll_clipboard_worker(
    runtime: &ViewerRuntime,
    worker: &clipboard_worker::WindowsClipboardWorker,
    session_active: bool,
    clipboard_enabled: &mut bool,
    pending_action: &mut Option<ClipboardPortAction>,
    metadata: &mut Vec<ClipboardMetadata>,
    max_events: usize,
) {
    use clipboard_worker::{ClipboardWorkerStatus as Status, QueueRemoteResult};

    let Some(drain) = worker.drain(max_events) else {
        return;
    };
    let adapter_failure = drain
        .statuses
        .iter()
        .any(|status| matches!(status, Status::ListenFailed(_) | Status::LocalReadFailed(_)));
    if adapter_failure && *clipboard_enabled {
        *clipboard_enabled = false;
        *pending_action = None;
        worker.set_enabled(false);
        let _ = runtime.send(ViewerCommand::SetClipboardEnabled(false));
    }
    if let Some(text) = drain.local_text {
        if session_active
            && *clipboard_enabled
            && runtime.notify_local_clipboard_change(&text).is_err()
        {
            push_clipboard_metadata(
                metadata,
                max_events,
                ClipboardTransferDirection::LocalToRemote,
                None,
                None,
                ClipboardTransferStatus::AdapterFailed,
            );
            *clipboard_enabled = false;
            *pending_action = None;
            worker.set_enabled(false);
            let _ = runtime.send(ViewerCommand::SetClipboardEnabled(false));
        }
    }
    for status in drain.statuses {
        let (direction, sequence, byte_len, transfer_status) = match status {
            Status::Enabled | Status::Disabled => continue,
            Status::ListenFailed(_) | Status::LocalReadFailed(_) => (
                ClipboardTransferDirection::LocalToRemote,
                None,
                None,
                ClipboardTransferStatus::AdapterFailed,
            ),
            Status::LocalTextTooLarge => (
                ClipboardTransferDirection::LocalToRemote,
                None,
                None,
                ClipboardTransferStatus::RejectedTooLarge,
            ),
            Status::RemoteApplied { sequence } => (
                ClipboardTransferDirection::RemoteToLocal,
                Some(sequence),
                None,
                ClipboardTransferStatus::AdapterApplied,
            ),
            Status::RemoteApplyFailed { sequence, .. } => (
                ClipboardTransferDirection::RemoteToLocal,
                Some(sequence),
                None,
                ClipboardTransferStatus::AdapterFailed,
            ),
        };
        push_clipboard_metadata(
            metadata,
            max_events,
            direction,
            sequence,
            byte_len,
            transfer_status,
        );
    }

    if !session_active || !*clipboard_enabled {
        *pending_action = None;
        let _ = runtime.poll_clipboard_actions(max_events);
        return;
    }
    if let Ok(actions) = runtime.poll_clipboard_actions(max_events) {
        if let Some(newest) = actions.into_iter().last() {
            *pending_action = Some(newest);
        }
    }
    let Some(action) = pending_action.take() else {
        return;
    };
    let retry = action.clone();
    match worker.queue_remote_text(action.sequence, action.text.into_bytes()) {
        QueueRemoteResult::Queued => {}
        QueueRemoteResult::Busy => *pending_action = Some(retry),
        QueueRemoteResult::Disabled | QueueRemoteResult::Stopped => {}
        QueueRemoteResult::TooLarge => push_clipboard_metadata(
            metadata,
            max_events,
            ClipboardTransferDirection::RemoteToLocal,
            Some(retry.sequence),
            None,
            ClipboardTransferStatus::RejectedTooLarge,
        ),
        QueueRemoteResult::InvalidUtf8 => push_clipboard_metadata(
            metadata,
            max_events,
            ClipboardTransferDirection::RemoteToLocal,
            Some(retry.sequence),
            None,
            ClipboardTransferStatus::RejectedInvalidUtf8,
        ),
    }
}

#[cfg(target_os = "macos")]
fn poll_macos_clipboard_worker(
    runtime: &ViewerRuntime,
    worker: &clipboard_worker_macos::MacClipboardWorker,
    session_active: bool,
    clipboard_enabled: &mut bool,
    pending_action: &mut Option<ClipboardPortAction>,
    metadata: &mut Vec<ClipboardMetadata>,
    max_events: usize,
) {
    use clipboard_worker_macos::{ClipboardWorkerStatus as Status, QueueRemoteResult};

    let Some(drain) = worker.drain(max_events) else {
        return;
    };
    let adapter_failure = drain
        .statuses
        .iter()
        .any(|status| matches!(status, Status::ListenFailed(_) | Status::LocalReadFailed(_)));
    if adapter_failure && *clipboard_enabled {
        *clipboard_enabled = false;
        *pending_action = None;
        worker.set_enabled(false);
        let _ = runtime.send(ViewerCommand::SetClipboardEnabled(false));
    }
    if let Some(text) = drain.local_text {
        if session_active
            && *clipboard_enabled
            && runtime.notify_local_clipboard_change(&text).is_err()
        {
            push_clipboard_metadata(
                metadata,
                max_events,
                ClipboardTransferDirection::LocalToRemote,
                None,
                None,
                ClipboardTransferStatus::AdapterFailed,
            );
            *clipboard_enabled = false;
            *pending_action = None;
            worker.set_enabled(false);
            let _ = runtime.send(ViewerCommand::SetClipboardEnabled(false));
        }
    }
    for status in drain.statuses {
        let (direction, sequence, byte_len, transfer_status) = match status {
            Status::Enabled | Status::Disabled => continue,
            Status::ListenFailed(_) | Status::LocalReadFailed(_) => (
                ClipboardTransferDirection::LocalToRemote,
                None,
                None,
                ClipboardTransferStatus::AdapterFailed,
            ),
            Status::LocalTextTooLarge => (
                ClipboardTransferDirection::LocalToRemote,
                None,
                None,
                ClipboardTransferStatus::RejectedTooLarge,
            ),
            Status::RemoteApplied { sequence } => (
                ClipboardTransferDirection::RemoteToLocal,
                Some(sequence),
                None,
                ClipboardTransferStatus::AdapterApplied,
            ),
            Status::RemoteApplyFailed { sequence, .. } => (
                ClipboardTransferDirection::RemoteToLocal,
                Some(sequence),
                None,
                ClipboardTransferStatus::AdapterFailed,
            ),
        };
        push_clipboard_metadata(
            metadata,
            max_events,
            direction,
            sequence,
            byte_len,
            transfer_status,
        );
    }

    if !session_active || !*clipboard_enabled {
        *pending_action = None;
        let _ = runtime.poll_clipboard_actions(max_events);
        return;
    }
    if let Ok(actions) = runtime.poll_clipboard_actions(max_events) {
        if let Some(newest) = actions.into_iter().last() {
            *pending_action = Some(newest);
        }
    }
    let Some(action) = pending_action.take() else {
        return;
    };
    let retry = action.clone();
    match worker.queue_remote_text(action.sequence, action.text.into_bytes()) {
        QueueRemoteResult::Queued => {}
        QueueRemoteResult::Busy => *pending_action = Some(retry),
        QueueRemoteResult::Disabled | QueueRemoteResult::Stopped => {}
        QueueRemoteResult::TooLarge => push_clipboard_metadata(
            metadata,
            max_events,
            ClipboardTransferDirection::RemoteToLocal,
            Some(retry.sequence),
            None,
            ClipboardTransferStatus::RejectedTooLarge,
        ),
        QueueRemoteResult::InvalidUtf8 => push_clipboard_metadata(
            metadata,
            max_events,
            ClipboardTransferDirection::RemoteToLocal,
            Some(retry.sequence),
            None,
            ClipboardTransferStatus::RejectedInvalidUtf8,
        ),
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn push_clipboard_metadata(
    metadata: &mut Vec<ClipboardMetadata>,
    max_events: usize,
    direction: ClipboardTransferDirection,
    sequence: Option<u64>,
    byte_len: Option<u32>,
    status: ClipboardTransferStatus,
) {
    const EXTRA_STATUS_BUDGET: usize = 8;
    if metadata.len() >= max_events.saturating_add(EXTRA_STATUS_BUDGET) {
        return;
    }
    metadata.push(ClipboardMetadata {
        direction,
        sequence,
        byte_len,
        status,
    });
}

impl Drop for LiveViewer {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(mut worker) = self.clipboard_worker.take() {
            worker.request_shutdown();
            let _ = worker.join();
        }
        #[cfg(target_os = "macos")]
        if let Some(mut worker) = self.clipboard_worker.take() {
            worker.request_shutdown();
            let _ = worker.join();
        }
    }
}

fn create_runtime<D, F>(
    host_addr: SocketAddr,
    local_bind_ip: IpAddr,
    local_name: String,
    local_os: OsType,
    decoder_factory: F,
    frames: Arc<LatestFrameStore>,
) -> Result<ViewerRuntime, String>
where
    D: racc_decode::Decoder + 'static,
    F: FnOnce() -> Result<D, racc_decode::DecodeError> + Send + 'static,
{
    let config = ViewerRuntimeConfig::new(
        host_addr,
        SocketAddr::new(local_bind_ip, 0),
        local_name,
        local_os,
    );
    ViewerRuntime::spawn_with_decoder_factory(config, decoder_factory, SharedFrameSink(frames))
        .map_err(|error| error.to_string())
}

const MAX_PENDING_CORE_EVENTS: usize = 1024;
const DISCOVERY_REFRESH_INTERVAL: Duration = Duration::from_secs(15);

struct DiscoveryWorker {
    requests: SyncSender<()>,
    results: Receiver<Result<DeviceDiscoveryUpdate, String>>,
}

impl DiscoveryWorker {
    fn spawn() -> Result<Self, String> {
        let mut discovery = CoreDeviceDiscovery::system().map_err(|error| error.to_string())?;
        let (requests, request_rx) = mpsc::sync_channel(1);
        let (result_tx, results) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("racc-peer-discovery".to_owned())
            .spawn(move || loop {
                let result = discovery.refresh().map_err(|error| error.to_string());
                if result_tx.send(result).is_err() {
                    break;
                }
                match request_rx.recv_timeout(DISCOVERY_REFRESH_INTERVAL) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self { requests, results })
    }

    fn request_refresh(&self) -> Result<(), String> {
        match self.requests.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(()),
            Err(TrySendError::Disconnected(())) => {
                Err("Device discovery worker has stopped.".to_owned())
            }
        }
    }

    fn try_recv(&self) -> Option<Result<DeviceDiscoveryUpdate, String>> {
        self.results.try_recv().ok()
    }
}
fn quality_message(quality: QualityPreset) -> SetQuality {
    let (max_height, bitrate_hint_kbps) = match quality {
        QualityPreset::P480 => (480, 1500),
        QualityPreset::P720 => (720, 3500),
        QualityPreset::P1080 => (1080, 7000),
        QualityPreset::Auto => (0, 0),
    };
    SetQuality {
        max_height,
        bitrate_hint_kbps,
    }
}

/// Replaces the current frame atomically; no queue of stale decoded frames builds up.
#[derive(Default)]
struct LatestFrameStore {
    latest: RwLock<Option<Arc<VideoFrame>>>,
}

impl FrameSink for LatestFrameStore {
    fn publish_frame(&self, frame: Arc<VideoFrame>) -> Result<(), CoreError> {
        frame.validate()?;
        *self
            .latest
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(frame);
        Ok(())
    }
}

struct SharedFrameSink(Arc<LatestFrameStore>);

impl FrameSink for SharedFrameSink {
    fn publish_frame(&self, frame: Arc<VideoFrame>) -> Result<(), CoreError> {
        self.0.publish_frame(frame)
    }
}

impl FrameSource for LatestFrameStore {
    fn latest_frame(&self) -> Option<Arc<VideoFrame>> {
        self.latest
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Empty source used when strict live endpoint validation fails.
struct EmptyFrameSource;

impl FrameSource for EmptyFrameSource {
    fn latest_frame(&self) -> Option<Arc<VideoFrame>> {
        None
    }
}

#[cfg(windows)]
fn local_device_name() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .map(|name| name.chars().take(64).collect())
        .unwrap_or_else(|| "Windows viewer".to_owned())
}

#[cfg(not(windows))]
fn local_device_name() -> String {
    "Viewer".to_owned()
}

#[cfg(test)]
mod tests {
    use super::{peer_endpoint, select_address_pair};
    use racc_core::{DeviceId, DeviceSnapshot, DiscoveredPeer};
    use racc_proto::OsType;
    use std::net::IpAddr;

    #[test]
    fn address_selection_prefers_valid_ipv4_pair_and_never_mixes_families() {
        let peers = [
            "fd7a:115c:a1e0::10"
                .parse::<IpAddr>()
                .expect("valid test IPv6"),
            "100.64.0.10".parse::<IpAddr>().expect("valid test IPv4"),
        ];
        let locals = [
            "fd7a:115c:a1e0::20"
                .parse::<IpAddr>()
                .expect("valid test IPv6"),
            "100.64.0.20".parse::<IpAddr>().expect("valid test IPv4"),
        ];
        assert_eq!(
            select_address_pair(&peers, &locals),
            Some((peers[1], locals[1]))
        );
        assert_eq!(
            select_address_pair(&peers[..1], &locals[1..]),
            None,
            "addresses from different families must not be paired"
        );
    }

    #[test]
    fn selected_peer_endpoint_checks_readiness_and_same_family_addresses() {
        let address = "100.64.0.10".parse::<IpAddr>().expect("valid test IPv4");
        let local = "100.64.0.20".parse::<IpAddr>().expect("valid test IPv4");
        let peer = DiscoveredPeer {
            device: DeviceSnapshot {
                id: DeviceId::new("peer-1").expect("valid test id"),
                name: "test peer".to_owned(),
                os: OsType::Windows,
                online: true,
                host_capable: true,
                displays: Vec::new(),
                streamed_display: None,
            },
            addresses: vec![address],
            path: racc_telemetry::PathKind::Direct,
        };
        assert_eq!(
            peer_endpoint(&peer, &[local]),
            Ok((
                std::net::SocketAddr::new(address, racc_core::DEFAULT_CONTROL_PORT),
                local,
            ))
        );
        let mut offline = peer.clone();
        offline.device.online = false;
        assert!(peer_endpoint(&offline, &[local]).is_err());
        let mut not_host = peer;
        not_host.device.host_capable = false;
        assert!(peer_endpoint(&not_host, &[local]).is_err());
    }

    #[test]
    fn address_selection_skips_non_tailscale_addresses() {
        let peers = ["192.168.1.10".parse::<IpAddr>().expect("valid test IPv4")];
        let locals = ["100.64.0.20".parse::<IpAddr>().expect("valid test IPv4")];
        assert_eq!(select_address_pair(&peers, &locals), None);
    }
}
