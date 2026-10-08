//! Platform-neutral capture contracts, bounded GPU frame pooling, and a deterministic fake.
//!
//! The Windows implementation is isolated in [`windows`]. It only touches the desktop when a
//! host explicitly calls its start and poll methods; this crate does not probe a screen on load.

use std::any::Any;
use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use racc_topology::{Display, DisplayId};

/// macOS capture, Screen Recording permission, and display metadata APIs.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod macos;
#[cfg(all(test, not(target_os = "macos")))]
#[path = "macos/control.rs"]
#[allow(dead_code)]
mod macos_control_tests;
#[cfg(windows)]
#[allow(unsafe_code)]
pub mod windows;

/// A display selected from an OS capture adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedDisplay {
    /// Validated topology metadata in physical pixels.
    pub display: Display,
    /// Backend-local output key. It is not sent over the wire.
    pub backend_handle: String,
    /// Backend-local adapter key useful for capture/encode co-location decisions.
    pub adapter_id: String,
    /// Human-readable graphics adapter model, without its OS adapter identifier.
    pub adapter_model: String,
    /// Source used to derive the stable display identity.
    pub identity_source: DisplayIdentitySource,
}

/// How the capture backend constructed a stable display identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayIdentitySource {
    /// EDID manufacturer, product and serial plus connector were available.
    EdidAndConnector,
    /// EDID was unavailable; the backend used the monitor/device path only.
    ConnectorPathOnly,
    /// DisplayConfig lookup failed and the backend used DXGI's GDI device name as a fallback.
    /// This value is not guaranteed to remain stable across topology or driver changes.
    GdiDeviceNameFallback,
    /// CoreGraphics display UUID was used as the connector identity.
    MacDisplayUuid,
}

/// Requested limits for a capture session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureParams {
    /// Maximum output width in physical pixels.
    pub max_width: u32,
    /// Maximum output height in physical pixels.
    pub max_height: u32,
    /// Maximum wait in `poll_event`; bounded to keep stop/migrate responsive.
    pub acquire_timeout: Duration,
}

impl Default for CaptureParams {
    fn default() -> Self {
        Self {
            max_width: 1920,
            max_height: 1080,
            acquire_timeout: Duration::from_millis(8),
        }
    }
}

/// Computes an aspect-preserving output size no larger than the requested bounds.
///
/// A zero bound is treated as one pixel so callers never receive a zero-sized surface.
pub(crate) fn bounded_capture_dimensions(
    source_width: u32,
    source_height: u32,
    max_width: u32,
    max_height: u32,
) -> (u32, u32) {
    let (max_width, max_height) = (max_width.clamp(1, 1920), max_height.clamp(1, 1080));
    if source_width == 0 || source_height == 0 {
        return (0, 0);
    }
    if source_width <= max_width && source_height <= max_height {
        return (source_width, source_height);
    }
    let width_limited = u64::from(source_width) * u64::from(max_height)
        > u64::from(source_height) * u64::from(max_width);
    if width_limited {
        let height = (u64::from(source_height) * u64::from(max_width) / u64::from(source_width))
            .max(1) as u32;
        (max_width, height.min(max_height))
    } else {
        let width = (u64::from(source_width) * u64::from(max_height) / u64::from(source_height))
            .max(1) as u32;
        (width.min(max_width), max_height)
    }
}

/// A GPU resource opaque to platform-neutral capture consumers.
pub trait GpuResource: Any + fmt::Debug + Send + Sync {
    /// Stable id for diagnostics and fake assertions.
    fn resource_id(&self) -> u64;
    /// Access to a concrete platform wrapper when the caller shares the backend.
    fn as_any(&self) -> &dyn Any;
}

/// Small fixed-size pool of reusable GPU resources.
///
/// A slot returns to the pool when the last owner of its [`GpuFrame`] drops it. Exhaustion drops
/// the newest frame and increments a counter; it never waits for a consumer.
#[derive(Clone)]
pub struct TexturePool {
    inner: Arc<TexturePoolInner>,
}

