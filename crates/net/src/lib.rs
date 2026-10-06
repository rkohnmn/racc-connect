//! Sans-I/O video transport policy and thin TCP/UDP socket shells.
//!
//! The deterministic core receives monotonic microsecond timestamps from its
//! caller. Only the socket wrappers use wall-clock timers and operating-system
//! I/O.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod bind;
mod control;
mod error;
mod loss;
mod sender;
mod video;
mod video_io;

pub use bind::{validate_bind_addr, BindError, BindPolicy};
pub use control::{
    configure_control_stream, connect_control, ControlConn, ControlError, ControlListener,
    ControlReadHalf, ControlSettings, ControlWriteHalf, DEFAULT_CONTROL_READ_TIMEOUT,
    DEFAULT_CONTROL_WRITE_TIMEOUT, DEFAULT_KEEPALIVE_IDLE,
};
pub use error::NetError;
pub use loss::{LossEstimate, LossEstimator, LossSample, LOSS_WINDOW_US, MAX_LOSS_BUCKETS};
pub use sender::{
    ForceKeyframe, FrameQueue, Pacer, PacerSchedule, QueuePush, ScheduledSend, SenderFrame,
};
pub use video::{
    fragment_count, slice_frame_into, FragmentMetadata, FrameData, Reassembler, ReassemblyCounters,
    ReassemblyEvent, ReassemblyReason, ReassemblyStats, SliceError,
    KEYFRAME_REQUEST_MAX_BACKOFF_MS, KEYFRAME_REQUEST_MIN_INTERVAL_MS, MAX_INFLIGHT_BYTES,
    MAX_INFLIGHT_FRAMES, REASSEMBLY_TIMEOUT_MS, REORDER_WINDOW_MS,
};
pub use video_io::{
    Sleeper, ThreadSleeper, VideoReceiver, VideoReceiverCounters, VideoSendMetrics, VideoSender,
    VideoTransportEvent, DEFAULT_FRAME_INTERVAL_US, PACING_FRACTION_PERCENT, RECEIVE_BUFFER_BYTES,
    RECEIVE_POLL_INTERVAL_MS, SEND_BUFFER_BYTES, SEND_QUEUE_MAX_FRAMES,
};

/// Maximum time budget used when joining transport worker threads.
pub const THREAD_JOIN_TIMEOUT_MS: u64 = 1000;
