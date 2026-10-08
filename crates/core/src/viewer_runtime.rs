//! Threaded Tailscale-only viewer connection and decode runtime.
//!
//! Control and video work run off the UI thread. Decoded pixels travel only to
//! the supplied FrameSink; public events contain metadata, never image planes.
use crate::{FrameSink, ViewerFramePipeline, ViewerQueueOutcome};
use racc_clipboard::{
    ClipboardDirections, ClipboardRejection, ClipboardSync, LocalChangeResult, OriginId,
    RemoteChangeResult, MAX_CLIPBOARD_BYTES,
};
use racc_decode::{DecodeError, Decoder, DecoderKind as DecodeBackendKind, EncodedAccessUnit};
use racc_net::{
    connect_control, BindPolicy, ControlConn, ControlReadHalf, ControlSettings, ControlWriteHalf,
    NetError, VideoReceiver, VideoTransportEvent,
};
use racc_proto::{
    ClipboardOrigin, ClipboardSyncControl, ControlMessage, CursorShape, CursorUpdate, Hello,
    HelloAck, HelloStatus, HostEventKind, InputEvent, LogicalClock, OsType, Ping, Pong, SetQuality,
    StatsReport, StreamReset, StreamStatus, ViewerReport, CLIPBOARD_LOGICAL_CLOCK_VERSION,
    MAX_VIEWER_REPORT_DROPPED_FRAMES, MAX_VIEWER_REPORT_DURATION_MS, PROTOCOL_VERSION,
};
use racc_session::{SessionEvent, ViewerAction, ViewerSession};
use racc_telemetry::{
    CodecKind, ConnectionState, DecoderKind, EventKind, PathKind, TelemetryHub, TelemetrySnapshot,
};
use racc_topology::{is_revision_newer, Topology};
use std::collections::VecDeque;
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Maximum metadata events retained for the app adapter.
pub const VIEWER_RUNTIME_EVENT_CAPACITY: usize = 64;
const INPUT_CAPACITY: usize = 16;
const VIDEO_COMMAND_CAPACITY: usize = 8;
const CONTROL_POLL: Duration = Duration::from_millis(100);
const CONTROL_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const RUNTIME_POLL: Duration = Duration::from_millis(20);
const VIDEO_POLL: Duration = Duration::from_millis(5);
const MAX_ERROR_BYTES: usize = 256;
const CLIPBOARD_ACTION_CAPACITY: usize = 8;
const CLIPBOARD_METADATA_CAPACITY: usize = 16;
const TELEMETRY_INTERVAL: Duration = Duration::from_millis(250);
const PING_INTERVAL: Duration = Duration::from_secs(1);
const VIEWER_REPORT_INTERVAL_US: u64 = 1_000_000;
const VIEWER_DECODE_SAMPLE_CAPACITY: usize = 120;
const PING_TIMEOUT_US: u64 = 5_000_000;
const VIEWER_CLIPBOARD_ORIGIN: OriginId = OriginId(0);
const HOST_CLIPBOARD_ORIGIN: OriginId = OriginId(1);

/// Remote endpoint and viewer capabilities. Both IP addresses must be Tailscale
/// addresses. A zero local UDP port requests an ephemeral port announced in Hello.
#[derive(Clone, Debug)]
pub struct ViewerRuntimeConfig {
    /// Remote host control endpoint.
    pub host_addr: SocketAddr,
    /// Local Tailscale address for the video receiver.
    pub local_video_bind: SocketAddr,
    /// Viewer device label.
    pub device_name: String,
    /// Viewer operating system.
    pub os: OsType,
    /// Viewer application version.
    pub app_version: String,
    /// Maximum requested stream height.
    pub max_height: u16,
    /// Supported protocol feature bits.
    pub features: u32,
}
impl ViewerRuntimeConfig {
    /// Creates an H.264 viewer config with the current package version and 1080p cap.
    pub fn new(
        host_addr: SocketAddr,
        local_video_bind: SocketAddr,
        device_name: impl Into<String>,
        os: OsType,
    ) -> Self {
        Self {
            host_addr,
            local_video_bind,
            device_name: device_name.into(),
            os,
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            max_height: 1080,
            features: 0,
        }
    }
    fn validate(&self, policy: BindPolicy) -> Result<(), ViewerRuntimeError> {
        if self.host_addr.port() == 0 {
            return Err(ViewerRuntimeError::InvalidConfig(
                "host control port must be nonzero",
            ));
        }
        if self.host_addr.is_ipv4() != self.local_video_bind.is_ipv4() {
            return Err(ViewerRuntimeError::InvalidConfig(
                "host and local video addresses must use the same IP family",
            ));
        }
        if !matches!(self.max_height, 480 | 720 | 1080) {
            return Err(ViewerRuntimeError::InvalidConfig(
                "maximum height must be 480, 720, or 1080",
            ));
        }
        racc_net::validate_bind_addr(self.host_addr.ip(), policy)
            .map_err(|_| ViewerRuntimeError::InvalidConfig("host address is outside Tailscale"))?;
        racc_net::validate_bind_addr(self.local_video_bind.ip(), policy).map_err(|_| {
            ViewerRuntimeError::InvalidConfig("local video address is outside Tailscale")
        })?;
        Ok(())
    }
}

/// Commands accepted by the runtime. Input events should be coalesced by the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewerCommand {
    /// Select a remote display.
    SelectDisplay(u32),
    /// Pause or resume video when the app is hidden or shown.
    SetVisible(bool),
    /// Set the current route classification from the latest Tailscale status.
    SetPath(PathKind),
    /// Set the host-selected quality preference.
    SetQuality(SetQuality),
    /// Forward one already validated input event.
    SendInput(InputEvent),
    /// Send viewer telemetry feedback.
    SendReport(ViewerReport),
    /// Enable or disable bidirectional text clipboard synchronization for this session.
    SetClipboardEnabled(bool),
    /// Close and send Goodbye.
    Close,
}

/// Direction of one clipboard transfer represented in bounded status metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardTransferDirection {
    /// Local clipboard text was sent to the host.
    LocalToRemote,
    /// Host clipboard text was accepted and queued for a local platform adapter.
    RemoteToLocal,
}

/// Outcome of one clipboard update without containing clipboard text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardTransferStatus {
    /// Sync is enabled for the active session.
    Enabled,
    /// Sync is disabled or the session is not active.
    Disabled,
    /// The update was sent to the peer.
    Sent,
    /// The update passed policy and is waiting in the separate clipboard action queue.
    ReceivedPendingApply,
    /// The newest update was queued and the oldest pending action was replaced because the queue was full.
    ApplyQueueFull,
    /// Text exceeded the 512 KiB policy limit.
    RejectedTooLarge,
    /// Text was not valid UTF-8.
    RejectedInvalidUtf8,
    /// The update matched an echo or duplicate and was ignored.
    Ignored,
    /// The text was successfully written to the local platform clipboard.
    AdapterApplied,
    /// A platform clipboard listener, read, or write operation failed.
    AdapterFailed,
    /// The policy accepted a local change and is waiting for the rate-limit tick.
    Queued,
}

/// Bounded clipboard status metadata; it never contains clipboard text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipboardMetadata {
    /// Direction of the update.
    pub direction: ClipboardTransferDirection,
    /// Sender sequence when available.
    pub sequence: Option<u64>,
    /// Text byte count when available.
    pub byte_len: Option<u32>,
    /// Policy or transport outcome.
    pub status: ClipboardTransferStatus,
}

/// One accepted remote clipboard value for a platform adapter.
///
/// This content-bearing action uses a separate bounded queue and is never placed
/// in `ViewerRuntimeEvent` or any UI metadata snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardPortAction {
    /// Remote sender sequence.
    pub sequence: u64,
    /// UTF-8 text to apply to the local clipboard.
    pub text: String,
}

/// Runtime lifecycle and telemetry events. Video frames use FrameSink only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewerRuntimeEvent {
    /// A connection attempt began.
    Connecting,
    /// The host accepted the handshake.
    Connected(HelloAck),
    /// A new validated topology arrived.
    TopologyChanged(Topology),
    /// The host announced an accepted stream reset.
    StreamReset(StreamReset),
    /// The local decoder factory selected this backend.
    DecoderSelected(DecoderKind),
    /// A decoded frame was published separately to FrameSink.
    FrameAvailable {
        /// Stream epoch.
        epoch: u16,
        /// Published frame ID.
        frame_id: u32,
    },
    /// Remote cursor position metadata.
    CursorPosition(CursorUpdate),
    /// Remote cursor bitmap metadata.
    CursorShape(CursorShape),
    /// Host CPU and capture/encoder statistics.
    HostStats(StatsReport),
    /// Portable viewer lifecycle event.
    Session(SessionEvent),
    /// The control connection ended; a fresh runtime may be created to retry.
    Disconnected,
    /// A bounded runtime failure.
    Failed(String),
    /// Viewer visibility changed.
    VisibilityChanged(bool),
}

/// Failure to configure or use the runtime handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewerRuntimeError {
    /// Invalid endpoint, capability, or Tailscale address.
    InvalidConfig(&'static str),
    /// The finite command queue is full.
    QueueFull,
    /// The runtime has stopped.
    Closed,
    /// A worker thread could not be started.
    ThreadSpawn(String),
    /// A worker panicked while joining.
    WorkerPanicked,
}
impl fmt::Display for ViewerRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(reason) => write!(f, "invalid viewer config: {reason}"),
            Self::QueueFull => f.write_str("viewer command queue is full"),
            Self::Closed => f.write_str("viewer runtime is closed"),
            Self::ThreadSpawn(reason) => write!(f, "could not start viewer worker: {reason}"),
            Self::WorkerPanicked => f.write_str("viewer worker panicked"),
        }
    }
}
impl std::error::Error for ViewerRuntimeError {}

/// Handle for submitting bounded commands and polling connection metadata.
pub struct ViewerRuntime {
    commands: SyncSender<Input>,
    events: Receiver<ViewerRuntimeEvent>,
    clipboard_change: Arc<Mutex<Option<ClipboardInput>>>,
    clipboard_enable_gate: Arc<AtomicBool>,
    clipboard_actions: Arc<Mutex<VecDeque<ClipboardPortAction>>>,
    clipboard_metadata: Receiver<ClipboardMetadata>,
    telemetry: Arc<Mutex<Option<TelemetrySnapshot>>>,
    dropped_events: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
}
impl ViewerRuntime {
    /// Starts a production runtime restricted to Tailscale addresses.
    ///
    /// The decoder must be movable to the decode worker. Platform decoders with
    /// thread-affine resources should use spawn_with_decoder_factory.
    pub fn spawn<D: Decoder + Send + 'static, S: FrameSink + 'static>(
        config: ViewerRuntimeConfig,
        decoder: D,
        sink: S,
    ) -> Result<Self, ViewerRuntimeError> {
        Self::spawn_with_decoder_factory(config, move || Ok(decoder), sink)
    }

    /// Starts a Tailscale viewer and constructs its decoder inside the decode worker.
    ///
    /// This factory keeps thread-affine resources such as COM apartments and Media
    /// Foundation transforms on the worker that owns and uses them. The factory is
    /// sent to the worker, but the returned decoder does not need to be Send.
    pub fn spawn_with_decoder_factory<D, F, S>(
        config: ViewerRuntimeConfig,
        decoder_factory: F,
        sink: S,
    ) -> Result<Self, ViewerRuntimeError>
    where
        D: Decoder + 'static,
        F: FnOnce() -> Result<D, DecodeError> + Send + 'static,
        S: FrameSink + 'static,
    {
        Self::spawn_policy_with_decoder_factory(
            config,
            decoder_factory,
            sink,
            BindPolicy::Tailscale,
        )
    }

