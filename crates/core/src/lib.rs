//! UI/core boundary for the remote desktop application.
//!
//! Commands and events carry control and metadata only. Video pixels use the
//! independent FrameSource / FrameSink path and are never represented by
//! CoreEvent or CoreSnapshot.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Tailscale peer discovery integration for core consumers.
pub mod discovery;
pub mod host_runtime;
pub mod ipc;
pub mod viewer_pipeline;
pub mod viewer_runtime;

pub use viewer_pipeline::{
    DecodeBatchStats, ViewerFramePipeline, ViewerPipelineError, ViewerQueueOutcome,
};
pub use viewer_runtime::{
    ClipboardMetadata, ClipboardPortAction, ClipboardTransferDirection, ClipboardTransferStatus,
    ViewerCommand, ViewerRuntime, ViewerRuntimeConfig, ViewerRuntimeError, ViewerRuntimeEvent,
    VIEWER_RUNTIME_EVENT_CAPACITY,
};

pub use discovery::{CoreDeviceDiscovery, DeviceDiscoveryUpdate, DiscoveredPeer};

pub use host_runtime::{
    is_tailscale_address, HostConnectionId, HostPeerAuthorization, HostQualityTier, HostRuntime,
    HostRuntimeCounters, HostRuntimeError, HostRuntimeEvent, HostRuntimePhase, HostRuntimeStatus,
    HostSenderObservation, PendingHostAuthorization, MAX_HOST_ENCODER_LAG_MS,
    MAX_HOST_PEER_KEY_BYTES, MAX_HOST_PEER_LABEL_BYTES, MAX_PENDING_HOST_AUTHORIZATIONS,
};
pub use racc_identity::DEFAULT_CONTROL_PORT;

use std::fmt;
use std::sync::Arc;

pub use ipc::{
    AllowlistEntry, HelperState, HostStatus, IpcClient, IpcError, IpcEvent, IpcFailureCode,
    IpcRequest, IpcRequestHandler, IpcResponse, IpcServerMessage, IpcTransport, PeerAction,
    PendingPeer, MAX_IPC_FRAME_BYTES, MAX_IPC_TEXT_BYTES, MAX_PEER_LIST_ENTRIES,
};
use racc_proto::OsType;
use racc_telemetry::TelemetrySnapshot;
pub use racc_topology::{DisplayId, Topology};

/// Stable identifier for a known peer, usually its Tailscale node ID.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeviceId(String);

impl DeviceId {
    /// Creates a device id from a non-empty string.
    pub fn new(value: impl Into<String>) -> Result<Self, CoreError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(CoreError::EmptyDeviceId);
        }
        Ok(Self(value))
    }

    /// Returns the identifier's string representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Display metadata suitable for one device row in the UI.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DisplaySnapshot {
    /// Stable display identifier.
    pub id: DisplayId,
    /// User-facing display name.
    pub name: String,
    /// Physical-pixel dimensions.
    pub width_px: u32,
    /// Physical-pixel dimensions.
    pub height_px: u32,
    /// Display scale in thousandths, where 1500 represents 150 percent.
    pub scale_milli: u16,
    /// Refresh rate in thousandths of a hertz.
    pub refresh_mhz: u32,
    /// Whether the display is available for capture.
    pub available: bool,
    /// Whether this is the primary display.
    pub primary: bool,
}

impl From<&racc_topology::Display> for DisplaySnapshot {
    fn from(display: &racc_topology::Display) -> Self {
        let flags = display.flags();
        let (width_px, height_px) = display.size();
        Self {
            id: display.id(),
            name: display.name().to_owned(),
            width_px,
            height_px,
            scale_milli: display.scale_milli(),
            refresh_mhz: display.refresh_mhz(),
            available: flags.available(),
            primary: flags.primary(),
        }
    }
}

/// One discovered or remembered peer and its current display topology.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DeviceSnapshot {
    /// Stable identity of this peer.
    pub id: DeviceId,
    /// Human-readable device name.
    pub name: String,
    /// Peer operating system.
    pub os: OsType,
    /// Whether Tailscale currently reports the peer online.
    pub online: bool,
    /// Whether the peer answered the project host handshake.
    pub host_capable: bool,
    /// Current displays, if a topology has been announced.
    pub displays: Vec<DisplaySnapshot>,
    /// Currently streamed display, if known.
    pub streamed_display: Option<DisplayId>,
}