struct TexturePoolInner {
    resources: Vec<Arc<dyn GpuResource>>,
    free_slots: Mutex<VecDeque<usize>>,
    dropped_newest: AtomicU64,
}

impl fmt::Debug for TexturePool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TexturePool")
            .field("capacity", &self.capacity())
            .field("available", &self.available())
            .field("dropped_newest", &self.dropped_newest())
            .finish()
    }
}

impl TexturePool {
    /// Creates a pool from already allocated resources. Empty pools are rejected.
    pub fn new(resources: Vec<Arc<dyn GpuResource>>) -> Result<Self, CaptureError> {
        if resources.is_empty() {
            return Err(CaptureError::EmptyTexturePool);
        }
        let free_slots = (0..resources.len()).collect();
        Ok(Self {
            inner: Arc::new(TexturePoolInner {
                resources,
                free_slots: Mutex::new(free_slots),
                dropped_newest: AtomicU64::new(0),
            }),
        })
    }
    /// Number of preallocated texture slots.
    pub fn capacity(&self) -> usize {
        self.inner.resources.len()
    }
    /// Number of slots available for the next frame.
    pub fn available(&self) -> usize {
        lock_unpoisoned(&self.inner.free_slots).len()
    }
    /// Number of frames dropped because every slot was still held by a consumer.
    pub fn dropped_newest(&self) -> u64 {
        self.inner.dropped_newest.load(Ordering::Relaxed)
    }
    /// Reserves a slot without waiting. Returns `None` when all pooled frames are held.
    pub fn try_acquire(&self) -> Option<GpuTextureLease> {
        let slot = lock_unpoisoned(&self.inner.free_slots).pop_front();
        let Some(slot) = slot else {
            self.inner.dropped_newest.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let resource = self.inner.resources.get(slot)?.clone();
        Some(GpuTextureLease {
            pool: self.inner.clone(),
            slot,
            resource,
        })
    }
}

/// Lease for one pooled texture. Dropping it returns the slot immediately.
pub struct GpuTextureLease {
    pool: Arc<TexturePoolInner>,
    slot: usize,
    resource: Arc<dyn GpuResource>,
}

impl fmt::Debug for GpuTextureLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GpuTextureLease")
            .field("slot", &self.slot)
            .field("resource_id", &self.resource.resource_id())
            .finish()
    }
}
impl GpuTextureLease {
    /// Returns the pooled platform texture wrapper.
    pub fn resource(&self) -> &dyn GpuResource {
        self.resource.as_ref()
    }
}
impl Drop for GpuTextureLease {
    fn drop(&mut self) {
        lock_unpoisoned(&self.pool.free_slots).push_back(self.slot);
    }
}

/// Damage summary attached to a GPU-resident frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DamageSummary {
    /// Damage metadata exceeded the backend's fixed bounds.
    Full,
    /// Counts of dirty and move rectangles returned by the OS.
    Rectangles { dirty: u16, moved: u16 },
    /// The frame carries no changed pixels.
    None,
}

/// Pixel layout of a capture texture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    /// 8-bit BGRA in a D3D11 texture.
    Bgra8,
}

/// Latest-wins video frame. Pixel contents remain on the GPU.
pub struct GpuFrame {
    lease: GpuTextureLease,
    /// Source display identity.
    pub display_id: DisplayId,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// GPU texture format.
    pub pixel_format: PixelFormat,
    /// Monotonic capture timestamp in microseconds.
    pub capture_ts_us: u64,
    /// OS damage metadata summary.
    pub damage: DamageSummary,
    /// Time spent waiting in the OS acquire call, in microseconds.
    pub acquire_wait_us: u64,
    /// CPU time spent submitting the GPU copy or scaling command, in microseconds.
    /// This is command-submission time, not GPU completion time.
    pub copy_scale_submit_us: u64,
    /// Time since the previous emitted frame's acquire completion, in microseconds.
    /// Zero is reported for the first frame.
    pub frame_interval_us: u64,
    /// Cumulative frame drops due to texture-pool exhaustion in this capture session.
    pub dropped_frames: u64,
}
impl fmt::Debug for GpuFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GpuFrame")
            .field("resource_id", &self.lease.resource().resource_id())
            .field("display_id", &self.display_id)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pixel_format", &self.pixel_format)
            .field("capture_ts_us", &self.capture_ts_us)
            .field("damage", &self.damage)
            .field("acquire_wait_us", &self.acquire_wait_us)
            .field("copy_scale_submit_us", &self.copy_scale_submit_us)
            .field("frame_interval_us", &self.frame_interval_us)
            .field("dropped_frames", &self.dropped_frames)
            .finish()
    }
}
impl GpuFrame {
    /// Returns the GPU resource for platform-specific encoder interop.
    pub fn texture(&self) -> &dyn GpuResource {
        self.lease.resource()
    }
}

