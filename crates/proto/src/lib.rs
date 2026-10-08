#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! Bounded wire types and serialization for the Racc Connect protocol.
//!
//! This crate performs no I/O and contains no platform-specific code. Decoders
//! validate all lengths and values before constructing owned protocol data.

mod codec;
mod error;
mod framing;
mod messages;
mod video;

pub use error::{ProtoError, ProtoResult};
pub use framing::{decode_control_frame, FrameDecoder};
pub use messages::{
    CaptureBackend, ClipboardOrigin, ClipboardSyncControl, ClipboardUpdate, ControlMessage,
    ControlPayload, CursorBlendMode, CursorShape, DisplayInfo, Encoder, Goodbye, GoodbyeReason,
    Hello, HelloAck, HelloStatus, HostEventKind, HostEventReport, InputEvent, InputEventKind,
    LogicalClock, OsType, PauseVideo, Ping, Pong, QualityAdjustment, QualityAdjustmentReason,
    RequestKeyframe, ResumeVideo, SetQuality, StatsReport, StreamCodec, StreamReset, StreamStatus,
    SwitchMonitor, TopologyAnnounce, ViewerReport,
};
pub use video::{
    encode_cursor_datagram, encode_video_datagram, parse_cursor_datagram, parse_video_datagram,
    CursorUpdate, VideoDatagram, VideoHeader, VIDEO_FLAG_CONFIG, VIDEO_FLAG_KEY,
    VIDEO_FLAG_LAST_FRAGMENT,
};

/// Current wire protocol version.
pub const PROTOCOL_VERSION: u8 = 5;
/// Hello/HelloAck feature bit for text-only clipboard synchronization.
pub const FEATURE_TEXT_CLIPBOARD: u32 = 1 << 0;
/// Maximum size of a video or cursor UDP datagram, including its header.
pub const MAX_DATAGRAM: usize = 1200;
/// Size in bytes of the video-slice datagram header.
pub const VIDEO_HEADER_LEN: usize = 18;
/// Maximum video payload in one datagram.
pub const MAX_VIDEO_PAYLOAD: usize = MAX_DATAGRAM - VIDEO_HEADER_LEN;
/// Maximum fragments allowed in one encoded frame.
pub const MAX_FRAGMENTS_PER_FRAME: usize = 1024;
/// Maximum TCP control frame body size, including the type byte but excluding its u32 prefix.
pub const MAX_CONTROL_FRAME_BYTES: usize = 1_048_576;
/// Maximum UTF-8 clipboard text payload in bytes.
pub const MAX_CLIPBOARD_BYTES: usize = 524_288;
/// Current logical-clock schema version embedded in clipboard updates.
pub const CLIPBOARD_LOGICAL_CLOCK_VERSION: u8 = 1;
/// Maximum clipboard Lamport counter (positive signed 64-bit range).
pub const MAX_CLIPBOARD_LOGICAL_CLOCK: u64 = i64::MAX as u64;
/// Maximum viewer-reported RTT and p95 decode duration in milliseconds.
pub const MAX_VIEWER_REPORT_DURATION_MS: u32 = 60_000;
/// Maximum dropped-frame count accepted in one viewer report.
pub const MAX_VIEWER_REPORT_DROPPED_FRAMES: u32 = 1_000_000;
/// Maximum displays in a topology announcement.
pub const MAX_DISPLAYS: usize = 16;
/// Maximum UTF-8 string size for protocol names and versions.
pub const MAX_NAME_BYTES: usize = 128;
/// Maximum cursor bitmap width or height in pixels.
pub const MAX_CURSOR_DIM: usize = 128;
/// Maximum BGRA cursor bitmap size in bytes.
pub const MAX_CURSOR_BYTES: usize = 65_536;

const _: () = assert!(MAX_VIDEO_PAYLOAD == MAX_DATAGRAM - VIDEO_HEADER_LEN);
const _: () = assert!(VIDEO_HEADER_LEN == 18);
const _: () = assert!(MAX_VIDEO_PAYLOAD == 1182);