/// Immutable UI-facing snapshot of local and remote session state.
///
/// This contains small metadata only; decoded or captured pixel data is absent.
#[derive(Clone, Debug)]
pub struct CoreSnapshot {
    /// Remote peers known to the local core.
    pub devices: Vec<DeviceSnapshot>,
    /// Selected peer, if one is selected.
    pub selected_device: Option<DeviceId>,
    /// Selected display of the current peer, if any.
    pub selected_display: Option<DisplayId>,
    /// Local machine display name.
    pub local_device_name: String,
    /// Whether local hosting is enabled.
    pub hosting_enabled: bool,
    /// Whether the viewer window is visible and should decode video.
    pub visible: bool,
    /// Whether the active session permits clipboard synchronization.
    pub clipboard_session_active: bool,
    /// Whether the user enabled bidirectional text clipboard synchronization.
    pub clipboard_sync_enabled: bool,
    /// Current session and host telemetry.
    pub telemetry: TelemetrySnapshot,
}

impl Default for CoreSnapshot {
    fn default() -> Self {
        Self {
            devices: Vec::new(),
            selected_device: None,
            selected_display: None,
            local_device_name: String::new(),
            hosting_enabled: false,
            visible: true,
            clipboard_session_active: false,
            clipboard_sync_enabled: false,
            telemetry: TelemetrySnapshot::default(),
        }
    }
}

/// Video quality preference sent by the viewer; the host remains authoritative.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum QualityPreset {
    /// Request the 480p30 tier.
    P480,
    /// Request the 720p30 tier.
    P720,
    /// Request the 1080p30 tier.
    P1080,
    /// Let the host adapt within the supported range.
    Auto,
}

/// Commands sent from the UI to the core.
///
/// This enum contains no frame or pixel payloads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UiCommand {
    /// Rescan the configured peer source for host-capable devices.
    DiscoverDevices,
    /// Select a known peer in the device list.
    SelectDevice(DeviceId),
    /// Request a display on the selected peer.
    SelectDisplay {
        /// Target peer.
        device_id: DeviceId,
        /// Target display.
        display_id: DisplayId,
    },
    /// Set a viewer quality preference.
    SetQuality {
        /// Target peer.
        device_id: DeviceId,
        /// Requested quality tier.
        quality: QualityPreset,
    },
    /// Tell the core whether the application window is visible.
    SetVisible(bool),
    /// Enable or release keyboard capture.
    ToggleKeyboardCapture(bool),
    /// Enable or release mouse capture.
    ToggleMouseCapture(bool),
    /// Enable or disable text clipboard synchronization for the active session.
    SetClipboardEnabled(bool),
    /// Disconnect from the selected peer.
    Disconnect,
    /// Connect to a known peer.
    Connect(DeviceId),
    /// Approve a pending peer authorization request.
    ApprovePeer(DeviceId),
    /// Reject a pending peer authorization request.
    RejectPeer(DeviceId),
    /// Remove an approved peer from the local allowlist.
    RemovePeer(DeviceId),
    /// Enable or disable hosting on this machine.
    SetHosting(bool),
}