/// Blend semantics retained while converting a Windows pointer shape to 32-bit BGRA.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorBlendMode {
    /// Normal premultiplied-alpha color pointer.
    PremultipliedAlpha,
    /// Windows masked-color pointer; the alpha byte selects copy or XOR behavior.
    WindowsMaskedColor,
    /// Windows monochrome AND/XOR bitplanes packed into BGRA alpha/RGB channels.
    WindowsAndXor,
}

/// Cursor bitmap normalized to 32-bit BGRA, kept separate from video frames.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorShape {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Hotspot x coordinate.
    pub hotspot_x: u32,
    /// Hotspot y coordinate.
    pub hotspot_y: u32,
    /// BGRA pixels, bounded by the capture backend.
    pub bgra8: Arc<[u8]>,
    /// Original Windows blend semantics needed to draw XOR pointers correctly.
    pub blend_mode: CursorBlendMode,
}
/// Pointer position/visibility in host physical pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CursorPosition {
    /// Host physical x coordinate.
    pub x: i32,
    /// Host physical y coordinate.
    pub y: i32,
    /// Whether the OS currently shows the pointer.
    pub visible: bool,
}

/// Capture events emitted by a backend.
pub enum CaptureEvent {
    /// A GPU-resident frame is available.
    Frame(GpuFrame),
    /// Cursor shape changed.
    CursorShape(CursorShape),
    /// Cursor position or visibility changed.
    CursorMoved(CursorPosition),
    /// Selected display disappeared.
    DisplayLost,
    /// Capture access was interrupted by the OS or content policy.
    AccessLost(AccessLostKind),
    /// D3D device was removed or reset.
    DeviceLost,
    /// Capture resumed after a recoverable interruption.
    Recovered,
    /// Non-recoverable or diagnostic backend error.
    Error(CaptureErrorKind),
}
impl fmt::Debug for CaptureEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(v) => f.debug_tuple("Frame").field(v).finish(),
            Self::CursorShape(v) => f.debug_tuple("CursorShape").field(v).finish(),
            Self::CursorMoved(v) => f.debug_tuple("CursorMoved").field(v).finish(),
            Self::DisplayLost => f.write_str("DisplayLost"),
            Self::AccessLost(v) => f.debug_tuple("AccessLost").field(v).finish(),
            Self::DeviceLost => f.write_str("DeviceLost"),
            Self::Recovered => f.write_str("Recovered"),
            Self::Error(v) => f.debug_tuple("Error").field(v).finish(),
        }
    }
}

/// Access-loss cause used by OS capture diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessLostKind {
    /// DXGI duplication was invalidated by a mode change, sleep, or display transition.
    DxgiAccessLost,
    /// Desktop topology or mode changed.
    ModeChanged,
    /// Interactive session was disconnected or switched.
    SessionDisconnected,
    /// OS denied access.
    AccessDenied,
    /// Protected content could not be captured.
    ProtectedContent,
}
/// Platform-neutral capture error category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureErrorKind {
    /// Driver lacks the requested capture interface.
    Unsupported,
    /// The driver returned an unclassified failure.
    DriverFailure,
    /// Capture returned an invalid resource or dimensions.
    InvalidFrame,
}

