//! H.264 decoder contracts, bounded frame data, and deterministic test backends.
//!
//! Decoders accept complete Annex B access units after network reassembly. They
//! never perform network I/O and return owned NV12 planes for a render handoff.

#![deny(unsafe_code)]
#![warn(missing_docs)]

use std::fmt;

use racc_proto::{StreamCodec, StreamReset, StreamStatus};

mod fake;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos;
/// Portable AVCC conversion and parameter-set cache for VideoToolbox integration.
pub mod macos_bitstream;
mod queue;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

pub use fake::FakeDecoder;
#[cfg(target_os = "macos")]
pub use macos::VideoToolboxDecoder;
/// VideoToolbox sample helper and per-stream parameter-set cache.
pub use macos_bitstream::{VideoToolboxBitstream, VideoToolboxSample};
pub use queue::{DecoderInputQueue, QueuePush, DECODER_INPUT_QUEUE_CAPACITY};
#[cfg(windows)]
pub use windows::MediaFoundationDecoder;

/// Maximum width accepted from a stream reset.
pub const MAX_DECODE_WIDTH: u32 = 1920;
/// Maximum height accepted from a stream reset.
pub const MAX_DECODE_HEIGHT: u32 = 1080;
/// Maximum encoded H.264 access unit accepted by this crate (8 MiB).
pub const MAX_ENCODED_ACCESS_UNIT_BYTES: usize = 8 * 1024 * 1024;
/// Maximum NV12 frame size accepted by this crate (Y plus interleaved UV).
pub const MAX_NV12_FRAME_BYTES: usize =
    (MAX_DECODE_WIDTH as usize) * (MAX_DECODE_HEIGHT as usize) * 3 / 2;
const MAX_NAL_UNITS: usize = 4096;

/// Decoder implementation selected by a platform factory or test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoderKind {
    /// Deterministic test decoder; it does not decode H.264 pixels.
    Fake,
    /// Generic Windows Media Foundation decoder kind for older consumers.
    MediaFoundation,
    /// MFT selected from Media Foundation hardware registrations; selection does not prove DXVA.
    WindowsMediaFoundationHardwareMft,
    /// Synchronous Media Foundation MFT selected as the fallback decoder.
    WindowsMediaFoundationSynchronousMft,
    /// macOS VideoToolbox H.264 decoder.
    VideoToolbox,
}