/// Events from the core to the UI.
///
/// Video frames are deliberately not an event variant. They travel through the
/// separate frame handoff traits.
#[derive(Clone, Debug)]
pub enum CoreEvent {
    /// A new peer was discovered or its metadata changed.
    DeviceDiscovered(DeviceSnapshot),
    /// A previously known peer became unreachable.
    DeviceOffline {
        /// Peer that became unreachable.
        device_id: DeviceId,
    },
    /// A bounded Tailscale device-list refresh completed successfully.
    DeviceDiscoveryCompleted {
        /// Number of peers included in the refreshed device catalogue.
        peer_count: usize,
        /// Local machine name reported by Tailscale, if available.
        local_device_name: Option<String>,
    },
    /// A peer's display list or topology revision changed.
    TopologyChanged {
        /// Peer whose topology changed.
        device_id: DeviceId,
        /// New validated topology.
        topology: Topology,
    },
    /// The active peer/display selection changed.
    DisplaySelected {
        /// Selected peer.
        device_id: DeviceId,
        /// Selected display.
        display_id: DisplayId,
    },
    /// Video streaming began.
    StreamStarted {
        /// Streaming peer.
        device_id: DeviceId,
        /// Streamed display.
        display_id: DisplayId,
        /// Stream width in pixels.
        width: u16,
        /// Stream height in pixels.
        height: u16,
        /// Stream frame rate.
        fps: u8,
    },
    /// A stream epoch or configuration changed.
    StreamReset {
        /// Streaming peer.
        device_id: DeviceId,
        /// New stream epoch.
        epoch: u16,
        /// Stream width in pixels.
        width: u16,
        /// Stream height in pixels.
        height: u16,
    },
    /// The decoder is ready to accept frames for the current stream.
    DecoderReady {
        /// Streaming peer.
        device_id: DeviceId,
    },
    /// Live connection and host telemetry changed.
    ConnectionStatsUpdated(TelemetrySnapshot),
    /// Local keyboard or mouse capture state changed.
    InputCaptureChanged {
        /// Whether keyboard capture is active.
        keyboard: bool,
        /// Whether mouse capture is active.
        mouse: bool,
    },
    /// A remote session ended.
    SessionEnded {
        /// Peer for the ended session, if there was one.
        device_id: Option<DeviceId>,
        /// Reason the session ended.
        reason: SessionEndReason,
    },
    /// A peer is waiting for local approval before it can connect.
    PendingAuthorization {
        /// Peer waiting for approval.
        device_id: DeviceId,
        /// Human-readable peer name.
        device_name: String,
    },
    /// Local clipboard synchronization state changed.
    ClipboardStatus(ClipboardStatus),
    /// A user-visible event or warning.
    Notification(Notification),
    /// The live frame source has no usable frame until a keyframe arrives.
    VideoUnavailable {
        /// Peer whose video is temporarily unavailable.
        device_id: DeviceId,
    },
}

/// Why a session ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionEndReason {
    /// The user explicitly disconnected.
    Disconnected,
    /// The remote peer sent Goodbye.
    RemoteClosed,
    /// The control connection timed out.
    ControlTimeout,
    /// An unrecoverable stream failure occurred.
    StreamFailure,
}

/// Clipboard synchronization status for UI display.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClipboardStatus {
    /// Clipboard sync is ready.
    Ready,
    /// Text was copied from the remote peer.
    Received {
        /// Clipboard update sequence.
        sequence: u64,
    },
    /// Text was sent to the remote peer.
    Sent {
        /// Clipboard update sequence.
        sequence: u64,
    },
    /// Clipboard sync could not complete.
    Failed {
        /// Short failure description.
        message: String,
    },
}

/// Severity of a user-facing notification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationLevel {
    /// Informational lifecycle detail.
    Info,
    /// A recoverable issue needs attention.
    Warning,
    /// An operation failed.
    Error,
}

/// User-facing notification metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notification {
    /// Severity.
    pub level: NotificationLevel,
    /// Short title.
    pub title: String,
    /// Concise explanatory message.
    pub message: String,
}

/// Errors returned by core and frame-boundary operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoreError {
    /// A device id was empty or whitespace-only.
    EmptyDeviceId,
    /// Command submission or event polling used a closed core.
    Closed,
    /// The supplied frame dimensions or plane strides were invalid.
    InvalidFrame(FrameValidationError),
    /// The supplied frame planes do not contain enough bytes for their layout.
    TruncatedFramePlane,
}

impl fmt::Display for CoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDeviceId => formatter.write_str("device id must not be empty"),
            Self::Closed => formatter.write_str("core handle is closed"),
            Self::InvalidFrame(error) => error.fmt(formatter),
            Self::TruncatedFramePlane => formatter.write_str("video frame plane is truncated"),
        }
    }
}

impl std::error::Error for CoreError {}

/// Contract between the UI event loop and the platform-neutral core.
///
/// Implementations keep command/event processing independent from UI
/// rendering. This interface carries only commands and metadata.
pub trait CoreHandle {
    /// Submits one UI command to the core.
    fn send(&mut self, command: UiCommand) -> Result<(), CoreError>;