/// Input to the pure recovery mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureCondition {
    /// `AcquireNextFrame` timeout; normal when the desktop is static.
    WaitTimeout,
    /// `DXGI_ERROR_ACCESS_LOST`.
    DxgiAccessLost,
    /// `DXGI_ERROR_DEVICE_REMOVED` or `DXGI_ERROR_DEVICE_RESET`.
    DeviceLost,
    /// `DXGI_ERROR_UNSUPPORTED`.
    Unsupported,
    /// `DXGI_ERROR_SESSION_DISCONNECTED`.
    SessionDisconnected,
    /// `E_ACCESSDENIED`.
    AccessDenied,
    /// Display mode/topology changed.
    ModeChanged,
    /// Selected output was removed.
    DisplayRemoved,
    /// Protected content blocked capture.
    ProtectedContent,
    /// Any other OS/driver error.
    Other,
}
/// Recovery signal suitable for conversion into a capture event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureSignal {
    /// Access to the desktop was interrupted.
    AccessLost(AccessLostKind),
    /// Device reset/removal occurred.
    DeviceLost,
    /// Selected output disappeared.
    DisplayLost,
    /// Backend error.
    Error(CaptureErrorKind),
    /// Backend recovered.
    Recovered,
}
/// Recovery action chosen for a capture condition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryAction {
    /// Static desktop produced no new frame.
    NoFrame,
    /// Recreate or reattach capture after a bounded delay.
    RetryAfter(Duration),
    /// Re-enumerate outputs and await a valid target.
    Reenumerate,
    /// Stop this backend path; caller may choose a documented fallback.
    Stop,
}
/// Pure event/action mapping for capture loss and recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryDecision {
    /// Event to surface to the caller.
    pub signal: Option<CaptureSignal>,
    /// Capture-loop action.
    pub action: RecoveryAction,
}
/// Bounded retry ladder shared by Windows capture recovery and fake tests.
#[derive(Clone, Debug, Default)]
pub struct CaptureRecovery {
    attempts: usize,
}
impl CaptureRecovery {
    /// Number of recoverable failures since the last successful frame.
    pub const fn attempts(&self) -> usize {
        self.attempts
    }
    /// Maps one observed result to an event and a bounded recovery action.
    pub fn observe(&mut self, condition: CaptureCondition) -> RecoveryDecision {
        use CaptureCondition as C;
        let (signal, retry) = match condition {
            C::WaitTimeout => {
                return RecoveryDecision {
                    signal: None,
                    action: RecoveryAction::NoFrame,
                }
            }
            C::DxgiAccessLost => (
                Some(CaptureSignal::AccessLost(AccessLostKind::DxgiAccessLost)),
                true,
            ),
            C::DeviceLost => (Some(CaptureSignal::DeviceLost), true),
            C::Unsupported => (
                Some(CaptureSignal::Error(CaptureErrorKind::Unsupported)),
                false,
            ),
            C::SessionDisconnected => (
                Some(CaptureSignal::AccessLost(
                    AccessLostKind::SessionDisconnected,
                )),
                true,
            ),
            C::AccessDenied => (
                Some(CaptureSignal::AccessLost(AccessLostKind::AccessDenied)),
                true,
            ),
            C::ModeChanged => (
                Some(CaptureSignal::AccessLost(AccessLostKind::ModeChanged)),
                true,
            ),
            C::DisplayRemoved => (Some(CaptureSignal::DisplayLost), false),
            C::ProtectedContent => (
                Some(CaptureSignal::AccessLost(AccessLostKind::ProtectedContent)),
                true,
            ),
            C::Other => (
                Some(CaptureSignal::Error(CaptureErrorKind::DriverFailure)),
                true,
            ),
        };
        let action = if retry {
            let delay = retry_delay(self.attempts);
            self.attempts = self.attempts.saturating_add(1);
            RecoveryAction::RetryAfter(delay)
        } else if condition == C::DisplayRemoved {
            self.attempts = 0;
            RecoveryAction::Reenumerate
        } else {
            self.attempts = 0;
            RecoveryAction::Stop
        };
        RecoveryDecision { signal, action }
    }
    /// Clears retry history after a successful frame or completed reattach.
    pub fn recovered(&mut self) -> RecoveryDecision {
        self.attempts = 0;
        RecoveryDecision {
            signal: Some(CaptureSignal::Recovered),
            action: RecoveryAction::NoFrame,
        }
    }
}
fn retry_delay(attempt: usize) -> Duration {
    const RETRY_MS: [u64; 6] = [50, 100, 200, 400, 800, 1_000];
    Duration::from_millis(RETRY_MS[attempt.min(RETRY_MS.len() - 1)])
}

