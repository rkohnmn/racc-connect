//! Deterministic host and viewer session lifecycle state machines.
//!
//! This crate contains no sockets, platform capture, codecs, or UI. Callers
//! provide events and execute the typed actions emitted by these state machines.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod host;
mod viewer;

pub use host::{
    bounded_stream_dimensions, capture_retry_delay, CaptureAction, CaptureFailure, EncoderAction,
    EncoderFailure, HostAction, HostConfig, HostPhase, HostSession, NetworkPath, QualityAction,
    RecoveryReason, StreamDimensions, DEFAULT_CONTROL_DISCONNECT_TIMEOUT_US,
    INITIAL_CAPTURE_RETRY_US, MAX_CAPTURE_RETRY_US, MAX_STREAM_HEIGHT_PX, MAX_STREAM_WIDTH_PX,
    MIN_STREAM_HEIGHT_PX,
};
pub use viewer::{
    Backoff, VideoDisposition, ViewerAction, ViewerPhase, ViewerSession,
    INITIAL_RECONNECT_DELAY_US, MAX_RECONNECT_DELAY_US,
};

/// Minimum interval between requests for another keyframe after a loss event.
pub const KEYFRAME_REQUEST_MIN_INTERVAL_US: u64 = 200_000;
/// Stream frame rate fixed by the product requirements.
pub const STREAM_FPS: u8 = 30;

/// A lifecycle event suitable for logging or conversion into telemetry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionEvent {
    /// The control handshake completed successfully.
    Connected,
    /// The control connection was lost and reconnecting began.
    Reconnecting,
    /// A control connection was established after reconnecting.
    Reconnected,
    /// The selected display changed after a stream reset.
    DisplaySelected(racc_topology::DisplayId),
    /// A requested display was removed or became unavailable.
    DisplayUnavailable(racc_topology::DisplayId),
    /// Capture was lost and recovery began.
    CaptureLost(RecoveryReason),
    /// Capture or encoder recovery succeeded.
    RecoverySucceeded,
    /// A decoder reset was requested after a decode failure.
    DecoderReset,
    /// The host network path changed.
    NetworkPathChanged(NetworkPath),
    /// A hardware encoder failure caused software fallback.
    EncoderFallbackToSoftware,
    /// The software encoder rebuild failed and the stream could not continue.
    EncoderFailed,
    /// The control disconnect timeout elapsed and streaming stopped.
    ControlTimedOut,
    /// The session reached a terminal state after Goodbye or a timeout.
    SessionEnded,
}
