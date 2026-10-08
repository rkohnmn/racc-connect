//! Deterministic host and viewer session lifecycle state machines.
//!
//! This crate contains no sockets, platform capture, codecs, or UI. Callers
//! provide events and execute the typed actions emitted by these state machines.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod host;
mod quality;
mod viewer;

#[cfg(test)]
mod scenarios;

pub use host::{
    bounded_stream_dimensions, bounded_stream_dimensions_with_height, capture_retry_delay,
    choose_stream_params, CaptureAction, CaptureFailure, EncoderAction, EncoderFailure, HostAction,
    HostConfig, HostPhase, HostSession, HostStreamCaps, NetworkPath, QualityAction, RecoveryReason,
    StreamDimensions, StreamParams, CAPTURE_RECOVERY_BACKOFF_US,
    DEFAULT_CONTROL_DISCONNECT_TIMEOUT_US, ENCODER_FAILURE_WINDOW_US, FORCE_IDR_MIN_INTERVAL_US,
    INITIAL_CAPTURE_RETRY_US, MAX_CAPTURE_RETRY_US, MAX_STREAM_HEIGHT_PX, MAX_STREAM_WIDTH_PX,
    MIN_STREAM_HEIGHT_PX,
};
pub use quality::{
    QualityChangeReason, QualityConfigError, QualityController, QualityEvent, QualityFeedback,
    QualityPreference, QualityTier, BITRATE_1080_BPS, BITRATE_480_BPS, BITRATE_720_BPS,
    QUALITY_BITRATE_TRIM_INTERVAL_US, QUALITY_BITRATE_TRIM_STEP_PERCENT,
    QUALITY_FRAME_LOSS_THRESHOLD_BPS, QUALITY_LOSS_DWELL_US, QUALITY_LOSS_THRESHOLD_BPS,
    QUALITY_MAX_HEIGHT, QUALITY_MIN_BITRATE_PERCENT, QUALITY_MIN_HEIGHT,
    QUALITY_OVERFLOW_WINDOW_US, QUALITY_RTT_DWELL_US, QUALITY_STABLE_DWELL_US,
    QUALITY_STABLE_LOSS_LIMIT_BPS, QUALITY_STEP_UP_INTERVAL_US,
};
pub use viewer::{
    Backoff, VideoDisposition, ViewerAction, ViewerPhase, ViewerSession, BUSY_RETRY_DELAY_MS,
    BUSY_RETRY_DELAY_US, FIRST_KEYFRAME_NUDGE_US, FIRST_KEYFRAME_RESET_US, HANDSHAKE_TIMEOUT_US,
    INITIAL_RECONNECT_DELAY_US, MAX_RECONNECT_DELAY_US, RECONNECT_BACKOFF_US, SWITCH_TIMEOUT_US,
};

/// Viewer and host heartbeat interval, in milliseconds.
pub const HEARTBEAT_INTERVAL_MS: u64 = 1_000;
/// Viewer and host control-silence deadline, in milliseconds.
pub const HEARTBEAT_TIMEOUT_MS: u64 = 5_000;
/// Viewer handshake deadline, in milliseconds.
pub const HANDSHAKE_TIMEOUT_MS: u64 = 5_000;
/// Switch reply timeout, in milliseconds.
pub const SWITCH_TIMEOUT_MS: u64 = 2_000;
/// First-keyframe recovery nudge delay, in milliseconds.
pub const FIRST_KEYFRAME_NUDGE_MS: u64 = 1_000;
/// First-keyframe decoder reset delay, in milliseconds.
pub const FIRST_KEYFRAME_RESET_MS: u64 = 5_000;
/// Viewer reconnect schedule; its final value repeats, in milliseconds.
pub const RECONNECT_BACKOFF_MS: [u64; 5] = [500, 1_000, 2_000, 4_000, 5_000];
/// Host's minimum spacing between IDR requests, in milliseconds.
pub const FORCE_IDR_MIN_INTERVAL_MS: u64 = 100;
/// Capture retry schedule; its final value repeats, in milliseconds.
pub const CAPTURE_RECOVERY_BACKOFF_MS: [u64; 6] = [50, 100, 200, 400, 800, 1_000];
/// Owner-selected host orphan timeout, overriding the M3b 10-second default.
pub const HOST_ORPHAN_TIMEOUT_MS: u64 = 5_000;
/// Encoder failures required before software fallback.
pub const ENCODER_FAILURE_THRESHOLD: usize = 3;
/// Rolling encoder failure window, in milliseconds.
pub const ENCODER_FAILURE_WINDOW_MS: u64 = 10_000;

/// Shared timer constants converted to the session API's microseconds.
pub const HEARTBEAT_INTERVAL_US: u64 = HEARTBEAT_INTERVAL_MS * 1_000;
/// Shared heartbeat silence limit in microseconds.
pub const HEARTBEAT_TIMEOUT_US: u64 = HEARTBEAT_TIMEOUT_MS * 1_000;
/// Host stream stop deadline in microseconds.
pub const HOST_ORPHAN_TIMEOUT_US: u64 = HOST_ORPHAN_TIMEOUT_MS * 1_000;
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