/// Errors from a capture implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CaptureError {
    /// Capture was polled before `start`.
    NotStarted,
    /// Target display is unavailable.
    DisplayUnavailable(DisplayId),
    /// GPU pool contained no textures.
    EmptyTexturePool,
    /// Native API failed with a stable diagnostic string.
    Platform(String),
}
impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotStarted => f.write_str("capture is not started"),
            Self::DisplayUnavailable(id) => write!(f, "display {} is unavailable", id.get()),
            Self::EmptyTexturePool => f.write_str("texture pool cannot be empty"),
            Self::Platform(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for CaptureError {}

/// Platform-neutral capture backend contract.
pub trait CaptureBackend: Send {
    /// Enumerates active OS outputs with topology and adapter metadata.
    fn enumerate_displays(&mut self) -> Result<Vec<CapturedDisplay>, CaptureError>;
    /// Starts capture on one available display.
    fn start(&mut self, display_id: DisplayId, params: CaptureParams) -> Result<(), CaptureError>;
    /// Attaches to another display while keeping the capture loop alive.
    fn migrate(&mut self, display_id: DisplayId) -> Result<(), CaptureError>;
    /// Stops capture and releases active backend resources.
    fn stop(&mut self);
    /// Polls one event; `None` means no changed pixels or queued metadata.
    fn poll_event(&mut self, timeout: Duration) -> Result<Option<CaptureEvent>, CaptureError>;
}

/// Deterministic fake capture backend for tests and the UI shell.
pub struct FakeCaptureBackend {
    displays: Vec<CapturedDisplay>,
    pool: TexturePool,
    queue: VecDeque<CaptureEvent>,
    queue_capacity: usize,
    dropped_events: u64,
    dropped_frames: u64,
    last_frame_ts_us: Option<u64>,
    pool_dropped_at_start: u64,
    selected_display: Option<DisplayId>,
    params: CaptureParams,
    recovery: CaptureRecovery,
    migration_count: u64,
    running: bool,
}
impl FakeCaptureBackend {
    /// Creates a fake backend with a three-texture pool and an eight-event bound.
    pub fn new(displays: Vec<CapturedDisplay>) -> Self {
        Self::with_limits(displays, 3, 8)
    }
    /// Creates a fake backend with explicit pool and event bounds; pool size is clamped to one.
    pub fn with_limits(
        displays: Vec<CapturedDisplay>,
        pool_size: usize,
        queue_capacity: usize,
    ) -> Self {
        let resources = (0..pool_size.max(1))
            .map(|index| Arc::new(FakeGpuResource(index as u64 + 1)) as Arc<dyn GpuResource>)
            .collect();
        let pool = TexturePool::new(resources)
            .unwrap_or_else(|_| unreachable!("clamped pool is nonempty"));
        Self {
            displays,
            pool,
            queue: VecDeque::with_capacity(queue_capacity),
            queue_capacity,
            dropped_events: 0,
            dropped_frames: 0,
            last_frame_ts_us: None,
            pool_dropped_at_start: 0,
            selected_display: None,
            params: CaptureParams::default(),
            recovery: CaptureRecovery::default(),
            migration_count: 0,
            running: false,
        }
    }
    /// Enqueues a deterministic fake frame. The newest drops if pool or queue capacity is full.
    pub fn emit_frame(&mut self, capture_ts_us: u64, damage: DamageSummary) -> bool {
        if !self.running {
            return false;
        }
        let Some(display_id) = self.selected_display else {
            return false;
        };
        let Some(display) = self
            .displays
            .iter()
            .find(|entry| entry.display.id() == display_id)
        else {
            return false;
        };
        let Some(lease) = self.pool.try_acquire() else {
            return false;
        };
        let (source_width, source_height) = display.display.size();
        let (width, height) = bounded_capture_dimensions(
            source_width,
            source_height,
            self.params.max_width,
            self.params.max_height,
        );
        let frame_interval_us = self
            .last_frame_ts_us
            .map_or(0, |previous| capture_ts_us.saturating_sub(previous));
        let dropped_frames = self.dropped_frames.saturating_add(
            self.pool
                .dropped_newest()
                .saturating_sub(self.pool_dropped_at_start),
        );
        let accepted = self.push_event(CaptureEvent::Frame(GpuFrame {
            lease,
            display_id,
            width,
            height,
            pixel_format: PixelFormat::Bgra8,
            capture_ts_us,
            damage,
            acquire_wait_us: 0,
            copy_scale_submit_us: 0,
            frame_interval_us,
            dropped_frames,
        }));
        if accepted {
            self.last_frame_ts_us = Some(capture_ts_us);
        }
        accepted
    }
    /// Injects a fake OS failure through the same pure recovery mapping as Windows.
    pub fn inject_condition(&mut self, condition: CaptureCondition) -> RecoveryDecision {
        let decision = self.recovery.observe(condition);
        if let Some(signal) = decision.signal {
            self.push_event(signal_to_event(signal));
        }
        if decision.action == RecoveryAction::Reenumerate {
            self.selected_display = None;
        }
        decision
    }
    /// Enqueues recovery and clears the retry history.
    pub fn inject_recovered(&mut self) {
        let decision = self.recovery.recovered();
        if let Some(signal) = decision.signal {
            self.push_event(signal_to_event(signal));
        }
    }
    /// Current display while started.
    pub const fn selected_display(&self) -> Option<DisplayId> {
        self.selected_display
    }
    /// Whether capture remains started after migration.
    pub const fn is_running(&self) -> bool {
        self.running
    }
    /// Number of display migrations since construction.
    pub const fn migration_count(&self) -> u64 {
        self.migration_count
    }
    /// Frames discarded because pool/queue capacity was exhausted.
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames
            .saturating_add(self.pool.dropped_newest())
    }
    /// Events discarded because the bounded fake queue was full.
    pub const fn dropped_events(&self) -> u64 {
        self.dropped_events
    }
    fn push_event(&mut self, event: CaptureEvent) -> bool {
        if self.queue.len() >= self.queue_capacity {
            self.dropped_events = self.dropped_events.saturating_add(1);
            if matches!(event, CaptureEvent::Frame(_)) {
                self.dropped_frames = self.dropped_frames.saturating_add(1);
            }
            return false;
        }
        self.queue.push_back(event);
        true
    }
}