    /// Returns queued metadata events, draining at most max_events.
    fn poll_events(&mut self, max_events: usize) -> Result<Vec<CoreEvent>, CoreError>;

    /// Returns a current immutable metadata snapshot.
    fn snapshot(&self) -> Result<CoreSnapshot, CoreError>;
}

/// Shared immutable bytes for one video plane.
pub type SharedPlane = Arc<[u8]>;

/// One row-pitched NV12 frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Nv12Frame {
    /// Luma plane, one byte per pixel.
    pub y: SharedPlane,
    /// Interleaved chroma plane, two bytes per 2x2 pixel block.
    pub uv: SharedPlane,
    /// Byte distance between luma rows; must be at least width.
    pub y_stride: u32,
    /// Byte distance between chroma rows; must be at least width.
    pub uv_stride: u32,
}

/// Payload provided by a real decoder or a deterministic synthetic source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FramePayload {
    /// Decoded NV12 content shared without copying pixel buffers.
    Nv12(Nv12Frame),
    /// Renderer-generated test pattern; contains metadata and no image bytes.
    SyntheticPattern {
        /// Deterministic pattern seed.
        seed: u64,
        /// Frame number used to animate the pattern.
        frame_number: u64,
    },
}

/// One frame handed directly to the native renderer.
///
/// This type is intentionally not accepted by CoreHandle or stored in
/// CoreSnapshot / CoreEvent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoFrame {
    /// Stream epoch, used to reject stale frames.
    pub epoch: u16,
    /// Monotonic frame identifier within the epoch.
    pub frame_id: u32,
    /// Display represented by this frame, if known.
    pub display_id: Option<DisplayId>,
    /// Encoded/decoded image width.
    pub width: u16,
    /// Encoded/decoded image height.
    pub height: u16,
    /// Frame rate supplied by the stream.
    pub fps: u8,
    /// Host monotonic capture timestamp carried by the video datagrams, wrapping at u32.
    /// This timestamp cannot be compared with the viewer clock without a clock-offset estimate.
    pub capture_ts_us: Option<u32>,
    /// Time spent inside the viewer decoder call, in viewer-local microseconds.
    pub decode_duration_us: Option<u64>,
    /// Frame pixels or synthetic test-pattern parameters.
    pub payload: FramePayload,
}

impl VideoFrame {
    /// Validates a frame's dimensions, row strides, and plane lengths.
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.width == 0 || self.height == 0 || self.fps == 0 {
            return Err(CoreError::InvalidFrame(FrameValidationError::ZeroDimension));
        }
        if let FramePayload::Nv12(frame) = &self.payload {
            let width = usize::from(self.width);
            let height = usize::from(self.height);
            let y_stride = frame.y_stride as usize;
            let uv_stride = frame.uv_stride as usize;
            if y_stride < width || uv_stride < width {
                return Err(CoreError::InvalidFrame(
                    FrameValidationError::StrideTooSmall,
                ));
            }
            let y_required = y_stride.checked_mul(height).ok_or(CoreError::InvalidFrame(
                FrameValidationError::PlaneLengthOverflow,
            ))?;
            let uv_rows = height.div_ceil(2);
            let uv_required = uv_stride
                .checked_mul(uv_rows)
                .ok_or(CoreError::InvalidFrame(
                    FrameValidationError::PlaneLengthOverflow,
                ))?;
            if frame.y.len() < y_required || frame.uv.len() < uv_required {
                return Err(CoreError::TruncatedFramePlane);
            }
        }
        Ok(())
    }
}

/// Why an NV12 frame failed validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameValidationError {
    /// Frame width, height, or rate was zero.
    ZeroDimension,
    /// A plane row stride was shorter than the corresponding row.
    StrideTooSmall,
    /// Multiplying row stride by plane height overflowed the platform size.
    PlaneLengthOverflow,
}

impl fmt::Display for FrameValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroDimension => {
                formatter.write_str("frame dimensions and frame rate must be nonzero")
            }
            Self::StrideTooSmall => {
                formatter.write_str("NV12 row stride is smaller than the row width")
            }
            Self::PlaneLengthOverflow => formatter.write_str("NV12 plane dimensions overflow"),
        }
    }
}