/// Typed decoder configuration, bitstream, queue, and backend failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The reset is unsupported or contains invalid stream dimensions or rate.
    InvalidConfig(&'static str),
    /// An access unit is empty, oversized, malformed, or disagrees with its flags.
    InvalidBitstream(&'static str),
    /// An encoded frame has invalid dimensions or plane lengths.
    InvalidFrame(&'static str),
    /// The access unit belongs to another stream epoch.
    WrongEpoch {
        /// Epoch currently configured in the decoder.
        expected: u16,
        /// Epoch carried by the rejected access unit.
        received: u16,
    },
    /// The access unit is not newer than the most recently accepted frame.
    OutOfOrderFrame {
        /// Most recently accepted frame identifier.
        previous: u32,
        /// Rejected frame identifier.
        received: u32,
    },
    /// Predictive frames are ignored until an IDR access unit arrives.
    AwaitingKeyframe,
    /// No SPS/PPS codec configuration has been received for this reset.
    MissingCodecConfiguration,
    /// A backend failed while configuring or decoding.
    Backend(String),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(reason) => {
                write!(formatter, "invalid decoder configuration: {reason}")
            }
            Self::InvalidBitstream(reason) => {
                write!(formatter, "invalid H.264 access unit: {reason}")
            }
            Self::InvalidFrame(reason) => write!(formatter, "invalid decoded NV12 frame: {reason}"),
            Self::WrongEpoch { expected, received } => {
                write!(
                    formatter,
                    "access unit epoch {received} does not match decoder epoch {expected}"
                )
            }
            Self::OutOfOrderFrame { previous, received } => {
                write!(
                    formatter,
                    "access unit frame {received} is not newer than {previous}"
                )
            }
            Self::AwaitingKeyframe => formatter.write_str("decoder is waiting for an IDR keyframe"),
            Self::MissingCodecConfiguration => {
                formatter.write_str("H.264 SPS/PPS are required before the first IDR")
            }
            Self::Backend(message) => write!(formatter, "decoder backend failed: {message}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// One complete, bounded H.264 Annex B access unit with transport metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedAccessUnit {
    /// Stream epoch carried by the video datagram header.
    pub epoch: u16,
    /// Monotonic frame identifier within the epoch.
    pub frame_id: u32,
    /// Host capture timestamp in microseconds, wrapping at u32.
    pub capture_ts_us: u32,
    /// True when the transport marked this unit as containing codec config.
    pub config: bool,
    /// Validated Annex B H.264 bytes.
    pub bytes: Vec<u8>,
    keyframe: bool,
    parameter_sets: bool,
    has_vcl: bool,
}

impl EncodedAccessUnit {
    /// Validates a complete Annex B access unit and its keyframe/config metadata.
    pub fn new(
        epoch: u16,
        frame_id: u32,
        capture_ts_us: u32,
        keyframe: bool,
        config: bool,
        bytes: Vec<u8>,
    ) -> Result<Self, DecodeError> {
        if bytes.is_empty() {
            return Err(DecodeError::InvalidBitstream("empty access unit"));
        }
        if bytes.len() > MAX_ENCODED_ACCESS_UNIT_BYTES {
            return Err(DecodeError::InvalidBitstream("access unit exceeds 8 MiB"));
        }
        let summary = summarize_annex_b(&bytes)?;
        if keyframe != summary.idr {
            return Err(DecodeError::InvalidBitstream(
                "transport keyframe flag disagrees with IDR NAL units",
            ));
        }
        if config && !summary.has_parameter_sets() {
            return Err(DecodeError::InvalidBitstream(
                "config flag is set without both SPS and PPS",
            ));
        }
        Ok(Self {
            epoch,
            frame_id,
            capture_ts_us,
            config,
            bytes,
            keyframe: summary.idr,
            parameter_sets: summary.has_parameter_sets(),
            has_vcl: summary.vcl,
        })
    }

    /// Whether the bytes contain an actual H.264 IDR slice.
    pub const fn is_keyframe(&self) -> bool {
        self.keyframe
    }

    /// Whether both SPS and PPS parameter sets occur in this access unit.
    pub const fn has_parameter_sets(&self) -> bool {
        self.parameter_sets
    }

    /// Whether the access unit contains a coded slice that can produce a picture.
    pub const fn has_vcl(&self) -> bool {
        self.has_vcl
    }
}

/// Decoded Y plane and interleaved UV plane of one NV12 image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedFrame {
    /// Stream epoch used to reject stale output during a display switch.
    pub epoch: u16,
    /// Monotonic frame identifier within the epoch.
    pub frame_id: u32,
    /// Host capture timestamp in microseconds, wrapping at u32.
    pub capture_ts_us: u32,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Luma plane, exactly `width * height` bytes.
    pub y: Vec<u8>,
    /// Interleaved U/V plane, exactly `width * height / 2` bytes.
    pub uv: Vec<u8>,
}

impl DecodedFrame {
    /// Creates an NV12 frame after checking dimensions, allocation bounds, and plane lengths.
    pub fn new(
        epoch: u16,
        frame_id: u32,
        capture_ts_us: u32,
        width: u32,
        height: u32,
        y: Vec<u8>,
        uv: Vec<u8>,
    ) -> Result<Self, DecodeError> {
        let (y_len, uv_len) = nv12_lengths(width, height)?;
        if y.len() != y_len || uv.len() != uv_len {
            return Err(DecodeError::InvalidFrame("NV12 plane length mismatch"));
        }
        Ok(Self {
            epoch,
            frame_id,
            capture_ts_us,
            width,
            height,
            y,
            uv,
        })
    }
}

/// H.264 decoder interface for platform and fake implementations.
pub trait Decoder {
    /// Identifies the selected backend for telemetry.
    fn kind(&self) -> DecoderKind;
    /// Applies a stream reset and discards prior-epoch codec state.
    fn configure(&mut self, reset: StreamReset) -> Result<(), DecodeError>;
    /// Decodes one complete access unit; returns `None` for config-only units.
    fn decode(
        &mut self,
        access_unit: &EncodedAccessUnit,
    ) -> Result<Option<DecodedFrame>, DecodeError>;
    /// Releases backend state where supported. The default is a no-op.
    fn flush(&mut self) -> Result<(), DecodeError> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct NalSummary {
    idr: bool,
    sps: bool,
    pps: bool,
    vcl: bool,
}

impl NalSummary {
    fn has_parameter_sets(self) -> bool {
        self.sps && self.pps
    }
}

fn summarize_annex_b(bytes: &[u8]) -> Result<NalSummary, DecodeError> {
    let Some((first_start, first_prefix)) = find_start_code(bytes, 0) else {
        return Err(DecodeError::InvalidBitstream(
            "Annex B start code is missing",
        ));
    };
    if first_start != 0 {
        return Err(DecodeError::InvalidBitstream(
            "bytes precede the first start code",
        ));
    }

    let mut summary = NalSummary::default();
    let mut current_start = first_start;
    let mut current_prefix = first_prefix;
    let mut nal_count = 0usize;
    loop {
        let Some(nal_start) = current_start.checked_add(current_prefix) else {
            return Err(DecodeError::InvalidBitstream("NAL offset overflow"));
        };
        let next = find_start_code(bytes, nal_start.saturating_add(1));
        let mut nal_end = next.map_or(bytes.len(), |(start, _)| start);
        while nal_end > nal_start && bytes.get(nal_end - 1) == Some(&0) {
            nal_end -= 1;
        }
        let Some(&header) = bytes.get(nal_start).filter(|_| nal_start < nal_end) else {
            return Err(DecodeError::InvalidBitstream("empty NAL unit"));
        };
        if header & 0x80 != 0 {
            return Err(DecodeError::InvalidBitstream("forbidden_zero_bit is set"));
        }
        match header & 0x1f {
            1..=5 => {
                summary.vcl = true;
                if header & 0x1f == 5 {
                    summary.idr = true;
                }
            }
            7 => summary.sps = true,
            8 => summary.pps = true,
            _ => {}
        }
        nal_count = nal_count.saturating_add(1);
        if nal_count > MAX_NAL_UNITS {
            return Err(DecodeError::InvalidBitstream("NAL count exceeds 4096"));
        }
        let Some((next_start, next_prefix)) = next else {
            break;
        };
        current_start = next_start;
        current_prefix = next_prefix;
    }
    Ok(summary)
}

fn find_start_code(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut index = from;
    while index < bytes.len() {
        if bytes.get(index..index.saturating_add(4)) == Some(&[0, 0, 0, 1]) {
            return Some((index, 4));
        }
        if bytes.get(index..index.saturating_add(3)) == Some(&[0, 0, 1]) {
            return Some((index, 3));
        }
        index = index.saturating_add(1);
    }
    None
}

fn validate_reset(reset: StreamReset) -> Result<(), DecodeError> {
    if reset.codec != StreamCodec::H264 {
        return Err(DecodeError::InvalidConfig("only H.264 is supported"));
    }
    if reset.status != StreamStatus::Ok {
        return Err(DecodeError::InvalidConfig(
            "stream reset did not start an active stream",
        ));
    }
    let width = u32::from(reset.width);
    let height = u32::from(reset.height);
    if reset.fps != 30 {
        return Err(DecodeError::InvalidConfig("frame rate must be 30 fps"));
    }
    let _ = nv12_lengths(width, height)?;
    Ok(())
}

fn nv12_lengths(width: u32, height: u32) -> Result<(usize, usize), DecodeError> {
    if width < 16 || height < 16 || width > MAX_DECODE_WIDTH || height > MAX_DECODE_HEIGHT {
        return Err(DecodeError::InvalidFrame(
            "dimensions are outside 16x16 through 1920x1080",
        ));
    }
    if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(DecodeError::InvalidFrame("NV12 dimensions must be even"));
    }
    let y_len = usize::try_from(width)
        .ok()
        .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
        .ok_or(DecodeError::InvalidFrame("plane size overflow"))?;
    let uv_len = y_len / 2;
    if y_len.saturating_add(uv_len) > MAX_NV12_FRAME_BYTES {
        return Err(DecodeError::InvalidFrame("NV12 frame exceeds 1920x1080"));
    }
    Ok((y_len, uv_len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset(epoch: u16, width: u16, height: u16) -> StreamReset {
        StreamReset {
            req_id: 0,
            epoch,
            codec: StreamCodec::H264,
            width,
            height,
            fps: 30,
            topology_rev: 1,
            display_id: 1,
            status: StreamStatus::Ok,
        }
    }

    #[test]
    fn stream_reset_enforces_h264_dimensions_and_fixed_rate() {
        let mut decoder = FakeDecoder::new();
        assert_eq!(decoder.configure(reset(4, 64, 48)), Ok(()));
        assert_eq!(
            decoder.configure(reset(5, 641, 480)),
            Err(DecodeError::InvalidFrame("NV12 dimensions must be even"))
        );
        let mut wrong_rate = reset(5, 640, 480);
        wrong_rate.fps = 60;
        assert_eq!(
            decoder.configure(wrong_rate),
            Err(DecodeError::InvalidConfig("frame rate must be 30 fps"))
        );
        let mut failed = reset(5, 640, 480);
        failed.status = StreamStatus::Paused;
        assert_eq!(
            decoder.configure(failed),
            Err(DecodeError::InvalidConfig(
                "stream reset did not start an active stream"
            ))
        );
    }

    #[test]
    fn access_unit_validation_checks_annex_b_flags_and_bounds() {
        assert!(EncodedAccessUnit::new(1, 1, 0, true, false, vec![0, 0, 1, 0x41]).is_err());
        assert!(EncodedAccessUnit::new(1, 1, 0, false, false, vec![0x65]).is_err());
        assert!(EncodedAccessUnit::new(1, 1, 0, true, false, vec![0, 0, 1, 0xe5]).is_err());
        assert!(EncodedAccessUnit::new(1, 1, 0, true, true, vec![0, 0, 1, 0x65]).is_err());
    }

    #[test]
    fn nv12_frame_constructor_rejects_oversize_and_mismatched_planes() {
        assert!(DecodedFrame::new(1, 1, 0, 64, 48, vec![0; 64 * 48], vec![0; 64 * 24]).is_ok());
        assert!(
            DecodedFrame::new(1, 1, 0, 64, 48, vec![0; 64 * 48 - 1], vec![0; 64 * 24]).is_err()
        );
        assert!(DecodedFrame::new(1, 1, 0, 1922, 1080, vec![], vec![]).is_err());
    }
}