#[derive(Debug)]
struct FakeGpuResource(u64);
impl GpuResource for FakeGpuResource {
    fn resource_id(&self) -> u64 {
        self.0
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl CaptureBackend for FakeCaptureBackend {
    fn enumerate_displays(&mut self) -> Result<Vec<CapturedDisplay>, CaptureError> {
        Ok(self.displays.clone())
    }
    fn start(&mut self, display_id: DisplayId, params: CaptureParams) -> Result<(), CaptureError> {
        if !self
            .displays
            .iter()
            .any(|entry| entry.display.id() == display_id && entry.display.flags().available())
        {
            return Err(CaptureError::DisplayUnavailable(display_id));
        }
        self.selected_display = Some(display_id);
        self.params = params;
        self.last_frame_ts_us = None;
        self.pool_dropped_at_start = self.pool.dropped_newest();
        self.dropped_frames = 0;
        self.running = true;
        Ok(())
    }
    fn migrate(&mut self, display_id: DisplayId) -> Result<(), CaptureError> {
        if !self.running {
            return Err(CaptureError::NotStarted);
        }
        if !self
            .displays
            .iter()
            .any(|entry| entry.display.id() == display_id && entry.display.flags().available())
        {
            return Err(CaptureError::DisplayUnavailable(display_id));
        }
        self.selected_display = Some(display_id);
        self.migration_count = self.migration_count.saturating_add(1);
        Ok(())
    }
    fn stop(&mut self) {
        self.running = false;
        self.selected_display = None;
    }
    fn poll_event(&mut self, _timeout: Duration) -> Result<Option<CaptureEvent>, CaptureError> {
        Ok(self.queue.pop_front())
    }
}

fn signal_to_event(signal: CaptureSignal) -> CaptureEvent {
    match signal {
        CaptureSignal::AccessLost(v) => CaptureEvent::AccessLost(v),
        CaptureSignal::DeviceLost => CaptureEvent::DeviceLost,
        CaptureSignal::DisplayLost => CaptureEvent::DisplayLost,
        CaptureSignal::Error(v) => CaptureEvent::Error(v),
        CaptureSignal::Recovered => CaptureEvent::Recovered,
    }
}
fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_topology::DisplayFlags;

    fn display(id: u32, x: i32) -> Display {
        Display::new(
            DisplayId::new(id).expect("nonzero test id"),
            format!("Display {id}"),
            x,
            0,
            1920,
            1080,
            1000,
            60_000,
            DisplayFlags::new(id == 1, true, true, false),
        )
    }
    fn displays() -> Vec<CapturedDisplay> {
        [display(1, 0), display(2, 1920)]
            .into_iter()
            .enumerate()
            .map(|(index, display)| CapturedDisplay {
                display,
                backend_handle: format!("output-{index}"),
                adapter_id: "adapter-0".to_owned(),
                adapter_model: "Fake Adapter".to_owned(),
                identity_source: DisplayIdentitySource::ConnectorPathOnly,
            })
            .collect()
    }

    #[test]
    fn capture_dimensions_preserve_aspect_ratio_and_obey_bounds() {
        assert_eq!(
            bounded_capture_dimensions(3840, 2160, 1920, 1080),
            (1920, 1080)
        );
        assert_eq!(
            bounded_capture_dimensions(2560, 1440, 1280, 720),
            (1280, 720)
        );
        assert_eq!(
            bounded_capture_dimensions(1080, 1920, 1920, 1080),
            (607, 1080)
        );
        assert_eq!(
            bounded_capture_dimensions(1365, 768, 1920, 1080),
            (1365, 768)
        );
        assert_eq!(bounded_capture_dimensions(1600, 900, 0, 0), (1, 1));
        assert_eq!(
            bounded_capture_dimensions(3840, 2160, 7680, 4320),
            (1920, 1080)
        );
        assert_eq!(
            bounded_capture_dimensions(3000, 1500, u32::MAX, u32::MAX),
            (1920, 960)
        );
        assert_eq!(
            bounded_capture_dimensions(1600, 2000, u32::MAX, u32::MAX),
            (864, 1080)
        );
    }

    #[test]
    fn texture_pool_is_bounded_nonblocking_and_recycles_after_last_lease_drop() {
        let resources: Vec<Arc<dyn GpuResource>> = (0..3)
            .map(|id| Arc::new(FakeGpuResource(id)) as Arc<dyn GpuResource>)
            .collect();
        let pool = TexturePool::new(resources).expect("three slots");
        let a = pool.try_acquire().expect("slot a");
        let b = pool.try_acquire().expect("slot b");
        let c = pool.try_acquire().expect("slot c");
        assert!(pool.try_acquire().is_none());
        assert_eq!(pool.dropped_newest(), 1);
        assert_eq!(pool.available(), 0);
        drop(b);
        let replacement = pool.try_acquire().expect("released slot recycled");
        drop((a, c, replacement));
        assert_eq!(pool.available(), 3);
    }

    #[test]
    fn fake_capture_migrates_without_stopping_and_drops_newest_when_bounded() {
        let mut fake = FakeCaptureBackend::with_limits(displays(), 1, 1);
        let listed = fake.enumerate_displays().expect("display list");
        assert_eq!(listed.len(), 2);
        let first = listed[0].display.id();
        let second = listed[1].display.id();
        fake.start(first, CaptureParams::default()).expect("start");
        assert!(fake.emit_frame(100, DamageSummary::Full));
        assert!(!fake.emit_frame(200, DamageSummary::None));
        assert_eq!(fake.dropped_frames(), 1);
        let first = fake.poll_event(Duration::ZERO).expect("poll");
        let Some(CaptureEvent::Frame(first)) = first else {
            panic!("expected first fake frame");
        };
        assert_eq!(first.capture_ts_us, 100);
        assert_eq!(first.acquire_wait_us, 0);
        assert_eq!(first.copy_scale_submit_us, 0);
        assert_eq!(first.frame_interval_us, 0);
        assert_eq!(first.dropped_frames, 0);
        drop(first);
        assert!(fake.migrate(second).is_ok());
        assert!(fake.is_running());
        assert_eq!(fake.selected_display(), Some(second));
        assert_eq!(fake.migration_count(), 1);
        assert!(fake.emit_frame(300, DamageSummary::Rectangles { dirty: 1, moved: 0 }));
        let Some(CaptureEvent::Frame(next)) = fake.poll_event(Duration::ZERO).expect("poll next")
        else {
            panic!("expected next fake frame");
        };
        assert_eq!(next.frame_interval_us, 200);
        assert_eq!(next.dropped_frames, 1);
    }

    #[test]
    fn recovery_maps_all_capture_error_classes() {
        let cases = [
            (CaptureCondition::WaitTimeout, None, RecoveryAction::NoFrame),
            (
                CaptureCondition::DxgiAccessLost,
                Some(CaptureSignal::AccessLost(AccessLostKind::DxgiAccessLost)),
                RecoveryAction::RetryAfter(Duration::from_millis(50)),
            ),
            (
                CaptureCondition::DeviceLost,
                Some(CaptureSignal::DeviceLost),
                RecoveryAction::RetryAfter(Duration::from_millis(50)),
            ),
            (
                CaptureCondition::Unsupported,
                Some(CaptureSignal::Error(CaptureErrorKind::Unsupported)),
                RecoveryAction::Stop,
            ),
            (
                CaptureCondition::SessionDisconnected,
                Some(CaptureSignal::AccessLost(
                    AccessLostKind::SessionDisconnected,
                )),
                RecoveryAction::RetryAfter(Duration::from_millis(50)),
            ),
            (
                CaptureCondition::AccessDenied,
                Some(CaptureSignal::AccessLost(AccessLostKind::AccessDenied)),
                RecoveryAction::RetryAfter(Duration::from_millis(50)),
            ),
            (
                CaptureCondition::ModeChanged,
                Some(CaptureSignal::AccessLost(AccessLostKind::ModeChanged)),
                RecoveryAction::RetryAfter(Duration::from_millis(50)),
            ),
            (
                CaptureCondition::DisplayRemoved,
                Some(CaptureSignal::DisplayLost),
                RecoveryAction::Reenumerate,
            ),
            (
                CaptureCondition::ProtectedContent,
                Some(CaptureSignal::AccessLost(AccessLostKind::ProtectedContent)),
                RecoveryAction::RetryAfter(Duration::from_millis(50)),
            ),
            (
                CaptureCondition::Other,
                Some(CaptureSignal::Error(CaptureErrorKind::DriverFailure)),
                RecoveryAction::RetryAfter(Duration::from_millis(50)),
            ),
        ];
        for (condition, signal, action) in cases {
            let mut recovery = CaptureRecovery::default();
            let decision = recovery.observe(condition);
            assert_eq!(decision.signal, signal, "{condition:?}");
            assert_eq!(decision.action, action, "{condition:?}");
        }
    }

    #[test]
    fn retry_backoff_caps_resets_on_success_and_wait_timeout_is_neutral() {
        let mut recovery = CaptureRecovery::default();
        for delay in [50, 100, 200, 400, 800, 1000, 1000] {
            assert_eq!(
                recovery.observe(CaptureCondition::ModeChanged).action,
                RecoveryAction::RetryAfter(Duration::from_millis(delay))
            );
        }
        assert_eq!(recovery.attempts(), 7);
        assert_eq!(
            recovery.observe(CaptureCondition::WaitTimeout).action,
            RecoveryAction::NoFrame
        );
        assert_eq!(recovery.attempts(), 7);
        assert_eq!(recovery.recovered().signal, Some(CaptureSignal::Recovered));
        assert_eq!(recovery.attempts(), 0);
    }
}