/// A publisher that replaces stale frames instead of growing a frame queue.
pub trait FrameSink: Send + Sync {
    /// Publishes one decoded or synthetic frame.
    fn publish_frame(&self, frame: Arc<VideoFrame>) -> Result<(), CoreError>;
}

/// A renderer-side handle for reading the latest available frame.
pub trait FrameSource: Send + Sync {
    /// Returns the latest frame, if one is ready; old frames may be skipped.
    fn latest_frame(&self) -> Option<Arc<VideoFrame>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display_id(raw: u32) -> DisplayId {
        DisplayId::new(raw).unwrap_or_else(|| unreachable!())
    }

    #[test]
    fn device_id_rejects_blank_and_preserves_value() {
        assert_eq!(DeviceId::new("  "), Err(CoreError::EmptyDeviceId));
        let id = DeviceId::new("node-123").unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(id.as_str(), "node-123");
    }

    #[test]
    fn snapshot_is_metadata_only_and_default_is_empty() {
        let snapshot = CoreSnapshot::default();
        assert!(snapshot.devices.is_empty());
        assert!(snapshot.selected_device.is_none());
        assert!(snapshot.selected_display.is_none());
        assert!(snapshot.visible);
    }

    #[test]
    fn synthetic_pattern_is_metadata_only_and_frame_is_valid() {
        let frame = VideoFrame {
            epoch: 4,
            frame_id: 9,
            display_id: Some(display_id(1)),
            width: 1280,
            height: 720,
            fps: 30,
            capture_ts_us: None,
            decode_duration_us: None,
            payload: FramePayload::SyntheticPattern {
                seed: 7,
                frame_number: 9,
            },
        };
        assert!(frame.validate().is_ok());
        let event = CoreEvent::DisplaySelected {
            device_id: DeviceId::new("peer").unwrap_or_else(|error| panic!("{error}")),
            display_id: display_id(1),
        };
        assert!(matches!(event, CoreEvent::DisplaySelected { .. }));
    }

    #[test]
    fn display_snapshot_preserves_scale() {
        let display = racc_topology::Display::new(
            display_id(1),
            "Main display",
            -1920,
            0,
            1920,
            1080,
            1500,
            60_000,
            racc_topology::DisplayFlags::new(true, true, true, false),
        );
        let snapshot = DisplaySnapshot::from(&display);
        assert_eq!(snapshot.scale_milli, 1500);
        assert_eq!(snapshot.width_px, 1920);
        assert!(snapshot.primary);
    }

    #[test]
    fn nv12_validates_stride_and_plane_sizes() {
        let valid = VideoFrame {
            epoch: 0,
            frame_id: 1,
            display_id: Some(display_id(1)),
            width: 4,
            height: 4,
            fps: 30,
            capture_ts_us: None,
            decode_duration_us: None,
            payload: FramePayload::Nv12(Nv12Frame {
                y: Arc::from(vec![0; 16]),
                uv: Arc::from(vec![128; 8]),
                y_stride: 4,
                uv_stride: 4,
            }),
        };
        assert!(valid.validate().is_ok());

        let short = VideoFrame {
            payload: FramePayload::Nv12(Nv12Frame {
                y: Arc::from(vec![0; 15]),
                uv: Arc::from(vec![128; 8]),
                y_stride: 4,
                uv_stride: 4,
            }),
            ..valid
        };
        assert_eq!(short.validate(), Err(CoreError::TruncatedFramePlane));
    }

    #[test]
    fn nv12_rejects_short_stride_and_zero_dimensions() {
        let short_stride = VideoFrame {
            epoch: 0,
            frame_id: 1,
            display_id: None,
            width: 4,
            height: 4,
            fps: 30,
            capture_ts_us: None,
            decode_duration_us: None,
            payload: FramePayload::Nv12(Nv12Frame {
                y: Arc::from(vec![0; 16]),
                uv: Arc::from(vec![128; 8]),
                y_stride: 3,
                uv_stride: 4,
            }),
        };
        assert_eq!(
            short_stride.validate(),
            Err(CoreError::InvalidFrame(
                FrameValidationError::StrideTooSmall
            ))
        );
        assert_eq!(
            VideoFrame {
                width: 0,
                ..short_stride
            }
            .validate(),
            Err(CoreError::InvalidFrame(FrameValidationError::ZeroDimension))
        );
    }
}