    fn spawn_policy_with_decoder_factory<D, F, S>(
        config: ViewerRuntimeConfig,
        decoder_factory: F,
        sink: S,
        policy: BindPolicy,
    ) -> Result<Self, ViewerRuntimeError>
    where
        D: Decoder + 'static,
        F: FnOnce() -> Result<D, DecodeError> + Send + 'static,
        S: FrameSink + 'static,
    {
        config.validate(policy)?;
        let (tx, rx) = mpsc::sync_channel(INPUT_CAPACITY);
        let event_capacity = VIEWER_RUNTIME_EVENT_CAPACITY;
        let (event_tx, event_rx) = mpsc::sync_channel(event_capacity);
        let clipboard_actions = Arc::new(Mutex::new(VecDeque::with_capacity(
            CLIPBOARD_ACTION_CAPACITY,
        )));
        let worker_clipboard_actions = Arc::clone(&clipboard_actions);
        let (clipboard_metadata_tx, clipboard_metadata_rx) =
            mpsc::sync_channel(CLIPBOARD_METADATA_CAPACITY);
        let telemetry = Arc::new(Mutex::new(None));
        let worker_telemetry = Arc::clone(&telemetry);
        let clipboard_change = Arc::new(Mutex::new(None));
        let worker_clipboard_change = Arc::clone(&clipboard_change);
        let clipboard_enable_gate = Arc::new(AtomicBool::new(false));
        let worker_clipboard_enable_gate = Arc::clone(&clipboard_enable_gate);
        let dropped = Arc::new(AtomicU64::new(0));
        let thread_dropped = Arc::clone(&dropped);
        let thread_tx = tx.clone();
        let worker = thread::Builder::new()
            .name("racc-viewer-runtime".to_owned())
            .spawn(move || {
                run::<D, F, S>(
                    config,
                    decoder_factory,
                    sink,
                    policy,
                    rx,
                    thread_tx,
                    event_tx,
                    thread_dropped,
                    worker_clipboard_change,
                    worker_clipboard_enable_gate,
                    worker_clipboard_actions,
                    clipboard_metadata_tx,
                    worker_telemetry,
                );
            })
            .map_err(|e| ViewerRuntimeError::ThreadSpawn(bounded(&e.to_string())))?;
        Ok(Self {
            commands: tx,
            events: event_rx,
            clipboard_change,
            clipboard_enable_gate,
            clipboard_actions,
            clipboard_metadata: clipboard_metadata_rx,
            telemetry,
            dropped_events: dropped,
            worker: Some(worker),
        })
    }
    /// Submits one command without blocking the caller.
    pub fn send(&self, command: ViewerCommand) -> Result<(), ViewerRuntimeError> {
        if self.worker.is_none() {
            return Err(ViewerRuntimeError::Closed);
        }
        let enabling_clipboard = matches!(command, ViewerCommand::SetClipboardEnabled(true));
        if matches!(command, ViewerCommand::SetClipboardEnabled(false)) {
            self.clipboard_enable_gate.store(false, Ordering::Release);
            if let Ok(mut pending) = self.clipboard_change.lock() {
                *pending = None;
            }
            clear_clipboard_actions(&self.clipboard_actions);
        }
        match self.commands.try_send(Input::Command(command)) {
            Ok(()) => {
                if enabling_clipboard {
                    self.clipboard_enable_gate.store(true, Ordering::Release);
                }
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(ViewerRuntimeError::QueueFull),
            Err(TrySendError::Disconnected(_)) => Err(ViewerRuntimeError::Closed),
        }
    }
    /// Drains at most max_events events without blocking.
    pub fn poll_events(
        &self,
        max_events: usize,
    ) -> Result<Vec<ViewerRuntimeEvent>, ViewerRuntimeError> {
        if self.worker.is_none() {
            return Err(ViewerRuntimeError::Closed);
        }
        let mut result = Vec::with_capacity(max_events.min(VIEWER_RUNTIME_EVENT_CAPACITY));
        for _ in 0..max_events.min(VIEWER_RUNTIME_EVENT_CAPACITY) {
            match self.events.try_recv() {
                Ok(event) => result.push(event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        Ok(result)
    }
    /// Replaces the pending local clipboard value with the newest change.
    ///
    /// The ingress is a single bounded slot. Values larger than the text limit
    /// are converted to a metadata-only rejection before they enter the slot.
    pub fn notify_local_clipboard_change(&self, bytes: &[u8]) -> Result<(), ViewerRuntimeError> {
        if self.worker.is_none() {
            return Err(ViewerRuntimeError::Closed);
        }
        let value = if bytes.len() > MAX_CLIPBOARD_BYTES {
            ClipboardInput::TooLarge(bytes.len())
        } else {
            ClipboardInput::Text(bytes.to_vec())
        };
        *self
            .clipboard_change
            .lock()
            .map_err(|_| ViewerRuntimeError::Closed)? = Some(value);
        Ok(())
    }

    /// Drains at most `max_actions` content-bearing clipboard actions for a platform adapter.
    ///
    /// This separate bounded path is never sent to the UI metadata event bus.
    pub fn poll_clipboard_actions(
        &self,
        max_actions: usize,
    ) -> Result<Vec<ClipboardPortAction>, ViewerRuntimeError> {
        if self.worker.is_none() {
            return Err(ViewerRuntimeError::Closed);
        }
        let mut actions = self
            .clipboard_actions
            .lock()
            .map_err(|_| ViewerRuntimeError::Closed)?;
        let count = max_actions.min(CLIPBOARD_ACTION_CAPACITY);
        let result = (0..count).filter_map(|_| actions.pop_front()).collect();
        Ok(result)
    }

    /// Drains at most `max_events` clipboard status metadata entries without text.
    pub fn poll_clipboard_metadata(
        &self,
        max_events: usize,
    ) -> Result<Vec<ClipboardMetadata>, ViewerRuntimeError> {
        if self.worker.is_none() {
            return Err(ViewerRuntimeError::Closed);
        }
        let mut result = Vec::with_capacity(max_events.min(CLIPBOARD_METADATA_CAPACITY));
        for _ in 0..max_events.min(CLIPBOARD_METADATA_CAPACITY) {
            match self.clipboard_metadata.try_recv() {
                Ok(metadata) => result.push(metadata),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        Ok(result)
    }

    /// Takes the latest immutable telemetry snapshot, published about four times per second.
    pub fn poll_telemetry(&self) -> Result<Option<TelemetrySnapshot>, ViewerRuntimeError> {
        if self.worker.is_none() {
            return Err(ViewerRuntimeError::Closed);
        }
        Ok(self
            .telemetry
            .lock()
            .map_err(|_| ViewerRuntimeError::Closed)?
            .take())
    }

    /// Returns the number of events dropped for a slow consumer.
    pub fn dropped_event_count(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }
    /// Requests shutdown and joins the runtime worker.
    pub fn close(&mut self) -> Result<(), ViewerRuntimeError> {
        if let Some(worker) = self.worker.take() {
            let _ = self.commands.send(Input::Command(ViewerCommand::Close));
            worker
                .join()
                .map_err(|_| ViewerRuntimeError::WorkerPanicked)?;
        }
        Ok(())
    }
}
impl Drop for ViewerRuntime {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

enum ClipboardInput {
    Text(Vec<u8>),
    TooLarge(usize),
}
enum Input {
    Command(ViewerCommand),
    Control {
        generation: u64,
        message: ControlMessage,
    },
    ControlClosed {
        generation: u64,
    },
    Video(VideoEvent),
}

struct ActiveControlReader {
    generation: u64,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<()>,
}
enum VideoEvent {
    NeedKeyframe(u16),
    ReceiverReset(u16),
    DecoderSelected(DecoderKind),
    DecodeFailed(String),
    FrameDecoded {
        epoch: u16,
        is_keyframe: bool,
        bytes: u64,
    },
    LossEstimate {
        packet: f64,
        frame: f64,
        loss_marker: u64,
    },
}

#[derive(Default)]
struct ViewerReportMetrics {
    epoch: Option<u16>,
    decode_durations_us: VecDeque<u64>,
    dropped_frames: u64,
}
impl ViewerReportMetrics {
    fn reset_epoch(&mut self, epoch: u16) {
        self.epoch = Some(epoch);
        self.decode_durations_us.clear();
        self.dropped_frames = 0;
    }

    fn record_decode(&mut self, epoch: u16, duration_us: u64) {
        if self.epoch != Some(epoch) {
            return;
        }
        if self.decode_durations_us.len() == VIEWER_DECODE_SAMPLE_CAPACITY {
            self.decode_durations_us.pop_front();
        }
        self.decode_durations_us.push_back(duration_us);
    }

    fn record_dropped(&mut self, epoch: u16, dropped: u64) {
        if self.epoch == Some(epoch) {
            self.dropped_frames = self.dropped_frames.saturating_add(dropped);
        }
    }

    fn take_report_values(&mut self, epoch: u16) -> (u32, u32) {
        if self.epoch != Some(epoch) {
            return (0, 0);
        }
        let decode_ms_p95 = percentile_95_ms(&self.decode_durations_us);
        self.decode_durations_us.clear();
        let dropped_frames = self
            .dropped_frames
            .min(u64::from(MAX_VIEWER_REPORT_DROPPED_FRAMES)) as u32;
        self.dropped_frames = 0;
        (decode_ms_p95, dropped_frames)
    }
}

#[derive(Debug)]
struct ViewerReportCadence {
    next_due_us: u64,
}
impl Default for ViewerReportCadence {
    fn default() -> Self {
        Self {
            next_due_us: VIEWER_REPORT_INTERVAL_US,
        }
    }
}
impl ViewerReportCadence {
    fn take_due(&mut self, now_us: u64, active: bool) -> bool {
        if !active || now_us < self.next_due_us {
            return false;
        }
        self.next_due_us = now_us.saturating_add(VIEWER_REPORT_INTERVAL_US);
        true
    }
}

fn percentile_95_ms(samples_us: &VecDeque<u64>) -> u32 {
    if samples_us.is_empty() {
        return 0;
    }
    let mut ordered: Vec<u64> = samples_us.iter().copied().collect();
    ordered.sort_unstable();
    let rank = ordered.len().saturating_mul(95).saturating_add(99) / 100;
    let duration_us = ordered[rank.saturating_sub(1)];
    (duration_us.saturating_add(999) / 1_000).min(u64::from(MAX_VIEWER_REPORT_DURATION_MS)) as u32
}

fn fraction_permille(fraction: f64) -> u16 {
    if fraction.is_finite() {
        (fraction.clamp(0.0, 1.0) * 1_000.0).round() as u16
    } else {
        0
    }
}

fn duration_ms(duration_us: Option<u64>) -> u32 {
    (duration_us.unwrap_or_default().saturating_add(999) / 1_000)
        .min(u64::from(MAX_VIEWER_REPORT_DURATION_MS)) as u32
}

fn viewer_report_from_snapshot(
    epoch: u16,
    snapshot: &TelemetrySnapshot,
    decode_ms_p95: u32,
    dropped_frames: u32,
) -> ViewerReport {
    ViewerReport {
        epoch,
        loss_permille: fraction_permille(snapshot.session.loss_fraction),
        frame_loss_permille: fraction_permille(snapshot.session.frame_loss_fraction),
        rtt_ms: duration_ms(snapshot.session.last_rtt_us),
        decode_ms_p95: decode_ms_p95.min(MAX_VIEWER_REPORT_DURATION_MS),
        dropped_frames: dropped_frames.min(MAX_VIEWER_REPORT_DROPPED_FRAMES),
    }
}
enum VideoCommand {
    Configure(StreamReset),
    ResetEpoch(u16),
    Stop,
}

fn update_video_activation(
    activation: &AtomicBool,
    connected: bool,
    visible: bool,
    video_ready: bool,
) {
    activation.store(connected && visible && video_ready, Ordering::Release);
}

fn apply_video_activation<D: Decoder, S: FrameSink>(
    activation: &AtomicBool,
    active: &mut bool,
    pipeline: &mut ViewerFramePipeline<D, S>,
) {
    let next = activation.load(Ordering::Acquire);
    if *active != next {
        *active = next;
        pipeline.set_active(next);
    }
}

// Worker resources stay explicit at this thread boundary.
#[allow(clippy::too_many_arguments)]
fn run<D, F, S>(
    config: ViewerRuntimeConfig,
    decoder_factory: F,
    sink: S,
    policy: BindPolicy,
    rx: Receiver<Input>,
    tx: SyncSender<Input>,
    events: SyncSender<ViewerRuntimeEvent>,
    dropped: Arc<AtomicU64>,
    clipboard_change: Arc<Mutex<Option<ClipboardInput>>>,
    clipboard_enable_gate: Arc<AtomicBool>,
    clipboard_actions: Arc<Mutex<VecDeque<ClipboardPortAction>>>,
    clipboard_metadata: SyncSender<ClipboardMetadata>,
    telemetry_slot: Arc<Mutex<Option<TelemetrySnapshot>>>,
) where
    D: Decoder + 'static,
    F: FnOnce() -> Result<D, DecodeError> + Send + 'static,
    S: FrameSink + 'static,
{
    emit(&events, &dropped, ViewerRuntimeEvent::Connecting);
    let receiver =
        match VideoReceiver::bind(config.local_video_bind, config.host_addr.ip(), 0, policy) {
            Ok(v) => v,
            Err(e) => {
                fail(&events, &dropped, &e.to_string());
                return;
            }
        };
    let video_port = match receiver.local_addr() {
        Ok(a) => a.port(),
        Err(e) => {
            fail(&events, &dropped, &e.to_string());
            return;
        }
    };
    let settings = ControlSettings {
        read_timeout: Some(CONTROL_POLL),
        ..ControlSettings::default()
    };
    let host_addr = config.host_addr;
    let (connect_request_tx, connect_request_rx) = mpsc::sync_channel(1);
    let (connect_result_tx, connect_result_rx) = mpsc::sync_channel(1);
    let connector = match thread::Builder::new()
        .name("racc-viewer-connect".to_owned())
        .spawn(move || {
            control_connector(
                host_addr,
                policy,
                settings,
                connect_request_rx,
                connect_result_tx,
            )
        }) {
        Ok(worker) => worker,
        Err(error) => {
            fail(&events, &dropped, &error.to_string());
            return;
        }
    };
    let hello = Hello {
        protocol_version: PROTOCOL_VERSION,
        device_name: config.device_name,
        os: config.os,
        app_version: config.app_version,
        video_udp_port: video_port,
        codecs: 1,
        max_height: config.max_height,
        features: config.features | 1,
    };
    let mut session = ViewerSession::new(hello);
    let mut writer: Option<ControlWriteHalf> = None;
    let mut active_control: Option<ActiveControlReader> = None;
    let mut control_generation = 0_u64;
    let mut handshake_deadline: Option<Instant> = None;
    let mut connect_pending = false;
    if !request_control_connect(&connect_request_tx, &mut connect_pending) {
        fail(
            &events,
            &dropped,
            "control connector stopped before initial attempt",
        );
        return;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let (video_tx, video_rx) = mpsc::sync_channel(VIDEO_COMMAND_CAPACITY);
    let video_activation = Arc::new(AtomicBool::new(false));
    let video_worker_activation = Arc::clone(&video_activation);
    let video_stop = Arc::clone(&stop);
    let video_input = tx.clone();
    let video_events = events.clone();
    let video_dropped = Arc::clone(&dropped);
    let report_metrics = Arc::new(Mutex::new(ViewerReportMetrics::default()));
    let video_report_metrics = Arc::clone(&report_metrics);
    let video_thread = match thread::Builder::new()
        .name("racc-viewer-decode".to_owned())
        .spawn(move || match decoder_factory() {
            Ok(decoder) => video_loop(
                receiver,
                decoder,
                sink,
                video_rx,
                video_input,
                video_events,
                video_dropped,
                video_stop,
                video_worker_activation,
                video_report_metrics,
            ),
            Err(error) => {
                let _ = video_input.send(Input::Video(VideoEvent::DecodeFailed(error.to_string())));
                video_stop.store(true, Ordering::Release);
            }
        }) {
        Ok(v) => v,
        Err(e) => {
            stop.store(true, Ordering::Release);
            drop(rx);
            drop(connect_request_tx);
            let _ = connector.join();
            fail(&events, &dropped, &e.to_string());
            return;
        }
    };
    let clock = Instant::now();
    let mut telemetry = TelemetryHub::new();
    telemetry.set_connection_state(0, ConnectionState::Handshaking);
    let mut clipboard_sync = ClipboardSync::new(VIEWER_CLIPBOARD_ORIGIN);
    let mut clipboard_enabled = false;
    let mut decoder_kind = DecoderKind::Unknown;
    let mut last_remote_wire_sequence: Option<(u32, u64)> = None;
    let mut visible = true;
    let mut topology: Option<Topology> = None;
    let mut connected = false;
    let mut video_ready = false;
    let mut session_paused = false;
    let mut resume_after_topology = false;
    let mut closing = false;
    let mut last_loss_marker = 0_u64;
    let mut next_ping_us = 0_u64;
    let mut next_telemetry_us = 0_u64;
    let mut report_cadence = ViewerReportCadence::default();
    let mut ping_nonce = 1_u64;
    while !closing && !stop.load(Ordering::Acquire) {
        match connect_result_rx.try_recv() {
            Ok(Ok(control)) => {
                connect_pending = false;
                control_generation = control_generation.wrapping_add(1).max(1);
                match start_control_reader(control, control_generation, tx.clone()) {
                    Ok((next_writer, reader)) => {
                        writer = Some(next_writer);
                        active_control = Some(reader);
                        handshake_deadline = Some(Instant::now() + CONTROL_HANDSHAKE_TIMEOUT);
                        telemetry.set_connection_state(now_us(clock), ConnectionState::Handshaking);
                        actions(session.on_connect(), &mut writer, &events, &dropped);
                    }
                    Err(_) => {
                        let now = now_us(clock);
                        connected = false;
                        clipboard_enable_gate.store(false, Ordering::Release);
                        topology = None;
                        video_ready = false;
                        update_video_activation(&video_activation, connected, visible, video_ready);
                        clipboard_sync.end_session();
                        last_remote_wire_sequence = None;
                        if let Ok(mut pending) = clipboard_change.lock() {
                            *pending = None;
                        }
                        clear_clipboard_actions(&clipboard_actions);
                        telemetry.set_connection_state(now, ConnectionState::Reconnecting);
                        let _ = telemetry.push_event(
                            now,
                            EventKind::ConnectionLost,
                            "control transport setup failed; reconnect scheduled",
                        );
                        actions(session.on_disconnect(now), &mut writer, &events, &dropped);
                    }
                }
            }
            Ok(Err(_)) => {
                connect_pending = false;
                let now = now_us(clock);
                connected = false;
                clipboard_enable_gate.store(false, Ordering::Release);
                topology = None;
                video_ready = false;
                update_video_activation(&video_activation, connected, visible, video_ready);
                clipboard_sync.end_session();
                last_remote_wire_sequence = None;
                if let Ok(mut pending) = clipboard_change.lock() {
                    *pending = None;
                }
                clear_clipboard_actions(&clipboard_actions);
                telemetry.set_connection_state(now, ConnectionState::Reconnecting);
                let _ = telemetry.push_event(
                    now,
                    EventKind::ConnectionLost,
                    "control connection failed; reconnect scheduled",
                );
                actions(session.on_disconnect(now), &mut writer, &events, &dropped);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                fail(&events, &dropped, "control connector stopped unexpectedly");
                closing = true;
            }
        }
        if !connected && handshake_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            if let Some(generation) = active_control.as_ref().map(|active| active.generation) {
                if tx.try_send(Input::ControlClosed { generation }).is_ok() {
                    handshake_deadline = None;
                }
            }
        }
        if let Some(change) = clipboard_change
            .lock()
            .ok()
            .and_then(|mut pending| pending.take())
        {
            match change {
                ClipboardInput::TooLarge(bytes) => emit_clipboard_metadata(
                    &clipboard_metadata,
                    &dropped,
                    ClipboardTransferDirection::LocalToRemote,
                    None,
                    u32::try_from(bytes).ok(),
                    ClipboardTransferStatus::RejectedTooLarge,
                ),
                ClipboardInput::Text(bytes)
                    if connected
                        && clipboard_enabled
                        && clipboard_enable_gate.load(Ordering::Acquire) =>
                {
                    let byte_len = u32::try_from(bytes.len()).ok();
                    match clipboard_sync.local_change(&bytes) {
                        LocalChangeResult::Queued => emit_clipboard_metadata(
                            &clipboard_metadata,
                            &dropped,
                            ClipboardTransferDirection::LocalToRemote,
                            None,
                            byte_len,
                            ClipboardTransferStatus::Queued,
                        ),
                        LocalChangeResult::Rejected(ClipboardRejection::TooLarge { .. }) => {
                            emit_clipboard_metadata(
                                &clipboard_metadata,
                                &dropped,
                                ClipboardTransferDirection::LocalToRemote,
                                None,
                                byte_len,
                                ClipboardTransferStatus::RejectedTooLarge,
                            )
                        }
                        LocalChangeResult::Rejected(ClipboardRejection::InvalidUtf8) => {
                            emit_clipboard_metadata(
                                &clipboard_metadata,
                                &dropped,
                                ClipboardTransferDirection::LocalToRemote,
                                None,
                                byte_len,
                                ClipboardTransferStatus::RejectedInvalidUtf8,
                            )
                        }
                        LocalChangeResult::EchoSuppressed | LocalChangeResult::Unchanged => {
                            emit_clipboard_metadata(
                                &clipboard_metadata,
                                &dropped,
                                ClipboardTransferDirection::LocalToRemote,
                                None,
                                byte_len,
                                ClipboardTransferStatus::Ignored,
                            )
                        }
                        LocalChangeResult::DirectionDisabled
                        | LocalChangeResult::SessionInactive => emit_clipboard_metadata(
                            &clipboard_metadata,
                            &dropped,
                            ClipboardTransferDirection::LocalToRemote,
                            None,
                            byte_len,
                            ClipboardTransferStatus::Disabled,
                        ),
                    }
                }
                ClipboardInput::Text(bytes) => emit_clipboard_metadata(
                    &clipboard_metadata,
                    &dropped,
                    ClipboardTransferDirection::LocalToRemote,
                    None,
                    u32::try_from(bytes.len()).ok(),
                    ClipboardTransferStatus::Disabled,
                ),
            }
        }
        match rx.recv_timeout(RUNTIME_POLL) {
            Ok(Input::Command(command)) => match command {
                ViewerCommand::SelectDisplay(id) => actions(
                    session.switch_display(id, now_us(clock)),
                    &mut writer,
                    &events,
                    &dropped,
                ),
                ViewerCommand::SetVisible(next) if next != visible => {
                    visible = next;
                    let a = if next {
                        session.on_resume()
                    } else {
                        session.on_pause()
                    };
                    let sent_pause = a.iter().any(|action| {
                        matches!(
                            action,
                            ViewerAction::SendControl(ControlMessage::PauseVideo(_))
                        )
                    });
                    let sent_resume = a.iter().any(|action| {
                        matches!(
                            action,
                            ViewerAction::SendControl(ControlMessage::ResumeVideo(_))
                        )
                    });
                    if sent_pause {
                        session_paused = true;
                        resume_after_topology = false;
                    } else if sent_resume {
                        session_paused = false;
                        resume_after_topology = false;
                    } else if next && !connected && session_paused {
                        resume_after_topology = true;
                    } else if !next {
                        resume_after_topology = false;
                    }
                    if !next || sent_resume {
                        video_ready = false;
                    }
                    update_video_activation(&video_activation, connected, visible, video_ready);
                    actions(a, &mut writer, &events, &dropped);
                    emit(
                        &events,
                        &dropped,
                        ViewerRuntimeEvent::VisibilityChanged(next),
                    );
                }
                ViewerCommand::SetVisible(_) => {}
                ViewerCommand::SetClipboardEnabled(enabled) => {
                    clipboard_enabled = enabled;
                    clipboard_enable_gate.store(enabled, Ordering::Release);
                    if clipboard_sync.is_session_active() {
                        clipboard_sync.set_enabled(
                            racc_clipboard::ClipboardDirection::LocalToRemote,
                            enabled,
                        );
                        clipboard_sync.set_enabled(
                            racc_clipboard::ClipboardDirection::RemoteToLocal,
                            enabled,
                        );
                    }
                    if !enabled {
                        if let Ok(mut pending) = clipboard_change.lock() {
                            *pending = None;
                        }
                        clear_clipboard_actions(&clipboard_actions);
                    }
                    if connected {
                        send_control(
                            &mut writer,
                            ControlMessage::ClipboardSyncControl(ClipboardSyncControl { enabled }),
                            &events,
                            &dropped,
                            &mut closing,
                        );
                    }
                    emit_clipboard_metadata(
                        &clipboard_metadata,
                        &dropped,
                        ClipboardTransferDirection::LocalToRemote,
                        None,
                        None,
                        if enabled && connected {
                            ClipboardTransferStatus::Enabled
                        } else {
                            ClipboardTransferStatus::Disabled
                        },
                    );
                }
                ViewerCommand::SetPath(path) => telemetry.set_path(now_us(clock), path),
                ViewerCommand::SetQuality(v) => send_control(
                    &mut writer,
                    ControlMessage::SetQuality(v),
                    &events,
                    &dropped,
                    &mut closing,
                ),
                ViewerCommand::SendInput(v) => send_control(
                    &mut writer,
                    ControlMessage::InputEvent(v),
                    &events,
                    &dropped,
                    &mut closing,
                ),
                ViewerCommand::SendReport(v) => send_control(
                    &mut writer,
                    ControlMessage::ViewerReport(v),
                    &events,
                    &dropped,
                    &mut closing,
                ),
                ViewerCommand::Close => {
                    actions(session.close(), &mut writer, &events, &dropped);
                    closing = true;
                }
            },
            Ok(Input::Control {
                generation,
                message,
            }) if active_control
                .as_ref()
                .is_some_and(|active| active.generation == generation) =>
            {
                match message {
                    ControlMessage::HelloAck(ack) => {
                        let mut a = session.on_hello_ack(ack.clone());
                        if ack.status == HelloStatus::Busy
                            && a.iter()
                                .any(|action| matches!(action, ViewerAction::DisconnectTransport))
                        {
                            stop_control_reader(&mut active_control, &mut writer);
                            connected = false;
                            clipboard_enable_gate.store(false, Ordering::Release);
                            topology = None;
                            video_ready = false;
                            update_video_activation(
                                &video_activation,
                                connected,
                                visible,
                                video_ready,
                            );
                            clipboard_sync.end_session();
                            last_remote_wire_sequence = None;
                            if let Ok(mut pending) = clipboard_change.lock() {
                                *pending = None;
                            }
                            clear_clipboard_actions(&clipboard_actions);
                            let now = now_us(clock);
                            telemetry.set_connection_state(now, ConnectionState::Reconnecting);
                            let _ = telemetry.push_event(
                                now,
                                EventKind::ConnectionLost,
                                "host is busy; retrying in five seconds",
                            );
                            emit(
                                &events,
                                &dropped,
                                ViewerRuntimeEvent::Session(SessionEvent::Reconnecting),
                            );
                            actions(a, &mut writer, &events, &dropped);
                        } else if a
                            .iter()
                            .any(|action| matches!(action, ViewerAction::DisconnectTransport))
                        {
                            fail(
                                &events,
                                &dropped,
                                &format!("host rejected Hello ({:?})", ack.status),
                            );
                            closing = true;
                        } else {
                            connected = true;
                            handshake_deadline = None;
                            if !visible {
                                let pause_already_requested = a.iter().any(|action| {
                                    matches!(
                                        action,
                                        ViewerAction::SendControl(ControlMessage::PauseVideo(_))
                                    )
                                });
                                if !pause_already_requested {
                                    a.extend(session.on_pause());
                                }
                                session_paused = true;
                                resume_after_topology = false;
                            } else if session_paused {
                                resume_after_topology = true;
                            }
                            last_remote_wire_sequence = None;
                            let directions = if clipboard_enabled {
                                ClipboardDirections::both()
                            } else {
                                ClipboardDirections::default()
                            };
                            if clipboard_sync
                                .start_session(HOST_CLIPBOARD_ORIGIN, directions)
                                .is_err()
                            {
                                emit_clipboard_metadata(
                                    &clipboard_metadata,
                                    &dropped,
                                    ClipboardTransferDirection::LocalToRemote,
                                    None,
                                    None,
                                    ClipboardTransferStatus::Disabled,
                                );
                            }
                            clipboard_enable_gate.store(clipboard_enabled, Ordering::Release);
                            send_control(
                                &mut writer,
                                ControlMessage::ClipboardSyncControl(ClipboardSyncControl {
                                    enabled: clipboard_enabled,
                                }),
                                &events,
                                &dropped,
                                &mut closing,
                            );
                            let now = now_us(clock);
                            telemetry.set_connection_state(now, ConnectionState::Connected);
                            let _ = telemetry.push_event(
                                now,
                                EventKind::ConnectionEstablished,
                                "viewer connected",
                            );
                            emit(&events, &dropped, ViewerRuntimeEvent::Connected(ack));
                            actions(a, &mut writer, &events, &dropped);
                            update_video_activation(
                                &video_activation,
                                connected,
                                visible,
                                video_ready,
                            );
                        }
                    }
                    ControlMessage::TopologyAnnounce(wire) => match Topology::from_proto(&wire) {
                        Ok(next)
                            if topology.as_ref().is_none_or(|old| {
                                is_revision_newer(next.revision(), old.revision())
                            }) =>
                        {
                            let mut topology_actions = session.on_topology(wire);
                            if visible && resume_after_topology && session_paused {
                                let resume = session.on_resume();
                                if resume.iter().any(|action| {
                                    matches!(
                                        action,
                                        ViewerAction::SendControl(ControlMessage::ResumeVideo(_))
                                    )
                                }) {
                                    session_paused = false;
                                    resume_after_topology = false;
                                    topology_actions.extend(resume);
                                }
                            }
                            actions(topology_actions, &mut writer, &events, &dropped);
                            topology = Some(next.clone());
                            let now = now_us(clock);
                            let _ = telemetry.push_event(
                                now,
                                EventKind::DisplaySwitch,
                                "topology updated",
                            );
                            emit(&events, &dropped, ViewerRuntimeEvent::TopologyChanged(next));
                        }
                        Ok(_) => {}
                        Err(e) => {
                            fail(&events, &dropped, &e.to_string());
                            closing = true;
                        }
                    },
                    ControlMessage::StreamReset(reset) => {
                        let a = session.on_stream_reset(reset);
                        let accepted = a.iter().any(|x| matches!(x, ViewerAction::ResetDecoder { epoch } if *epoch == reset.epoch));
                        if accepted {
                            let command = if reset.status == StreamStatus::Ok {
                                VideoCommand::Configure(reset)
                            } else {
                                VideoCommand::ResetEpoch(reset.epoch)
                            };
                            if video_tx.send(command).is_err() {
                                fail(&events, &dropped, "decode worker closed");
                                closing = true;
                            } else {
                                video_ready = reset.status == StreamStatus::Ok;
                                update_video_activation(
                                    &video_activation,
                                    connected,
                                    visible,
                                    video_ready,
                                );
                            }
                            let now = now_us(clock);
                            telemetry.set_stream(now, CodecKind::H264, decoder_kind, reset.epoch);
                            let _ = telemetry.push_event(
                                now,
                                EventKind::StreamReset,
                                "stream configuration changed",
                            );
                            emit(&events, &dropped, ViewerRuntimeEvent::StreamReset(reset));
                            actions(a, &mut writer, &events, &dropped);
                        }
                    }
                    ControlMessage::StatsReport(v) => {
                        telemetry.update_host_stats(now_us(clock), v);
                        emit(&events, &dropped, ViewerRuntimeEvent::HostStats(v));
                    }
                    ControlMessage::HostEventReport(report) => {
                        let (kind, detail) = match report.kind {
                            HostEventKind::CaptureLost => {
                                (EventKind::CaptureLost, "host capture paused")
                            }
                            HostEventKind::CaptureRecovered => {
                                (EventKind::CaptureRecovered, "host capture recovered")
                            }
                            HostEventKind::EncoderFallback => {
                                (EventKind::EncoderFallback, "host switched encoder backend")
                            }
                            HostEventKind::Paused => (EventKind::Paused, "host paused video"),
                            HostEventKind::Resumed => (EventKind::Resumed, "host resumed video"),
                        };
                        let _ = telemetry.push_event(now_us(clock), kind, detail);
                    }
                    ControlMessage::QualityAdjustment(adjustment) => {
                        let now = now_us(clock);
                        if adjustment.epoch == telemetry.snapshot(now).session.epoch {
                            let change = if adjustment.from_height == adjustment.to_height {
                                format!("{}p bitrate", adjustment.to_height)
                            } else {
                                format!("{}p -> {}p", adjustment.from_height, adjustment.to_height)
                            };
                            let detail = format!(
                                "{change} · {:.2} -> {:.2} Mbps · {}",
                                adjustment.from_bitrate_bps as f64 / 1_000_000.0,
                                adjustment.to_bitrate_bps as f64 / 1_000_000.0,
                                quality_adjustment_reason_label(adjustment.reason),
                            );
                            let _ = telemetry.push_event(now, EventKind::QualityAdjustment, detail);
                        }
                    }
                    ControlMessage::CursorShape(v) => {
                        emit(&events, &dropped, ViewerRuntimeEvent::CursorShape(v))
                    }
                    ControlMessage::Ping(v) => send_control(
                        &mut writer,
                        ControlMessage::Pong(Pong {
                            nonce: v.nonce,
                            echo_ts_us: v.sender_ts_us,
                        }),
                        &events,
                        &dropped,
                        &mut closing,
                    ),
                    ControlMessage::ClipboardUpdate(update) => {
                        if !connected
                            || !clipboard_enabled
                            || !clipboard_enable_gate.load(Ordering::Acquire)
                        {
                            emit_clipboard_metadata(
                                &clipboard_metadata,
                                &dropped,
                                ClipboardTransferDirection::RemoteToLocal,
                                Some(u64::from(update.seq)),
                                u32::try_from(update.text.len()).ok(),
                                ClipboardTransferStatus::Disabled,
                            );
                        } else if update.logical_clock.version != CLIPBOARD_LOGICAL_CLOCK_VERSION {
                            emit_clipboard_metadata(
                                &clipboard_metadata,
                                &dropped,
                                ClipboardTransferDirection::RemoteToLocal,
                                Some(u64::from(update.seq)),
                                u32::try_from(update.text.len()).ok(),
                                ClipboardTransferStatus::Ignored,
                            );
                        } else {
                            let byte_len = u32::try_from(update.text.len()).ok();
                            let sequence =
                                extend_wire_sequence(update.seq, &mut last_remote_wire_sequence);
                            match clipboard_sync.receive_remote(racc_clipboard::ClipboardUpdate {
                                origin: match update.origin {
                                    ClipboardOrigin::Viewer => OriginId(0),
                                    ClipboardOrigin::Host => OriginId(1),
                                },
                                seq: sequence,
                                logical_clock: update.logical_clock.counter,
                                bytes: update.text.into_bytes(),
                            }) {
                                RemoteChangeResult::Apply(applied) => {
                                    match String::from_utf8(applied.bytes) {
                                        Ok(text) => {
                                            let overflow = enqueue_clipboard_action(
                                                &clipboard_actions,
                                                ClipboardPortAction {
                                                    sequence: applied.seq,
                                                    text,
                                                },
                                            );
                                            emit_clipboard_metadata(
                                                &clipboard_metadata,
                                                &dropped,
                                                ClipboardTransferDirection::RemoteToLocal,
                                                Some(applied.seq),
                                                byte_len,
                                                if overflow {
                                                    ClipboardTransferStatus::ApplyQueueFull
                                                } else {
                                                    ClipboardTransferStatus::ReceivedPendingApply
                                                },
                                            );
                                        }
                                        Err(_) => emit_clipboard_metadata(
                                            &clipboard_metadata,
                                            &dropped,
                                            ClipboardTransferDirection::RemoteToLocal,
                                            Some(applied.seq),
                                            None,
                                            ClipboardTransferStatus::RejectedInvalidUtf8,
                                        ),
                                    }
                                }
                                RemoteChangeResult::Rejected(ClipboardRejection::TooLarge {
                                    ..
                                }) => {
                                    emit_clipboard_metadata(
                                        &clipboard_metadata,
                                        &dropped,
                                        ClipboardTransferDirection::RemoteToLocal,
                                        Some(u64::from(update.seq)),
                                        byte_len,
                                        ClipboardTransferStatus::RejectedTooLarge,
                                    );
                                }
                                RemoteChangeResult::Rejected(ClipboardRejection::InvalidUtf8) => {
                                    emit_clipboard_metadata(
                                        &clipboard_metadata,
                                        &dropped,
                                        ClipboardTransferDirection::RemoteToLocal,
                                        Some(u64::from(update.seq)),
                                        byte_len,
                                        ClipboardTransferStatus::RejectedInvalidUtf8,
                                    );
                                }
                                RemoteChangeResult::Ignored(_) => emit_clipboard_metadata(
                                    &clipboard_metadata,
                                    &dropped,
                                    ClipboardTransferDirection::RemoteToLocal,
                                    Some(u64::from(update.seq)),
                                    byte_len,
                                    ClipboardTransferStatus::Ignored,
                                ),
                            }
                        }
                    }
                    ControlMessage::Goodbye(_) => {
                        emit(&events, &dropped, ViewerRuntimeEvent::Disconnected);
                        closing = true;
                    }
                    ControlMessage::Pong(v) => {
                        let _ = telemetry.match_pong(v.nonce, v.echo_ts_us, now_us(clock));
                    }
                    ControlMessage::Hello(_)
                    | ControlMessage::SwitchMonitor(_)
                    | ControlMessage::SetQuality(_)
                    | ControlMessage::RequestKeyframe(_)
                    | ControlMessage::PauseVideo(_)
                    | ControlMessage::ResumeVideo(_)
                    | ControlMessage::InputEvent(_)
                    | ControlMessage::ViewerReport(_)
                    | ControlMessage::ClipboardSyncControl(_) => {
                        fail(&events, &dropped, "host sent a viewer-only control message");
                        closing = true;
                    }
                }
            }
            Ok(Input::Control { .. }) => {}
            Ok(Input::ControlClosed { generation })
                if active_control
                    .as_ref()
                    .is_some_and(|active| active.generation == generation) =>
            {
                stop_control_reader(&mut active_control, &mut writer);
                connected = false;
                clipboard_enable_gate.store(false, Ordering::Release);
                handshake_deadline = None;
                topology = None;
                video_ready = false;
                resume_after_topology = visible && session_paused;
                update_video_activation(&video_activation, connected, visible, video_ready);
                clipboard_sync.end_session();
                last_remote_wire_sequence = None;
                if let Ok(mut pending) = clipboard_change.lock() {
                    *pending = None;
                }
                clear_clipboard_actions(&clipboard_actions);
                let now = now_us(clock);
                telemetry.set_connection_state(now, ConnectionState::Reconnecting);
                let _ = telemetry.push_event(
                    now,
                    EventKind::ConnectionLost,
                    "control connection lost; reconnect scheduled",
                );
                actions(session.on_disconnect(now), &mut writer, &events, &dropped);
            }
            Ok(Input::ControlClosed { .. }) => {}
            Ok(Input::Video(VideoEvent::DecoderSelected(selected))) => {
                decoder_kind = selected;
                let now = now_us(clock);
                telemetry.set_decoder(now, selected);
                emit(
                    &events,
                    &dropped,
                    ViewerRuntimeEvent::DecoderSelected(selected),
                );
            }
            Ok(Input::Video(VideoEvent::FrameDecoded {
                epoch,
                is_keyframe,
                bytes,
            })) => {
                telemetry.record_frame(now_us(clock), false);
                telemetry.record_bytes(now_us(clock), bytes);
                telemetry.set_stream(now_us(clock), CodecKind::H264, decoder_kind, epoch);
                let (_, session_actions) = session.on_decoded_frame(epoch, is_keyframe);
                actions(session_actions, &mut writer, &events, &dropped);
            }
            Ok(Input::Video(VideoEvent::LossEstimate {
                packet,
                frame,
                loss_marker,
            })) => record_loss_estimate(
                &mut telemetry,
                now_us(clock),
                packet,
                frame,
                loss_marker,
                &mut last_loss_marker,
            ),
            Ok(Input::Video(
                VideoEvent::NeedKeyframe(epoch) | VideoEvent::ReceiverReset(epoch),
            )) => {
                if session.epoch() == Some(epoch) && connected && visible {
                    actions(
                        session.on_packet_loss(now_us(clock)),
                        &mut writer,
                        &events,
                        &dropped,
                    );
                }
            }
            Ok(Input::Video(VideoEvent::DecodeFailed(message))) => {
                let _ = telemetry.push_event(
                    now_us(clock),
                    EventKind::DecoderReset,
                    "decoder error; keyframe requested",
                );
                emit(
                    &events,
                    &dropped,
                    ViewerRuntimeEvent::Failed(bounded(&message)),
                );
                actions(
                    session.on_decoder_error(now_us(clock)),
                    &mut writer,
                    &events,
                    &dropped,
                );
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => closing = true,
        }

        let now = now_us(clock);
        let retry_actions = session.tick(now);
        let reconnect_due = retry_actions
            .iter()
            .any(|action| matches!(action, ViewerAction::ReconnectTransport));
        actions(retry_actions, &mut writer, &events, &dropped);
        if reconnect_due && !request_control_connect(&connect_request_tx, &mut connect_pending) {
            fail(
                &events,
                &dropped,
                "control connector stopped during reconnect",
            );
            closing = true;
        }
        if connected && now >= next_ping_us {
            let ping = Ping {
                nonce: ping_nonce,
                sender_ts_us: now,
            };
            ping_nonce = ping_nonce.wrapping_add(1);
            let _ = telemetry.record_ping(ping.nonce, ping.sender_ts_us, now);
            send_control(
                &mut writer,
                ControlMessage::Ping(ping),
                &events,
                &dropped,
                &mut closing,
            );
            next_ping_us =
                now.saturating_add(u64::try_from(PING_INTERVAL.as_micros()).unwrap_or(u64::MAX));
        }
        telemetry.expire_pings(now, PING_TIMEOUT_US);
        if connected {
            if let Some(update) = clipboard_sync.tick(now / 1_000) {
                let wire_sequence = update.seq as u32;
                let byte_len = u32::try_from(update.bytes.len()).ok();
                let text = String::from_utf8(update.bytes);
                let logical_clock = LogicalClock::new(update.logical_clock);
                match (text, logical_clock) {
                    (Ok(text), Ok(logical_clock)) => {
                        let sequence = update.seq;
                        let message =
                            ControlMessage::ClipboardUpdate(racc_proto::ClipboardUpdate {
                                seq: wire_sequence,
                                origin: ClipboardOrigin::Viewer,
                                logical_clock,
                                text,
                            });
                        let send_result = writer.as_mut().map(|writer| writer.send(&message));
                        match send_result {
                            Some(Ok(())) => emit_clipboard_metadata(
                                &clipboard_metadata,
                                &dropped,
                                ClipboardTransferDirection::LocalToRemote,
                                Some(sequence),
                                byte_len,
                                ClipboardTransferStatus::Sent,
                            ),
                            Some(Err(error)) => {
                                emit_clipboard_metadata(
                                    &clipboard_metadata,
                                    &dropped,
                                    ClipboardTransferDirection::LocalToRemote,
                                    Some(sequence),
                                    byte_len,
                                    ClipboardTransferStatus::Disabled,
                                );
                                fail(&events, &dropped, &error.to_string());
                                closing = true;
                            }
                            None => {}
                        }
                    }
                    _ => emit_clipboard_metadata(
                        &clipboard_metadata,
                        &dropped,
                        ClipboardTransferDirection::LocalToRemote,
                        Some(update.seq),
                        byte_len,
                        ClipboardTransferStatus::RejectedInvalidUtf8,
                    ),
                }
            }
        }
        if now >= next_telemetry_us {
            let snapshot = telemetry.snapshot(now);
            publish_telemetry(&telemetry_slot, snapshot.clone());
            if report_cadence.take_due(now, connected && session.epoch().is_some()) {
                if let Some(epoch) = session.epoch() {
                    let (decode_ms_p95, dropped_frames) = report_metrics
                        .lock()
                        .map(|mut metrics| metrics.take_report_values(epoch))
                        .unwrap_or_default();
                    let report = viewer_report_from_snapshot(
                        epoch,
                        &snapshot,
                        decode_ms_p95,
                        dropped_frames,
                    );
                    send_control(
                        &mut writer,
                        ControlMessage::ViewerReport(report),
                        &events,
                        &dropped,
                        &mut closing,
                    );
                }
            }
            next_telemetry_us = now
                .saturating_add(u64::try_from(TELEMETRY_INTERVAL.as_micros()).unwrap_or(u64::MAX));
        }
    }
    clipboard_enable_gate.store(false, Ordering::Release);
    clipboard_sync.end_session();
    if let Ok(mut pending) = clipboard_change.lock() {
        *pending = None;
    }
    clear_clipboard_actions(&clipboard_actions);
    telemetry.set_connection_state(now_us(clock), ConnectionState::Disconnected);
    if connected {
        let _ = telemetry.push_event(
            now_us(clock),
            EventKind::ConnectionLost,
            "viewer disconnected",
        );
    }
    publish_telemetry(&telemetry_slot, telemetry.snapshot(now_us(clock)));
    stop_control_reader(&mut active_control, &mut writer);
    update_video_activation(&video_activation, false, false, false);
    stop.store(true, Ordering::Release);
    let _ = video_tx.try_send(VideoCommand::Stop);
    drop(rx);
    let _ = video_thread.join();
    drop(connect_request_tx);
    let _ = connector.join();
}

fn request_control_connect(requests: &SyncSender<()>, pending: &mut bool) -> bool {
    if *pending {
        return true;
    }
    match requests.try_send(()) {
        Ok(()) | Err(TrySendError::Full(())) => {
            *pending = true;
            true
        }
        Err(TrySendError::Disconnected(())) => false,
    }
}

fn control_connector(
    address: SocketAddr,
    policy: BindPolicy,
    settings: ControlSettings,
    requests: Receiver<()>,
    results: SyncSender<Result<ControlConn, String>>,
) {
    while requests.recv().is_ok() {
        let result =
            connect_control(address, policy, settings).map_err(|error| bounded(&error.to_string()));
        if results.send(result).is_err() {
            return;
        }
    }
}

fn start_control_reader(
    control: ControlConn,
    generation: u64,
    tx: SyncSender<Input>,
) -> Result<(ControlWriteHalf, ActiveControlReader), String> {
    let (mut reader, writer) = control
        .split()
        .map_err(|error| bounded(&error.to_string()))?;
    let stop = Arc::new(AtomicBool::new(false));
    let reader_stop = Arc::clone(&stop);
    let worker = thread::Builder::new()
        .name("racc-viewer-control".to_owned())
        .spawn(move || control_reader(&mut reader, tx, reader_stop, generation))
        .map_err(|error| bounded(&error.to_string()))?;
    Ok((
        writer,
        ActiveControlReader {
            generation,
            stop,
            worker,
        },
    ))
}

fn stop_control_reader(
    active: &mut Option<ActiveControlReader>,
    writer: &mut Option<ControlWriteHalf>,
) {
    *writer = None;
    if let Some(active) = active.take() {
        active.stop.store(true, Ordering::Release);
        let _ = active.worker.join();
    }
}

fn control_reader(
    reader: &mut ControlReadHalf,
    tx: SyncSender<Input>,
    stop: Arc<AtomicBool>,
    generation: u64,
) {
    while !stop.load(Ordering::Acquire) {
        match reader.recv() {
            Ok(message) => {
                if !send_reader_input(
                    &tx,
                    Input::Control {
                        generation,
                        message,
                    },
                    &stop,
                ) {
                    return;
                }
            }
            Err(racc_net::ControlError::Timeout) => {}
            Err(_) => {
                let _ = send_reader_input(&tx, Input::ControlClosed { generation }, &stop);
                return;
            }
        }
    }
}

fn telemetry_decoder_kind(kind: DecodeBackendKind) -> DecoderKind {
    match kind {
        DecodeBackendKind::Fake => DecoderKind::Unknown,
        DecodeBackendKind::MediaFoundation => DecoderKind::MediaFoundation,
        DecodeBackendKind::WindowsMediaFoundationHardwareMft => {
            DecoderKind::MediaFoundationHardwareMft
        }
        DecodeBackendKind::WindowsMediaFoundationSynchronousMft => {
            DecoderKind::MediaFoundationSynchronousMft
        }
        DecodeBackendKind::VideoToolbox => DecoderKind::VideoToolbox,
    }
}

fn send_reader_input(tx: &SyncSender<Input>, mut input: Input, stop: &AtomicBool) -> bool {
    loop {
        match tx.try_send(input) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) if !stop.load(Ordering::Acquire) => {
                input = returned;
                thread::sleep(Duration::from_millis(2));
            }
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

// Worker resources stay explicit at this thread boundary.
#[allow(clippy::too_many_arguments)]
fn video_loop<D: Decoder + 'static, S: FrameSink + 'static>(
    mut receiver: VideoReceiver,
    decoder: D,
    sink: S,
    commands: Receiver<VideoCommand>,
    tx: SyncSender<Input>,
    events: SyncSender<ViewerRuntimeEvent>,
    dropped: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    activation: Arc<AtomicBool>,
    report_metrics: Arc<Mutex<ViewerReportMetrics>>,
) {
    let mut pipeline = ViewerFramePipeline::new(decoder, sink);
    let mut current_epoch: u16 = 0;
    let mut active = false;
    let mut next_stats = Instant::now() + TELEMETRY_INTERVAL;
    let mut last_dropped_gap = 0_u64;
    while !stop.load(Ordering::Acquire) {
        loop {
            match commands.try_recv() {
                Ok(VideoCommand::Configure(reset)) => {
                    if let Err(e) = reset_receiver_epoch(&receiver, reset.epoch) {
                        let _ = tx.send(Input::Video(VideoEvent::DecodeFailed(e.to_string())));
                        continue;
                    }
                    current_epoch = reset.epoch;
                    if let Ok(mut metrics) = report_metrics.lock() {
                        metrics.reset_epoch(current_epoch);
                    }
                    match pipeline.apply_stream_reset(reset) {
                        Ok(()) => {
                            let selected = telemetry_decoder_kind(pipeline.decoder_kind());
                            let _ = tx.send(Input::Video(VideoEvent::DecoderSelected(selected)));
                            let _ =
                                tx.try_send(Input::Video(VideoEvent::ReceiverReset(current_epoch)));
                        }
                        Err(error) => {
                            let _ =
                                tx.send(Input::Video(VideoEvent::DecodeFailed(error.to_string())));
                        }
                    }
                }
                Ok(VideoCommand::ResetEpoch(epoch)) => {
                    if let Err(e) = reset_receiver_epoch(&receiver, epoch) {
                        let _ = tx.send(Input::Video(VideoEvent::DecodeFailed(e.to_string())));
                    } else {
                        current_epoch = epoch;
                        if let Ok(mut metrics) = report_metrics.lock() {
                            metrics.reset_epoch(current_epoch);
                        }
                    }
                }
                Ok(VideoCommand::Stop) => {
                    stop.store(true, Ordering::Release);
                    break;
                }
                Err(TryRecvError::Empty) => break,

                Err(TryRecvError::Disconnected) => {
                    stop.store(true, Ordering::Release);
                    break;
                }
            }
        }
        if stop.load(Ordering::Acquire) {
            break;
        }
        apply_video_activation(&activation, &mut active, &mut pipeline);
        match receiver.recv_event(VIDEO_POLL) {
            Ok(VideoTransportEvent::FrameReady(frame))
                if active && activation.load(Ordering::Acquire) =>
            {
                let epoch = current_epoch;
                let frame_id = frame.frame_id;
                let is_keyframe = frame.keyframe;
                let encoded_bytes = u64::try_from(frame.bytes.len()).unwrap_or(u64::MAX);
                let unit = match EncodedAccessUnit::new(
                    epoch,
                    frame_id,
                    frame.capture_ts_us,
                    frame.keyframe,
                    frame.config,
                    frame.bytes,
                ) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = tx.send(Input::Video(VideoEvent::DecodeFailed(e.to_string())));
                        continue;
                    }
                };
                match pipeline.enqueue_access_unit(unit) {
                    Ok(ViewerQueueOutcome::Queued) => {}
                    Ok(ViewerQueueOutcome::ReplacedBacklog { dropped }) => {
                        if let Ok(mut metrics) = report_metrics.lock() {
                            metrics
                                .record_dropped(epoch, u64::try_from(dropped).unwrap_or(u64::MAX));
                        }
                    }
                    Ok(ViewerQueueOutcome::DroppedAwaitingKeyframe { dropped }) => {
                        if let Ok(mut metrics) = report_metrics.lock() {
                            metrics
                                .record_dropped(epoch, u64::try_from(dropped).unwrap_or(u64::MAX));
                        }
                        let _ = tx.send(Input::Video(VideoEvent::NeedKeyframe(epoch)));
                        continue;
                    }
                    Ok(
                        ViewerQueueOutcome::DroppedInactive
                        | ViewerQueueOutcome::DroppedWrongEpoch { .. },
                    ) => continue,
                    Err(e) => {
                        let _ = tx.send(Input::Video(VideoEvent::DecodeFailed(e.to_string())));
                        continue;
                    }
                }
                if !activation.load(Ordering::Acquire) {
                    apply_video_activation(&activation, &mut active, &mut pipeline);
                    continue;
                }
                match pipeline.decode_pending_timed() {
                    Ok(stats) => {
                        if let Ok(mut metrics) = report_metrics.lock() {
                            for duration_us in
                                &stats.decode_durations_us[..stats.decode_sample_count]
                            {
                                metrics.record_decode(epoch, *duration_us);
                            }
                        }
                        if stats.published_frames > 0 {
                            emit(
                                &events,
                                &dropped,
                                ViewerRuntimeEvent::FrameAvailable { epoch, frame_id },
                            );
                            let _ = tx.try_send(Input::Video(VideoEvent::FrameDecoded {
                                epoch,
                                is_keyframe,
                                bytes: encoded_bytes,
                            }));
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Input::Video(VideoEvent::DecodeFailed(e.to_string())));
                    }
                }
            }
            Ok(VideoTransportEvent::FrameReady(_)) => {
                apply_video_activation(&activation, &mut active, &mut pipeline);
            }
            Ok(VideoTransportEvent::NeedKeyframe(epoch)) => {
                let _ = tx.send(Input::Video(VideoEvent::NeedKeyframe(epoch)));
            }
            Ok(VideoTransportEvent::Cursor(v)) => {
                emit(&events, &dropped, ViewerRuntimeEvent::CursorPosition(v))
            }
            Err(NetError::Timeout) => {}
            Err(e) => {
                let _ = tx.send(Input::Video(VideoEvent::DecodeFailed(e.to_string())));
                stop.store(true, Ordering::Release);
            }
        }
        if Instant::now() >= next_stats {
            if let Ok(stats) = receiver.reassembly_stats() {
                let dropped_gaps = if stats.counters.dropped_gap >= last_dropped_gap {
                    stats.counters.dropped_gap - last_dropped_gap
                } else {
                    stats.counters.dropped_gap
                };
                last_dropped_gap = stats.counters.dropped_gap;
                if dropped_gaps > 0 {
                    if let Ok(mut metrics) = report_metrics.lock() {
                        metrics.record_dropped(current_epoch, dropped_gaps);
                    }
                }
                let _ = tx.try_send(Input::Video(VideoEvent::LossEstimate {
                    packet: stats.loss.packet_loss_fraction,
                    frame: stats.loss.whole_frame_loss_fraction,
                    loss_marker: stats
                        .loss
                        .missing_fragments
                        .saturating_add(stats.loss.whole_frames_lost),
                }));
            }
            next_stats = Instant::now() + TELEMETRY_INTERVAL;
        }
    }
    let _ = receiver.close();
}

fn reset_receiver_epoch(receiver: &VideoReceiver, epoch: u16) -> Result<(), NetError> {
    for _ in 0..50 {
        match receiver.reset_epoch(epoch) {
            Ok(()) => return Ok(()),
            Err(NetError::QueueClosed) => thread::sleep(Duration::from_millis(1)),
            Err(e) => return Err(e),
        }
    }
    Err(NetError::QueueClosed)
}
fn record_loss_estimate(
    telemetry: &mut TelemetryHub,
    now_us: u64,
    packet_loss: f64,
    frame_loss: f64,
    loss_marker: u64,
    last_loss_marker: &mut u64,
) {
    telemetry.record_loss(now_us, packet_loss, frame_loss);
    if loss_marker > *last_loss_marker {
        *last_loss_marker = loss_marker;
        let _ = telemetry.push_event(
            now_us,
            EventKind::PacketLossEvent,
            "video packet or frame loss observed",
        );
    }
}

fn enqueue_clipboard_action(
    queue: &Arc<Mutex<VecDeque<ClipboardPortAction>>>,
    action: ClipboardPortAction,
) -> bool {
    let Ok(mut queue) = queue.lock() else {
        return true;
    };
    let overflow = queue.len() == CLIPBOARD_ACTION_CAPACITY;
    if overflow {
        queue.pop_front();
    }
    queue.push_back(action);
    overflow
}

fn clear_clipboard_actions(queue: &Arc<Mutex<VecDeque<ClipboardPortAction>>>) {
    if let Ok(mut queue) = queue.lock() {
        queue.clear();
    }
}

fn emit_clipboard_metadata(
    metadata: &SyncSender<ClipboardMetadata>,
    dropped: &AtomicU64,
    direction: ClipboardTransferDirection,
    sequence: Option<u64>,
    byte_len: Option<u32>,
    status: ClipboardTransferStatus,
) {
    match metadata.try_send(ClipboardMetadata {
        direction,
        sequence,
        byte_len,
        status,
    }) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
        Err(TrySendError::Disconnected(_)) => {}
    }
}

fn publish_telemetry(slot: &Arc<Mutex<Option<TelemetrySnapshot>>>, snapshot: TelemetrySnapshot) {
    if let Ok(mut latest) = slot.lock() {
        *latest = Some(snapshot);
    }
}

fn extend_wire_sequence(sequence: u32, previous: &mut Option<(u32, u64)>) -> u64 {
    const HALF_RANGE: u32 = 1 << 31;
    match *previous {
        None => {
            let extended = u64::from(sequence);
            *previous = Some((sequence, extended));
            extended
        }
        Some((last_wire, last_extended)) => {
            let distance = sequence.wrapping_sub(last_wire);
            if distance == 0 || distance >= HALF_RANGE {
                last_extended
            } else {
                let extended = last_extended.saturating_add(u64::from(distance));
                *previous = Some((sequence, extended));
                extended
            }
        }
    }
}

fn actions(
    list: Vec<ViewerAction>,
    writer: &mut Option<ControlWriteHalf>,
    events: &SyncSender<ViewerRuntimeEvent>,
    dropped: &AtomicU64,
) {
    for action in list {
        match action {
            ViewerAction::SendControl(v) => {
                if let Some(writer) = writer.as_mut() {
                    if let Err(e) = writer.send(&v) {
                        fail(events, dropped, &e.to_string());
                    }
                }
            }
            ViewerAction::ForwardInput(input) => {
                if let Some(writer) = writer.as_mut() {
                    if let Err(e) = writer.send(&ControlMessage::InputEvent(input)) {
                        fail(events, dropped, &e.to_string());
                    }
                }
            }
            ViewerAction::Event(v) => emit(events, dropped, ViewerRuntimeEvent::Session(v)),
            ViewerAction::ResetDecoder { .. } => {}
            ViewerAction::ReconnectTransport => emit(
                events,
                dropped,
                ViewerRuntimeEvent::Session(SessionEvent::Reconnecting),
            ),
            ViewerAction::DisconnectTransport => {}
        }
    }
}
fn send_control(
    writer: &mut Option<ControlWriteHalf>,
    message: ControlMessage,
    events: &SyncSender<ViewerRuntimeEvent>,
    dropped: &AtomicU64,
    closing: &mut bool,
) {
    if let Some(writer) = writer.as_mut() {
        if let Err(e) = writer.send(&message) {
            fail(events, dropped, &e.to_string());
            *closing = true;
        }
    }
}
fn quality_adjustment_reason_label(reason: racc_proto::QualityAdjustmentReason) -> &'static str {
    use racc_proto::QualityAdjustmentReason as Reason;
    match reason {
        Reason::Loss => "packet loss",
        Reason::RttInflation => "RTT inflation",
        Reason::QueueOverflow => "sender queue pressure",
        Reason::Stable => "stable connection",
        Reason::Preference => "quality preference",
        Reason::BitrateTrim => "bitrate trim",
    }
}

fn now_us(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}
fn emit(events: &SyncSender<ViewerRuntimeEvent>, dropped: &AtomicU64, event: ViewerRuntimeEvent) {
    match events.try_send(event) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
        Err(TrySendError::Disconnected(_)) => {}
    }
}
fn fail(events: &SyncSender<ViewerRuntimeEvent>, dropped: &AtomicU64, message: &str) {
    emit(
        events,
        dropped,
        ViewerRuntimeEvent::Failed(bounded(message)),
    );
}
fn bounded(text: &str) -> String {
    let mut end = text.len().min(MAX_ERROR_BYTES);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_decode::{EncodedAccessUnit, FakeDecoder};
    use racc_net::{ControlConn, ControlListener, VideoSender};
    use racc_proto::{DisplayInfo, Pong, StreamCodec, TopologyAnnounce};
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Mutex;

    #[derive(Clone)]
    struct Sink(Arc<Mutex<Option<Arc<crate::VideoFrame>>>>);
    impl FrameSink for Sink {
        fn publish_frame(&self, frame: Arc<crate::VideoFrame>) -> Result<(), crate::CoreError> {
            *self.0.lock().map_err(|_| crate::CoreError::Closed)? = Some(frame);
            Ok(())
        }
    }
    #[test]
    fn hide_activation_bypasses_a_full_video_command_queue() {
        let (commands, _receiver) = mpsc::sync_channel(VIDEO_COMMAND_CAPACITY);
        for _ in 0..VIDEO_COMMAND_CAPACITY {
            assert!(commands.try_send(VideoCommand::ResetEpoch(1)).is_ok());
        }
        assert!(matches!(
            commands.try_send(VideoCommand::Stop),
            Err(TrySendError::Full(VideoCommand::Stop))
        ));

        let activation = AtomicBool::new(false);
        update_video_activation(&activation, true, true, true);
        let latest = Arc::new(Mutex::new(None));
        let sink = Sink(Arc::clone(&latest));
        let mut pipeline = ViewerFramePipeline::new(FakeDecoder::new(), sink);
        pipeline
            .apply_stream_reset(reset())
            .expect("configure fake decoder");
        let idr = EncodedAccessUnit::new(
            1,
            1,
            33_333,
            true,
            true,
            vec![
                0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0, 0, 1, 0x68, 0xce, 0, 0, 1, 0x65, 0x88,
            ],
        )
        .expect("well-formed fake IDR");
        assert_eq!(
            pipeline.enqueue_access_unit(idr),
            Ok(ViewerQueueOutcome::Queued)
        );
        let mut active = false;
        apply_video_activation(&activation, &mut active, &mut pipeline);
        assert_eq!(pipeline.decode_pending(), Ok(1));
        assert_eq!(
            latest
                .lock()
                .ok()
                .and_then(|frame| frame.as_ref().map(|frame| frame.frame_id)),
            Some(1)
        );

        update_video_activation(&activation, true, false, true);
        apply_video_activation(&activation, &mut active, &mut pipeline);
        let predictive =
            EncodedAccessUnit::new(1, 2, 66_666, false, false, vec![0, 0, 0, 1, 0x41, 0x88])
                .expect("well-formed fake predictive frame");
        assert_eq!(
            pipeline.enqueue_access_unit(predictive),
            Ok(ViewerQueueOutcome::DroppedInactive)
        );
        assert_eq!(pipeline.decode_pending(), Ok(0));
        assert_eq!(
            latest
                .lock()
                .ok()
                .and_then(|frame| frame.as_ref().map(|frame| frame.frame_id)),
            Some(1),
            "the held frame remains available while hidden"
        );
    }

    fn topology() -> TopologyAnnounce {
        TopologyAnnounce {
            topology_rev: 1,
            active_display_id: 1,
            displays: vec![DisplayInfo {
                display_id: 1,
                name: "Primary".to_owned(),
                x: 0,
                y: 0,
                width_px: 1280,
                height_px: 720,
                scale_milli: 1000,
                refresh_mhz: 60_000,
                flags: 7,
            }],
        }
    }
    fn reset() -> StreamReset {
        StreamReset {
            req_id: 0,
            epoch: 1,
            codec: StreamCodec::H264,
            width: 640,
            height: 480,
            fps: 30,
            topology_rev: 1,
            display_id: 1,
            status: StreamStatus::Ok,
        }
    }
    fn next_host_control(
        connection: &mut ControlConn,
        deadline: Instant,
    ) -> Result<ControlMessage, String> {
        loop {
            if Instant::now() >= deadline {
                return Err("timed out waiting for viewer control message".to_owned());
            }
            match connection.recv() {
                Ok(ControlMessage::Ping(ping)) => connection
                    .send(&ControlMessage::Pong(Pong {
                        nonce: ping.nonce,
                        echo_ts_us: ping.sender_ts_us,
                    }))
                    .map_err(|error| error.to_string())?,
                Ok(message) => return Ok(message),
                Err(racc_net::ControlError::Timeout) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    }
    fn expect_host_control(
        connection: &mut ControlConn,
        deadline: Instant,
        predicate: impl Fn(&ControlMessage) -> bool,
    ) -> Result<ControlMessage, String> {
        loop {
            let message = next_host_control(connection, deadline)?;
            if predicate(&message) {
                return Ok(message);
            }
        }
    }
    #[test]
    fn decoder_backend_selection_maps_to_precise_local_telemetry() {
        assert_eq!(
            telemetry_decoder_kind(DecodeBackendKind::Fake),
            DecoderKind::Unknown
        );
        assert_eq!(
            telemetry_decoder_kind(DecodeBackendKind::MediaFoundation),
            DecoderKind::MediaFoundation
        );
        assert_eq!(
            telemetry_decoder_kind(DecodeBackendKind::WindowsMediaFoundationHardwareMft),
            DecoderKind::MediaFoundationHardwareMft
        );
        assert_eq!(
            telemetry_decoder_kind(DecodeBackendKind::WindowsMediaFoundationSynchronousMft),
            DecoderKind::MediaFoundationSynchronousMft
        );
        assert_eq!(
            telemetry_decoder_kind(DecodeBackendKind::VideoToolbox),
            DecoderKind::VideoToolbox
        );
    }

    #[test]
    fn production_bind_policy_rejects_loopback() {
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let config = ViewerRuntimeConfig::new(
            SocketAddr::new(ip, 44000),
            SocketAddr::new(ip, 0),
            "viewer",
            OsType::Windows,
        );
        assert_eq!(
            config.validate(BindPolicy::Tailscale),
            Err(ViewerRuntimeError::InvalidConfig(
                "host address is outside Tailscale"
            ))
        );
    }
    #[test]
    fn control_disconnect_reconnects_and_restores_selected_stream_after_pause() {
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let listener = ControlListener::bind(
            SocketAddr::new(ip, 0),
            BindPolicy::TestOnlyLoopback,
            ControlSettings {
                read_timeout: Some(Duration::from_millis(100)),
                ..ControlSettings::default()
            },
        )
        .expect("test listener");
        let host_addr = listener.local_addr().expect("host address");
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let (first_paused_tx, first_paused_rx) = mpsc::channel();
        let (reconnected_paused_tx, reconnected_paused_rx) = mpsc::channel();
        let host = thread::spawn(move || -> Result<(), String> {
            let (mut first, viewer_addr) = listener.accept().map_err(|error| error.to_string())?;
            let first_hello =
                match next_host_control(&mut first, Instant::now() + Duration::from_secs(5))? {
                    ControlMessage::Hello(hello) => hello,
                    _ => return Err("first connection did not start with Hello".to_owned()),
                };
            first
                .send(&ControlMessage::HelloAck(HelloAck {
                    protocol_version: PROTOCOL_VERSION,
                    status: HelloStatus::Ok,
                    device_name: "reconnect host".to_owned(),
                    os: OsType::Windows,
                    app_version: "test".to_owned(),
                    codecs: 1,
                    max_height: 1080,
                    features: 0,
                    host_cpu_cores: 8,
                }))
                .map_err(|error| error.to_string())?;
            first
                .send(&ControlMessage::TopologyAnnounce(topology()))
                .map_err(|error| error.to_string())?;
            let switch = match expect_host_control(
                &mut first,
                Instant::now() + Duration::from_secs(5),
                |message| matches!(message, ControlMessage::SwitchMonitor(_)),
            )? {
                ControlMessage::SwitchMonitor(switch) => switch,
                _ => return Err("viewer did not select the initial display".to_owned()),
            };
            let mut first_reset = reset();
            first_reset.req_id = switch.req_id;
            first
                .send(&ControlMessage::StreamReset(first_reset))
                .map_err(|error| error.to_string())?;
            expect_host_control(
                &mut first,
                Instant::now() + Duration::from_secs(5),
                |message| matches!(message, ControlMessage::RequestKeyframe(request) if request.epoch == 1),
            )?;
            let sender = VideoSender::bind(
                SocketAddr::new(ip, 0),
                SocketAddr::new(viewer_addr.ip(), first_hello.video_udp_port),
                BindPolicy::TestOnlyLoopback,
                33_333,
            )
            .map_err(|error| error.to_string())?;
            for frame_id in 1..=5 {
                sender
                    .send_frame(racc_net::SenderFrame {
                        epoch: 1,
                        frame_id,
                        keyframe: true,
                        config: true,
                        capture_ts_us: 10 + frame_id,
                        bytes: vec![
                            0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0, 0, 1, 0x68, 0xce, 0, 0, 1, 0x65,
                            0x88,
                        ],
                    })
                    .map_err(|error| error.to_string())?;
                thread::sleep(Duration::from_millis(10));
            }
            expect_host_control(
                &mut first,
                Instant::now() + Duration::from_secs(5),
                |message| matches!(message, ControlMessage::PauseVideo(_)),
            )?;
            let _ = first_paused_tx.send(());
            release_first_rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|error| error.to_string())?;
            drop(first);

            let (mut second, _) = listener.accept().map_err(|error| error.to_string())?;
            let second_hello =
                match next_host_control(&mut second, Instant::now() + Duration::from_secs(5))? {
                    ControlMessage::Hello(hello) => hello,
                    _ => return Err("reconnected control did not start with Hello".to_owned()),
                };
            if second_hello.video_udp_port != first_hello.video_udp_port {
                return Err("viewer changed its UDP port during control reconnect".to_owned());
            }
            second
                .send(&ControlMessage::HelloAck(HelloAck {
                    protocol_version: PROTOCOL_VERSION,
                    status: HelloStatus::Ok,
                    device_name: "reconnect host".to_owned(),
                    os: OsType::Windows,
                    app_version: "test".to_owned(),
                    codecs: 1,
                    max_height: 1080,
                    features: 0,
                    host_cpu_cores: 8,
                }))
                .map_err(|error| error.to_string())?;
            second
                .send(&ControlMessage::TopologyAnnounce(topology()))
                .map_err(|error| error.to_string())?;
            expect_host_control(
                &mut second,
                Instant::now() + Duration::from_secs(5),
                |message| matches!(message, ControlMessage::PauseVideo(_)),
            )?;
            let _ = reconnected_paused_tx.send(());
            expect_host_control(
                &mut second,
                Instant::now() + Duration::from_secs(5),
                |message| matches!(message, ControlMessage::ResumeVideo(_)),
            )?;
            let mut second_reset = reset();
            second_reset.epoch = 2;
            second
                .send(&ControlMessage::StreamReset(second_reset))
                .map_err(|error| error.to_string())?;
            expect_host_control(
                &mut second,
                Instant::now() + Duration::from_secs(5),
                |message| matches!(message, ControlMessage::RequestKeyframe(request) if request.epoch == 2),
            )?;
            // UDP can legitimately lose one whole-frame datagram. Send a short
            // deterministic keyframe sequence so this reconnect test verifies
            // delivery/recovery rather than depending on one localhost packet.
            for frame_id in 1..=5 {
                sender
                    .send_frame(racc_net::SenderFrame {
                        epoch: 2,
                        frame_id,
                        keyframe: true,
                        config: true,
                        capture_ts_us: 20 + frame_id,
                        bytes: vec![
                            0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0, 0, 1, 0x68, 0xce, 0, 0, 1, 0x65,
                            0x88,
                        ],
                    })
                    .map_err(|error| error.to_string())?;
                thread::sleep(Duration::from_millis(10));
            }
            expect_host_control(
                &mut second,
                Instant::now() + Duration::from_secs(5),
                |message| matches!(message, ControlMessage::Goodbye(_)),
            )?;
            Ok(())
        });

        let slot = Arc::new(Mutex::new(None));
        let config = ViewerRuntimeConfig::new(
            host_addr,
            SocketAddr::new(ip, 0),
            "viewer-reconnect-test",
            OsType::Windows,
        );
        let mut runtime = ViewerRuntime::spawn_policy_with_decoder_factory(
            config,
            || Ok(FakeDecoder::new()),
            Sink(Arc::clone(&slot)),
            BindPolicy::TestOnlyLoopback,
        )
        .expect("runtime");
        let mut seen = Vec::new();
        let topology_deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < topology_deadline
            && !seen
                .iter()
                .any(|event| matches!(event, ViewerRuntimeEvent::TopologyChanged(_)))
        {
            seen.extend(runtime.poll_events(16).unwrap_or_default());
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            seen.iter()
                .any(|event| matches!(event, ViewerRuntimeEvent::TopologyChanged(_))),
            "initial topology was not delivered"
        );
        runtime
            .send(ViewerCommand::SelectDisplay(1))
            .expect("select display");

        let first_frame_deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < first_frame_deadline
            && !seen
                .iter()
                .any(|event| matches!(event, ViewerRuntimeEvent::FrameAvailable { epoch: 1, .. }))
        {
            seen.extend(runtime.poll_events(16).unwrap_or_default());
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            seen.iter()
                .any(|event| matches!(event, ViewerRuntimeEvent::FrameAvailable { epoch: 1, .. })),
            "initial keyframe was not decoded; events: {seen:?}"
        );
        assert_eq!(
            slot.lock()
                .expect("frame sink")
                .as_ref()
                .map(|frame| frame.epoch),
            Some(1)
        );
        runtime
            .send(ViewerCommand::SetVisible(false))
            .expect("hide viewer");
        first_paused_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("host did not receive pause request");
        release_first_tx.send(()).expect("release first control");

        reconnected_paused_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("reconnected viewer did not restore paused state");
        let reconnect_deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < reconnect_deadline
            && (seen
                .iter()
                .filter(|event| matches!(event, ViewerRuntimeEvent::Connected(_)))
                .count()
                < 2
                || seen
                    .iter()
                    .filter(|event| matches!(event, ViewerRuntimeEvent::TopologyChanged(_)))
                    .count()
                    < 2
                || !seen.iter().any(|event| {
                    matches!(
                        event,
                        ViewerRuntimeEvent::Session(SessionEvent::Reconnected)
                    )
                }))
        {
            seen.extend(runtime.poll_events(16).unwrap_or_default());
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            seen.iter()
                .filter(|event| matches!(event, ViewerRuntimeEvent::Connected(_)))
                .count()
                >= 2
        );
        assert!(
            seen.iter()
                .filter(|event| matches!(event, ViewerRuntimeEvent::TopologyChanged(_)))
                .count()
                >= 2
        );
        assert!(seen.iter().any(|event| matches!(
            event,
            ViewerRuntimeEvent::Session(SessionEvent::Reconnecting)
        )));
        assert_eq!(
            slot.lock()
                .expect("held frame")
                .as_ref()
                .map(|frame| frame.epoch),
            Some(1),
            "last good frame should remain available until the resumed keyframe"
        );
        assert!(!seen
            .iter()
            .any(|event| matches!(event, ViewerRuntimeEvent::Disconnected)));

        runtime
            .send(ViewerCommand::SetVisible(true))
            .expect("show viewer");
        let second_frame_deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < second_frame_deadline
            && !seen
                .iter()
                .any(|event| matches!(event, ViewerRuntimeEvent::FrameAvailable { epoch: 2, .. }))
        {
            seen.extend(runtime.poll_events(16).unwrap_or_default());
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            seen.iter().any(|event| matches!(
                event,
                ViewerRuntimeEvent::StreamReset(reset) if reset.epoch == 2
            )),
            "resumed stream reset was not delivered"
        );
        assert!(
            seen.iter()
                .any(|event| matches!(event, ViewerRuntimeEvent::FrameAvailable { epoch: 2, .. })),
            "resumed keyframe was not decoded; events: {seen:?}"
        );
        assert_eq!(
            slot.lock()
                .expect("promoted frame")
                .as_ref()
                .map(|frame| frame.epoch),
            Some(2),
            "new keyframe should atomically replace the held frame"
        );
        runtime.close().expect("close runtime");
        host.join().expect("host join").expect("host protocol");
    }

    #[test]
    fn busy_handshake_retries_without_stopping_the_viewer_runtime() {
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let listener = ControlListener::bind(
            SocketAddr::new(ip, 0),
            BindPolicy::TestOnlyLoopback,
            ControlSettings {
                read_timeout: Some(Duration::from_millis(250)),
                ..ControlSettings::default()
            },
        )
        .expect("test listener");
        let host_addr = listener.local_addr().expect("host addr");
        let host = thread::spawn(move || -> Result<(), String> {
            let (mut first, _) = listener.accept().map_err(|error| error.to_string())?;
            if !matches!(first.recv(), Ok(ControlMessage::Hello(_))) {
                return Err("first connection did not send Hello".to_owned());
            }
            first
                .send(&ControlMessage::HelloAck(HelloAck {
                    protocol_version: PROTOCOL_VERSION,
                    status: HelloStatus::Busy,
                    device_name: "fake host".to_owned(),
                    os: OsType::Windows,
                    app_version: "test".to_owned(),
                    codecs: 1,
                    max_height: 1080,
                    features: 0,
                    host_cpu_cores: 8,
                }))
                .map_err(|error| error.to_string())?;
            drop(first);

            let (mut retry, _) = listener.accept().map_err(|error| error.to_string())?;
            if !matches!(retry.recv(), Ok(ControlMessage::Hello(_))) {
                return Err("retry connection did not send Hello".to_owned());
            }
            retry
                .send(&ControlMessage::HelloAck(HelloAck {
                    protocol_version: PROTOCOL_VERSION,
                    status: HelloStatus::Ok,
                    device_name: "fake host".to_owned(),
                    os: OsType::Windows,
                    app_version: "test".to_owned(),
                    codecs: 1,
                    max_height: 1080,
                    features: 0,
                    host_cpu_cores: 8,
                }))
                .map_err(|error| error.to_string())?;
            loop {
                match retry.recv() {
                    Ok(ControlMessage::Goodbye(_)) | Err(racc_net::ControlError::Closed) => break,
                    Ok(_) | Err(racc_net::ControlError::Timeout) => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            Ok(())
        });

        let config = ViewerRuntimeConfig::new(
            host_addr,
            SocketAddr::new(ip, 0),
            "viewer-test",
            OsType::Windows,
        );
        let mut runtime = ViewerRuntime::spawn_policy_with_decoder_factory(
            config,
            || Ok(FakeDecoder::new()),
            Sink(Arc::new(Mutex::new(None))),
            BindPolicy::TestOnlyLoopback,
        )
        .expect("runtime");

        let deadline = Instant::now() + Duration::from_secs(12);
        let mut saw_reconnecting = false;
        let mut saw_connected = false;
        let mut failures = Vec::new();
        while Instant::now() < deadline && !saw_connected {
            for event in runtime.poll_events(16).unwrap_or_default() {
                match event {
                    ViewerRuntimeEvent::Session(SessionEvent::Reconnecting) => {
                        saw_reconnecting = true;
                    }
                    ViewerRuntimeEvent::Connected(_) => saw_connected = true,
                    ViewerRuntimeEvent::Failed(message) => failures.push(message),
                    _ => {}
                }
            }
            if !saw_connected {
                thread::sleep(Duration::from_millis(2));
            }
        }

        assert!(
            saw_reconnecting,
            "Busy was not exposed as a reconnecting state"
        );
        assert!(
            saw_connected,
            "runtime did not retry after the host became available"
        );
        assert!(
            failures.is_empty(),
            "Busy was treated as fatal: {failures:?}"
        );
        runtime.close().expect("close runtime");
        host.join().expect("host join").expect("host protocol");
    }

    #[test]
    fn loopback_handshake_topology_reset_decodes_directly_to_sink() {
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let listener = ControlListener::bind(
            SocketAddr::new(ip, 0),
            BindPolicy::TestOnlyLoopback,
            ControlSettings {
                read_timeout: Some(Duration::from_secs(2)),
                ..ControlSettings::default()
            },
        )
        .expect("test listener");
        let host_addr = listener.local_addr().expect("host addr");
        let host = thread::spawn(move || -> Result<(), String> {
            let (mut conn, viewer_addr) = listener.accept().map_err(|e| e.to_string())?;
            let hello = match conn.recv().map_err(|e| e.to_string())? {
                ControlMessage::Hello(v) => v,
                _ => return Err("missing Hello".to_owned()),
            };
            conn.send(&ControlMessage::HelloAck(HelloAck {
                protocol_version: PROTOCOL_VERSION,
                status: HelloStatus::Ok,
                device_name: "fake host".into(),
                os: OsType::Windows,
                app_version: "test".into(),
                codecs: 1,
                max_height: 1080,
                features: 0,
                host_cpu_cores: 8,
            }))
            .map_err(|e| e.to_string())?;
            conn.send(&ControlMessage::TopologyAnnounce(topology()))
                .map_err(|e| e.to_string())?;
            conn.send(&ControlMessage::StreamReset(reset()))
                .map_err(|e| e.to_string())?;
            let mut requested = false;
            for _ in 0..20 {
                match conn.recv() {
                    Ok(ControlMessage::RequestKeyframe(v)) if v.epoch == 1 => {
                        requested = true;
                        break;
                    }
                    Ok(_) => {}
                    Err(racc_net::ControlError::Timeout) => continue,
                    Err(e) => return Err(e.to_string()),
                }
            }
            if !requested {
                return Err("no IDR request".to_owned());
            }
            let sender = VideoSender::bind(
                SocketAddr::new(ip, 0),
                SocketAddr::new(viewer_addr.ip(), hello.video_udp_port),
                BindPolicy::TestOnlyLoopback,
                33_333,
            )
            .map_err(|e| e.to_string())?;
            sender
                .send_frame(racc_net::SenderFrame {
                    epoch: 1,
                    frame_id: 1,
                    keyframe: true,
                    config: true,
                    capture_ts_us: 10,
                    bytes: vec![
                        0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0, 0, 1, 0x68, 0xce, 0, 0, 1, 0x65, 0x88,
                    ],
                })
                .map_err(|e| e.to_string())?;
            loop {
                match conn.recv() {
                    Ok(ControlMessage::Goodbye(_)) => break,
                    Ok(_) => {}
                    Err(racc_net::ControlError::Timeout) => continue,
                    Err(e) => return Err(e.to_string()),
                }
            }
            Ok(())
        });
        let slot = Arc::new(Mutex::new(None));
        let (factory_tx, factory_rx) = mpsc::sync_channel(1);
        let config = ViewerRuntimeConfig::new(
            host_addr,
            SocketAddr::new(ip, 0),
            "viewer-test",
            OsType::Windows,
        );
        let mut runtime = ViewerRuntime::spawn_policy_with_decoder_factory(
            config,
            move || {
                let _ = factory_tx.send(thread::current().name().unwrap_or("unnamed").to_owned());
                Ok(FakeDecoder::new())
            },
            Sink(Arc::clone(&slot)),
            BindPolicy::TestOnlyLoopback,
        )
        .expect("runtime");
        assert_eq!(
            factory_rx
                .recv_timeout(Duration::from_secs(1))
                .ok()
                .as_deref(),
            Some("racc-viewer-decode"),
            "decoder factory must run on its owning worker"
        );
        // The test uses real loopback sockets and runs beside the rest of the
        // workspace suite. Allow ordinary scheduler jitter and avoid a hot spin
        // that can starve the host and decoder workers on loaded CI machines.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut saw_frame = false;
        let mut saw_decoder_selection = false;
        while Instant::now() < deadline {
            for event in runtime.poll_events(16).unwrap_or_default() {
                saw_frame |= matches!(
                    event,
                    ViewerRuntimeEvent::FrameAvailable {
                        epoch: 1,
                        frame_id: 1
                    }
                );
                saw_decoder_selection |= matches!(
                    event,
                    ViewerRuntimeEvent::DecoderSelected(DecoderKind::Unknown)
                );
            }
            if saw_frame
                && saw_decoder_selection
                && slot.lock().map(|x| x.is_some()).unwrap_or(false)
            {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(saw_frame, "frame metadata was not delivered");
        assert!(
            saw_decoder_selection,
            "fake decoder must be reported as unknown, not as a platform decoder"
        );
        let frame = slot.lock().expect("sink").clone().expect("frame published");
        assert_eq!(
            (frame.epoch, frame.frame_id, frame.width, frame.height),
            (1, 1, 640, 480)
        );
        assert!(matches!(frame.payload, crate::FramePayload::Nv12(_)));
        runtime.close().expect("close runtime");
        host.join().expect("host join").expect("host protocol");
    }

    #[test]
    fn clipboard_action_overflow_keeps_newest_and_clear_drops_pending_text() {
        let queue = Arc::new(Mutex::new(VecDeque::with_capacity(
            CLIPBOARD_ACTION_CAPACITY,
        )));
        for sequence in 0..=CLIPBOARD_ACTION_CAPACITY {
            let overflow = enqueue_clipboard_action(
                &queue,
                ClipboardPortAction {
                    sequence: sequence as u64,
                    text: format!("text-{sequence}"),
                },
            );
            assert_eq!(overflow, sequence == CLIPBOARD_ACTION_CAPACITY);
        }
        let actions = queue.lock().unwrap();
        assert_eq!(actions.len(), CLIPBOARD_ACTION_CAPACITY);
        assert_eq!(actions.front().unwrap().sequence, 1);
        assert_eq!(
            actions.back().unwrap().sequence,
            CLIPBOARD_ACTION_CAPACITY as u64
        );
        drop(actions);

        clear_clipboard_actions(&queue);
        assert!(queue.lock().unwrap().is_empty());
    }

    #[test]
    fn wire_clipboard_sequences_extend_across_u32_wrap() {
        let mut prior = None;
        assert_eq!(
            extend_wire_sequence(u32::MAX - 1, &mut prior),
            u64::from(u32::MAX - 1)
        );
        assert_eq!(extend_wire_sequence(1, &mut prior), u64::from(u32::MAX) + 2);
        assert_eq!(
            extend_wire_sequence(u32::MAX, &mut prior),
            u64::from(u32::MAX) + 2
        );
    }

    #[test]
    fn viewer_report_cadence_is_one_second_without_catch_up_bursts() {
        let mut cadence = ViewerReportCadence::default();
        assert!(!cadence.take_due(999_999, true));
        assert!(!cadence.take_due(1_000_000, false));
        assert!(cadence.take_due(1_000_000, true));
        assert!(!cadence.take_due(1_999_999, true));
        assert!(cadence.take_due(2_000_000, true));
        assert!(cadence.take_due(5_000_000, true));
        assert!(!cadence.take_due(5_000_001, true));
    }

    #[test]
    fn viewer_report_uses_rtt_loss_decode_p95_and_bounded_drop_sources() {
        let mut telemetry = TelemetryHub::new();
        telemetry.record_rtt(1, 25_001);
        telemetry.record_loss(1, 0.126, 0.055);
        let snapshot = telemetry.snapshot(1);

        let mut metrics = ViewerReportMetrics::default();
        metrics.reset_epoch(7);
        for duration_ms in 1..=20 {
            metrics.record_decode(7, duration_ms * 1_000);
        }
        metrics.record_decode(6, 60_000_000);
        metrics.record_dropped(7, 3);
        let (decode_ms_p95, dropped_frames) = metrics.take_report_values(7);
        let report = viewer_report_from_snapshot(7, &snapshot, decode_ms_p95, dropped_frames);

        assert_eq!(report.epoch, 7);
        assert_eq!(report.loss_permille, 126);
        assert_eq!(report.frame_loss_permille, 55);
        assert_eq!(report.rtt_ms, 26);
        assert_eq!(report.decode_ms_p95, 19);
        assert_eq!(report.dropped_frames, 3);
        assert_eq!(metrics.take_report_values(7), (0, 0));
    }

    #[test]
    fn packet_loss_estimate_populates_snapshot_and_event_log() {
        let mut telemetry = TelemetryHub::new();
        let mut last_loss_marker = 0;
        record_loss_estimate(&mut telemetry, 1_000, 0.125, 0.25, 3, &mut last_loss_marker);
        let snapshot = telemetry.snapshot(1_000);
        assert_eq!(snapshot.session.loss_fraction, 0.125);
        assert_eq!(snapshot.session.frame_loss_fraction, 0.25);
        assert!(snapshot
            .events
            .events()
            .iter()
            .any(|event| event.kind == EventKind::PacketLossEvent));
        assert_eq!(last_loss_marker, 3);
    }

    #[test]
    fn viewer_runtime_bridges_clipboard_and_reports_live_control_telemetry() {
        use racc_proto::{
            CaptureBackend, ClipboardOrigin, Encoder, HostEventKind, HostEventReport, LogicalClock,
            StatsReport,
        };

        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let listener = ControlListener::bind(
            SocketAddr::new(ip, 0),
            BindPolicy::TestOnlyLoopback,
            ControlSettings {
                read_timeout: Some(Duration::from_millis(200)),
                ..ControlSettings::default()
            },
        )
        .expect("test listener");
        let host_addr = listener.local_addr().expect("host address");
        let (report_tx, report_rx) = mpsc::sync_channel(1);
        let (clipboard_control_tx, clipboard_control_rx) = mpsc::sync_channel(4);
        let host = thread::spawn(move || -> Result<(), String> {
            let (mut conn, _) = listener.accept().map_err(|error| error.to_string())?;
            let hello = match conn.recv().map_err(|error| error.to_string())? {
                ControlMessage::Hello(hello) => hello,
                _ => return Err("viewer did not send Hello".to_owned()),
            };
            if hello.features & 1 == 0 {
                return Err("viewer did not advertise text clipboard support".to_owned());
            }
            conn.send(&ControlMessage::HelloAck(HelloAck {
                protocol_version: PROTOCOL_VERSION,
                status: HelloStatus::Ok,
                device_name: "telemetry host".to_owned(),
                os: OsType::Windows,
                app_version: "test".to_owned(),
                codecs: 1,
                max_height: 1080,
                features: 1,
                host_cpu_cores: 8,
            }))
            .map_err(|error| error.to_string())?;
            conn.send(&ControlMessage::StatsReport(StatsReport {
                host_cpu_pct_x10: 237,
                capture_backend: CaptureBackend::Dxgi,
                encoder: Encoder::MediaFoundationHw,
                width: 1280,
                height: 720,
                display_refresh_mhz: 60_000,
                target_bitrate_kbps: 3_500,
                actual_bitrate_kbps: 3_100,
                process_cpu_pct_x10: Some(123),
            }))
            .map_err(|error| error.to_string())?;
            conn.send(&ControlMessage::HostEventReport(HostEventReport {
                kind: HostEventKind::CaptureLost,
            }))
            .map_err(|error| error.to_string())?;
            conn.send(&ControlMessage::TopologyAnnounce(topology()))
                .map_err(|error| error.to_string())?;

            let mut sent_remote_clipboard = false;
            let mut clipboard_enabled = false;
            loop {
                match conn.recv() {
                    Ok(ControlMessage::Ping(ping)) => conn
                        .send(&ControlMessage::Pong(Pong {
                            nonce: ping.nonce,
                            echo_ts_us: ping.sender_ts_us,
                        }))
                        .map_err(|error| error.to_string())?,
                    Ok(ControlMessage::SwitchMonitor(request)) => {
                        let mut stream_reset = reset();
                        stream_reset.req_id = request.req_id;
                        conn.send(&ControlMessage::StreamReset(stream_reset))
                            .map_err(|error| error.to_string())?;
                    }
                    Ok(ControlMessage::ViewerReport(report)) => {
                        let _ = report_tx.try_send(report);
                    }
                    Ok(ControlMessage::ClipboardSyncControl(control)) => {
                        clipboard_enabled = control.enabled;
                        let _ = clipboard_control_tx.try_send(control.enabled);
                    }
                    Ok(ControlMessage::ClipboardUpdate(update)) => {
                        if !clipboard_enabled {
                            return Err(
                                "viewer sent clipboard text while sync was disabled".to_owned()
                            );
                        }
                        if update.origin != ClipboardOrigin::Viewer || update.text != "viewer text"
                        {
                            return Err("unexpected viewer clipboard update metadata".to_owned());
                        }
                        conn.send(&ControlMessage::ClipboardUpdate(
                            racc_proto::ClipboardUpdate {
                                seq: 0,
                                origin: ClipboardOrigin::Host,
                                logical_clock: LogicalClock::new(2)
                                    .map_err(|error| error.to_string())?,
                                text: "host text".to_owned(),
                            },
                        ))
                        .map_err(|error| error.to_string())?;
                        sent_remote_clipboard = true;
                    }
                    Ok(ControlMessage::Goodbye(_)) => break,
                    Ok(_) => {}
                    Err(racc_net::ControlError::Timeout) => continue,
                    Err(error) => return Err(error.to_string()),
                }
            }
            if !sent_remote_clipboard {
                return Err("viewer clipboard update was not received".to_owned());
            }
            Ok(())
        });

        let slot = Arc::new(Mutex::new(None));
        let config = ViewerRuntimeConfig::new(
            host_addr,
            SocketAddr::new(ip, 0),
            "viewer-test",
            OsType::Windows,
        );
        let mut runtime = ViewerRuntime::spawn_policy_with_decoder_factory(
            config,
            || Ok(FakeDecoder::new()),
            Sink(slot),
            BindPolicy::TestOnlyLoopback,
        )
        .expect("runtime");

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut connected = false;
        while Instant::now() < deadline && !connected {
            connected = runtime
                .poll_events(16)
                .unwrap_or_default()
                .iter()
                .any(|event| matches!(event, ViewerRuntimeEvent::Connected(_)));
            thread::sleep(Duration::from_millis(2));
        }
        assert!(connected, "viewer handshake did not complete");
        runtime
            .send(ViewerCommand::SetPath(PathKind::Derp))
            .expect("set current Tailscale path");
        runtime
            .send(ViewerCommand::SelectDisplay(1))
            .expect("select initial display");
        runtime
            .send(ViewerCommand::SetClipboardEnabled(true))
            .expect("enable clipboard");

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut enabled = false;
        while Instant::now() < deadline && !enabled {
            enabled = runtime
                .poll_clipboard_metadata(16)
                .unwrap_or_default()
                .iter()
                .any(|metadata| metadata.status == ClipboardTransferStatus::Enabled);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(enabled, "clipboard enable status was not published");
        runtime
            .notify_local_clipboard_change(b"viewer text")
            .expect("submit local text");

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut sent = false;
        let mut received = false;
        let mut action = None;
        let mut telemetry: Option<TelemetrySnapshot> = None;
        while Instant::now() < deadline
            && !(sent
                && received
                && action.is_some()
                && telemetry.as_ref().is_some_and(|snapshot| {
                    snapshot.session.last_rtt_us.is_some()
                        && snapshot.host.cpu_pct_x10 == 237
                        && snapshot.host.process_cpu_pct_x10 == Some(123)
                        && snapshot.session.path == PathKind::Derp
                        && snapshot
                            .events
                            .events()
                            .iter()
                            .any(|event| event.kind == EventKind::CaptureLost)
                }))
        {
            for metadata in runtime.poll_clipboard_metadata(16).unwrap_or_default() {
                match (metadata.direction, metadata.status, metadata.byte_len) {
                    (
                        ClipboardTransferDirection::LocalToRemote,
                        ClipboardTransferStatus::Sent,
                        Some(11),
                    ) => sent = true,
                    (
                        ClipboardTransferDirection::RemoteToLocal,
                        ClipboardTransferStatus::ReceivedPendingApply,
                        Some(9),
                    ) => received = true,
                    _ => {}
                }
            }
            action = runtime
                .poll_clipboard_actions(8)
                .unwrap_or_default()
                .into_iter()
                .next()
                .or(action);
            telemetry = runtime.poll_telemetry().unwrap_or(None).or(telemetry);
            thread::sleep(Duration::from_millis(5));
        }
        let action = action.expect("remote text action");
        assert_eq!(action.text, "host text");
        assert_eq!(action.sequence, 0);
        assert!(sent, "local update was not reported sent");
        assert!(received, "remote update was not reported pending apply");
        let telemetry = telemetry.expect("telemetry snapshot");
        assert_eq!(
            telemetry.session.connection_state,
            ConnectionState::Connected
        );
        assert!(telemetry.session.last_rtt_us.is_some());
        assert_eq!(telemetry.host.cpu_pct_x10, 237);
        assert_eq!(telemetry.host.process_cpu_pct_x10, Some(123));
        assert_eq!(telemetry.host.width, 1280);
        assert_eq!(telemetry.session.path, PathKind::Derp);
        assert!(telemetry
            .events
            .events()
            .iter()
            .any(|event| event.kind == EventKind::CaptureLost));

        let report = report_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("automatic viewer report");
        assert_eq!(report.epoch, 1);
        assert!(report.rtt_ms <= MAX_VIEWER_REPORT_DURATION_MS);
        assert_eq!(report.loss_permille, 0);
        assert_eq!(report.frame_loss_permille, 0);

        runtime
            .send(ViewerCommand::SetClipboardEnabled(false))
            .expect("disable clipboard");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut remote_disabled = false;
        while Instant::now() < deadline && !remote_disabled {
            remote_disabled = clipboard_control_rx.try_iter().any(|enabled| !enabled);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            remote_disabled,
            "host did not receive clipboard disable control"
        );
        runtime
            .notify_local_clipboard_change(b"must not leave after disable")
            .expect("the local port remains available");
        thread::sleep(Duration::from_millis(100));

        runtime.close().expect("close runtime");
        host.join().expect("host join").expect("host protocol");
    }
}
