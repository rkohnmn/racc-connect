//! H.264 encoder contracts, a BSD OpenH264 fallback, and bounded Annex B output.
//!
//! The Windows Media Foundation implementation is isolated in [`windows`]. Synthetic probes
//! generate I420/NV12 test patterns and never acquire desktop content.

#![deny(unsafe_code)]

use std::fmt;
use std::io::{self, Write};

use openh264::encoder::{
    BitRate, Encoder as NativeOpenH264Encoder, EncoderConfig as NativeOpenH264Config, FrameRate,
    Profile, RateControlMode, UsageType, VuiConfig,
};
use openh264::{formats::YUVSource, OpenH264API, Timestamp};

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos;
/// Platform-neutral VideoToolbox AVCC normalization and parameter-set handling.
pub mod macos_avcc;
pub mod pipeline;
pub mod probe;
#[cfg(target_os = "macos")]
pub use macos::VideoToolboxH264Encoder;
#[cfg(windows)]
#[allow(unsafe_code)]
pub mod windows;
#[cfg(windows)]
pub mod windows_capture_probe;
#[cfg(windows)]
#[allow(unsafe_code)]
pub mod windows_software_fallback;
#[cfg(windows)]
#[allow(unsafe_code)]
pub mod windows_video_processor;

/// Highest encoded dimensions supported by the product.
pub const MAX_WIDTH: u32 = 1920;
/// Highest encoded dimensions supported by the product.
pub const MAX_HEIGHT: u32 = 1080;
/// Product frame rate is fixed at 30 fps.
pub const FRAME_RATE: u32 = 30;
/// Defensive limit for one encoded access unit.
pub const MAX_ENCODED_PACKET_BYTES: usize = 16 * 1024 * 1024;
/// Defensive limit for one raw I420 plane set.
pub const MAX_I420_BYTES: usize = (MAX_WIDTH as usize * MAX_HEIGHT as usize * 3) / 2;
/// Maximum synthetic probe duration, in frames.
pub const MAX_PROBE_FRAMES: u32 = 30 * 60;

/// Validated H.264 encoder configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EncoderConfig {
    width: u32,
    height: u32,
    bitrate_bps: u32,
}

impl EncoderConfig {
    /// Creates a configuration within the product's 1080p30 limits.
    pub fn new(width: u32, height: u32, bitrate_bps: u32) -> Result<Self, EncodeError> {
        if width < 2 || height < 2 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(EncodeError::InvalidConfig(
                "width and height must be even and at least two pixels".to_owned(),
            ));
        }
        if width > MAX_WIDTH || height > MAX_HEIGHT {
            return Err(EncodeError::InvalidConfig(format!(
                "resolution {width}x{height} exceeds {MAX_WIDTH}x{MAX_HEIGHT}"
            )));
        }
        if bitrate_bps == 0 || bitrate_bps > 20_000_000 {
            return Err(EncodeError::InvalidConfig(
                "bitrate must be between 1 and 20000000 bits per second".to_owned(),
            ));
        }
        let bytes = usize::try_from(width)
            .ok()
            .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
            .and_then(|pixels| pixels.checked_mul(3))
            .map(|bytes| bytes / 2)
            .ok_or_else(|| EncodeError::InvalidConfig("frame size overflow".to_owned()))?;
        if bytes > MAX_I420_BYTES {
            return Err(EncodeError::InvalidConfig(
                "raw frame exceeds the bounded I420 size".to_owned(),
            ));
        }
        Ok(Self {
            width,
            height,
            bitrate_bps,
        })
    }

    /// Configured frame width.
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Configured frame height.
    pub const fn height(self) -> u32 {
        self.height
    }

    /// Target average bitrate in bits per second.
    pub const fn bitrate_bps(self) -> u32 {
        self.bitrate_bps
    }

    /// Fixed product frame rate.
    pub const fn fps(self) -> u32 {
        FRAME_RATE
    }
}

/// Borrowed planar YUV 4:2:0 (I420) input. No color conversion or frame copy occurs here.
#[derive(Clone, Copy, Debug)]
pub struct I420Frame<'a> {
    width: u32,
    height: u32,
    timestamp_us: u64,
    y: &'a [u8],
    u: &'a [u8],
    v: &'a [u8],
}

impl<'a> I420Frame<'a> {
    /// Validates the planes against dimensions and the maximum product frame size.
    pub fn new(
        width: u32,
        height: u32,
        timestamp_us: u64,
        y: &'a [u8],
        u: &'a [u8],
        v: &'a [u8],
    ) -> Result<Self, EncodeError> {
        let config = EncoderConfig::new(width, height, 1)?;
        let y_len = usize::try_from(config.width)
            .ok()
            .and_then(|w| {
                usize::try_from(config.height)
                    .ok()
                    .and_then(|h| w.checked_mul(h))
            })
            .ok_or_else(|| EncodeError::InvalidFrame("frame size overflow".to_owned()))?;
        let chroma_len = y_len / 4;
        if y.len() != y_len || u.len() != chroma_len || v.len() != chroma_len {
            return Err(EncodeError::InvalidFrame(format!(
                "I420 planes must be Y={y_len}, U={chroma_len}, V={chroma_len} bytes"
            )));
        }
        Ok(Self {
            width,
            height,
            timestamp_us,
            y,
            u,
            v,
        })
    }

    /// Input width.
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Input height.
    pub const fn height(self) -> u32 {
        self.height
    }

    /// Capture timestamp in microseconds.
    pub const fn timestamp_us(self) -> u64 {
        self.timestamp_us
    }
}

impl YUVSource for I420Frame<'_> {
    fn dimensions(&self) -> (usize, usize) {
        (self.width as usize, self.height as usize)
    }

    fn strides(&self) -> (usize, usize, usize) {
        let width = self.width as usize;
        (width, width / 2, width / 2)
    }

    fn y(&self) -> &[u8] {
        self.y
    }

    fn u(&self) -> &[u8] {
        self.u
    }

    fn v(&self) -> &[u8] {
        self.v
    }
}

/// Frame representation accepted by the platform encoder implementations.
pub enum EncoderInput<'a> {
    /// Borrowed planar I420 frame, used by OpenH264 and the portable fake.
    I420(&'a I420Frame<'a>),
    /// GPU-resident NV12 surface, used by the Windows Media Foundation hardware MFT.
    #[cfg(windows)]
    D3D11Nv12(D3D11Nv12Frame<'a>),
    /// Owned NV12 surface from the bounded capture converter; lease is retained through encode.
    #[cfg(windows)]
    PooledNv12(crate::windows_video_processor::Nv12Surface),
    /// Native NV12 CoreVideo input buffer from macOS capture.
    #[cfg(target_os = "macos")]
    MacPixelBuffer(&'a objc2_core_video::CVPixelBuffer, u64),
}

/// Windows D3D11 NV12 input with a timestamp. Construction is explicit at the platform boundary.
#[cfg(windows)]
#[derive(Clone, Copy)]
pub struct D3D11Nv12Frame<'a> {
    /// Texture dimensions.
    pub width: u32,
    /// Texture dimensions.
    pub height: u32,
    /// Presentation timestamp in microseconds.
    pub timestamp_us: u64,
    /// GPU-resident NV12 texture with no CPU access flags.
    pub texture: &'a ::windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
}

/// Which implementation produced an encoded access unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncoderKind {
    /// Hardware Media Foundation transform.
    WindowsMediaFoundationHardware,
    /// Hardware-required H.264 encoding through Apple VideoToolbox.
    MacVideoToolbox,
    /// Cisco OpenH264 BSD software encoder.
    OpenH264,
    /// Deterministic non-production encoder used for tests and fake scenarios.
    Fake,
}

/// One Annex B access unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedPacket {
    /// Annex B bytes with start-code-prefixed NAL units.
    pub bytes: Vec<u8>,
    /// Presentation timestamp in microseconds.
    pub timestamp_us: u64,
    /// True when the Annex B access unit contains an H.264 IDR slice (NAL type 5).
    pub keyframe: bool,
}

/// Encoder failure with backend and bounds errors kept explicit.
#[derive(Debug)]
pub enum EncodeError {
    /// Invalid dimensions, bitrate, or frame rate configuration.
    InvalidConfig(String),
    /// Input planes or texture metadata are invalid or exceed a bound.
    InvalidFrame(String),
    /// A backend does not accept the supplied frame representation.
    UnsupportedInput(&'static str),
    /// No hardware encoder MFT could be activated/configured.
    NoHardwareEncoder(String),
    /// OS, driver, or codec API error.
    Backend(String),
    /// An input surface belongs to a different D3D11 device than the encoder.
    CrossDevice,
    /// All fixed-size converted NV12 surfaces are still in use.
    SurfacePoolExhausted,
    /// Encoded bytes were malformed or exceeded a defensive bound.
    InvalidBitstream(String),
    /// Probe file operation failed.
    Io(io::Error),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(f, "invalid encoder configuration: {message}"),
            Self::InvalidFrame(message) => write!(f, "invalid encoder input: {message}"),
            Self::UnsupportedInput(message) => write!(f, "unsupported encoder input: {message}"),
            Self::NoHardwareEncoder(message) => write!(f, "no hardware H.264 encoder: {message}"),
            Self::Backend(message) => write!(f, "encoder backend failed: {message}"),
            Self::CrossDevice => write!(
                f,
                "capture and encoder textures use different D3D11 devices"
            ),
            Self::SurfacePoolExhausted => {
                write!(f, "all bounded NV12 conversion surfaces are in use")
            }
            Self::InvalidBitstream(message) => write!(f, "invalid H.264 output: {message}"),
            Self::Io(error) => write!(f, "encoder probe file failed: {error}"),
        }
    }
}

impl std::error::Error for EncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for EncodeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// H.264 encoder interface shared by platform and fake implementations.
pub trait Encoder {
    /// Actual implementation selected by the factory or caller.
    fn kind(&self) -> EncoderKind;
    /// Validated active configuration.
    fn config(&self) -> EncoderConfig;
    /// Encodes one input and returns no packet when the backend needs more input.
    fn encode(&mut self, input: EncoderInput<'_>) -> Result<Option<EncodedPacket>, EncodeError>;
    /// Flushes delayed output after the final input; software encoders normally return none.
    fn finish(&mut self) -> Result<Option<EncodedPacket>, EncodeError> {
        Ok(None)
    }
    /// Requests a keyframe when the backend exposes a force-keyframe control.
    fn request_keyframe(&mut self) -> Result<(), EncodeError> {
        Err(EncodeError::UnsupportedInput(
            "this encoder does not expose a force-keyframe control",
        ))
    }
}

/// BSD-licensed OpenH264 software fallback operating on I420 frames.
pub struct OpenH264Encoder {
    config: EncoderConfig,
    encoder: NativeOpenH264Encoder,
}

impl OpenH264Encoder {
    /// Creates a real-time, Main-profile OpenH264 encoder at the configured fixed 30 fps.
    pub fn new(config: EncoderConfig) -> Result<Self, EncodeError> {
        let native_config = NativeOpenH264Config::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(config.bitrate_bps))
            .max_frame_rate(FrameRate::from_hz(FRAME_RATE as f32))
            .profile(Profile::Main)
            .skip_frames(true)
            .intra_frame_period(openh264::encoder::IntraFramePeriod::from_num_frames(
                FRAME_RATE,
            ))
            .vui(VuiConfig::srgb());
        let encoder =
            NativeOpenH264Encoder::with_api_config(OpenH264API::from_source(), native_config)
                .map_err(|error| EncodeError::Backend(format!("initialize OpenH264: {error}")))?;
        Ok(Self { config, encoder })
    }
}

impl Encoder for OpenH264Encoder {
    fn kind(&self) -> EncoderKind {
        EncoderKind::OpenH264
    }

    fn config(&self) -> EncoderConfig {
        self.config
    }

    fn encode(&mut self, input: EncoderInput<'_>) -> Result<Option<EncodedPacket>, EncodeError> {
        let EncoderInput::I420(frame) = input else {
            return Err(EncodeError::UnsupportedInput(
                "OpenH264 requires a GPU-to-I420 software input path",
            ));
        };
        validate_frame_matches_config(frame, self.config)?;
        let timestamp_ms = frame.timestamp_us / 1000;
        let encoded = self
            .encoder
            .encode_at(frame, Timestamp::from_millis(timestamp_ms))
            .map_err(|error| EncodeError::Backend(format!("OpenH264 encode: {error}")))?;

        let mut writer = BoundedVecWriter::new();
        encoded
            .write(&mut writer)
            .map_err(|error| EncodeError::Backend(format!("OpenH264 bitstream: {error}")))?;
        if writer.bytes.is_empty() {
            return Ok(None);
        }
        let bytes = normalize_annex_b(&writer.bytes)?;
        let keyframe = annex_b_contains_idr(&bytes);
        Ok(Some(EncodedPacket {
            bytes,
            timestamp_us: frame.timestamp_us,
            keyframe,
        }))
    }

    fn request_keyframe(&mut self) -> Result<(), EncodeError> {
        self.encoder.force_intra_frame();
        Ok(())
    }
}

/// Deterministic fake encoder for tests and synthetic app scenarios.
pub struct FakeEncoder {
    config: EncoderConfig,
    frame_index: u32,
    force_keyframe: bool,
}

impl FakeEncoder {
    /// Creates a fake encoder with validated bounds.
    pub const fn new(config: EncoderConfig) -> Self {
        Self {
            config,
            frame_index: 0,
            force_keyframe: false,
        }
    }
}

impl Encoder for FakeEncoder {
    fn kind(&self) -> EncoderKind {
        EncoderKind::Fake
    }

    fn config(&self) -> EncoderConfig {
        self.config
    }

    fn encode(&mut self, input: EncoderInput<'_>) -> Result<Option<EncodedPacket>, EncodeError> {
        let (width, height, timestamp_us) = input_dimensions_and_timestamp(&input);
        if width != self.config.width || height != self.config.height {
            return Err(EncodeError::InvalidFrame(
                "input dimensions do not match fake encoder configuration".to_owned(),
            ));
        }
        if let EncoderInput::I420(frame) = input {
            I420Frame::new(
                frame.width,
                frame.height,
                frame.timestamp_us,
                frame.y,
                frame.u,
                frame.v,
            )?;
        }
        self.frame_index = self.frame_index.wrapping_add(1);
        let mut bytes = Vec::with_capacity(16);
        let keyframe = self.frame_index == 1 || self.force_keyframe;
        self.force_keyframe = false;
        bytes.extend_from_slice(&[0, 0, 0, 1, if keyframe { 0x65 } else { 0x41 }]);
        bytes.extend_from_slice(&self.frame_index.to_le_bytes());
        Ok(Some(EncodedPacket {
            bytes,
            timestamp_us,
            keyframe,
        }))
    }

    fn request_keyframe(&mut self) -> Result<(), EncodeError> {
        self.force_keyframe = true;
        Ok(())
    }
}

fn input_dimensions_and_timestamp(input: &EncoderInput<'_>) -> (u32, u32, u64) {
    match input {
        EncoderInput::I420(frame) => (frame.width, frame.height, frame.timestamp_us),
        #[cfg(windows)]
        EncoderInput::D3D11Nv12(frame) => (frame.width, frame.height, frame.timestamp_us),
        #[cfg(windows)]
        EncoderInput::PooledNv12(frame) => (frame.width(), frame.height(), frame.timestamp_us()),
        #[cfg(target_os = "macos")]
        EncoderInput::MacPixelBuffer(frame, timestamp_us) => (
            objc2_core_video::CVPixelBufferGetWidth(frame) as u32,
            objc2_core_video::CVPixelBufferGetHeight(frame) as u32,
            *timestamp_us,
        ),
    }
}

fn validate_frame_matches_config(
    frame: &I420Frame<'_>,
    config: EncoderConfig,
) -> Result<(), EncodeError> {
    I420Frame::new(
        frame.width,
        frame.height,
        frame.timestamp_us,
        frame.y,
        frame.u,
        frame.v,
    )?;
    if frame.width != config.width || frame.height != config.height {
        return Err(EncodeError::InvalidFrame(
            "input dimensions do not match encoder configuration".to_owned(),
        ));
    }
    Ok(())
}

struct BoundedVecWriter {
    bytes: Vec<u8>,
}

impl BoundedVecWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::with_capacity(64 * 1024),
        }
    }
}

impl Write for BoundedVecWriter {
    fn write(&mut self, source: &[u8]) -> io::Result<usize> {
        let next_length = self.bytes.len().checked_add(source.len()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "encoded frame size overflow")
        })?;
        if next_length > MAX_ENCODED_PACKET_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "encoded access unit exceeds the configured bound",
            ));
        }
        self.bytes.extend_from_slice(source);
        Ok(source.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Converts length-prefixed or start-code-prefixed NAL units to a bounded Annex B byte stream.
pub fn normalize_annex_b(input: &[u8]) -> Result<Vec<u8>, EncodeError> {
    if input.is_empty() {
        return Err(EncodeError::InvalidBitstream(
            "empty access unit".to_owned(),
        ));
    }
    if input.len() > MAX_ENCODED_PACKET_BYTES {
        return Err(EncodeError::InvalidBitstream(
            "access unit exceeds the configured bound".to_owned(),
        ));
    }
    if has_annex_b_start_code(input) {
        return Ok(input.to_vec());
    }

    let mut output = Vec::with_capacity(input.len().saturating_add(4 * 16));
    let mut offset = 0usize;
    let mut nal_count = 0usize;
    while offset < input.len() {
        let length_end = offset
            .checked_add(4)
            .ok_or_else(|| EncodeError::InvalidBitstream("NAL length overflow".to_owned()))?;
        let length_bytes = input.get(offset..length_end).ok_or_else(|| {
            EncodeError::InvalidBitstream("truncated NAL length prefix".to_owned())
        })?;
        let nal_len = u32::from_be_bytes([
            length_bytes[0],
            length_bytes[1],
            length_bytes[2],
            length_bytes[3],
        ]) as usize;
        if nal_len == 0 {
            return Err(EncodeError::InvalidBitstream(
                "zero-length NAL unit".to_owned(),
            ));
        }
        let nal_start = length_end;
        let nal_end = nal_start
            .checked_add(nal_len)
            .ok_or_else(|| EncodeError::InvalidBitstream("NAL length overflow".to_owned()))?;
        let nal = input
            .get(nal_start..nal_end)
            .ok_or_else(|| EncodeError::InvalidBitstream("truncated NAL unit".to_owned()))?;
        if output.len().saturating_add(4).saturating_add(nal.len()) > MAX_ENCODED_PACKET_BYTES {
            return Err(EncodeError::InvalidBitstream(
                "normalized access unit exceeds the configured bound".to_owned(),
            ));
        }
        output.extend_from_slice(&[0, 0, 0, 1]);
        output.extend_from_slice(nal);
        nal_count += 1;
        if nal_count > 4096 {
            return Err(EncodeError::InvalidBitstream(
                "NAL unit count exceeds the configured bound".to_owned(),
            ));
        }
        offset = nal_end;
    }
    if output.is_empty() {
        return Err(EncodeError::InvalidBitstream(
            "no NAL units in access unit".to_owned(),
        ));
    }
    Ok(output)
}

fn has_annex_b_start_code(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1])
}

/// Returns true only when an Annex B access unit contains an IDR slice NAL (type 5).
///
/// Media Foundation clean-point metadata is not sufficient to identify an IDR boundary, so
/// callers use this parser on normalized H.264 bytes for keyframe/recovery metadata.
pub(crate) fn annex_b_contains_idr(bytes: &[u8]) -> bool {
    let Some((mut start, mut prefix_len)) = find_annex_b_start_code(bytes, 0) else {
        return false;
    };
    loop {
        let nal_start = start + prefix_len;
        let Some(&header) = bytes.get(nal_start) else {
            return false;
        };
        if header & 0x80 == 0 && header & 0x1f == 5 {
            return true;
        }
        let Some((next_start, next_prefix_len)) = find_annex_b_start_code(bytes, nal_start + 1)
        else {
            return false;
        };
        start = next_start;
        prefix_len = next_prefix_len;
    }
}

fn find_annex_b_start_code(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut index = from;
    while index < bytes.len() {
        let remaining = bytes.len() - index;
        if remaining >= 4 && bytes[index..index + 4] == [0, 0, 0, 1] {
            return Some((index, 4));
        }
        if remaining >= 3 && bytes[index..index + 3] == [0, 0, 1] {
            return Some((index, 3));
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_configuration_enforces_resolution_bitrate_and_even_dimensions() {
        assert!(EncoderConfig::new(1920, 1080, 6_000_000).is_ok());
        assert_eq!(
            EncoderConfig::new(1920, 1082, 6_000_000)
                .err()
                .map(|e| e.to_string()),
            Some(
                "invalid encoder configuration: resolution 1920x1082 exceeds 1920x1080".to_owned()
            )
        );
        assert!(EncoderConfig::new(641, 480, 1_000_000).is_err());
        assert!(EncoderConfig::new(640, 480, 0).is_err());
        assert_eq!(
            EncoderConfig::new(640, 480, 1_000_000)
                .expect("valid configuration")
                .fps(),
            30
        );
    }

    #[test]
    fn i420_input_rejects_truncated_or_oversized_planes() {
        let y = vec![16; 64 * 48];
        let u = vec![128; 32 * 24];
        let v = vec![128; 32 * 24];
        assert!(I420Frame::new(64, 48, 0, &y, &u, &v).is_ok());
        assert!(I420Frame::new(64, 48, 0, &y[..y.len() - 1], &u, &v).is_err());
        let mut overlong = u.clone();
        overlong.push(0);
        assert!(I420Frame::new(64, 48, 0, &y, &overlong, &v).is_err());
    }

    #[test]
    fn idr_detection_requires_nal_type_five() {
        let idr_access_unit = [
            0, 0, 0, 1, 0x67, 0xaa, // SPS
            0, 0, 1, 0x68, 0xbb, // PPS
            0, 0, 0, 1, 0x65, 0xcc, // IDR slice
        ];
        let non_idr_access_unit = [
            0, 0, 0, 1, 0x67, 0xaa, // SPS
            0, 0, 1, 0x68, 0xbb, // PPS
            0, 0, 0, 1, 0x61, 0xcc, // non-IDR slice, type 1
        ];
        assert!(annex_b_contains_idr(&idr_access_unit));
        assert!(!annex_b_contains_idr(&non_idr_access_unit));
        assert!(!annex_b_contains_idr(&[0, 0, 0, 1, 0x41]));
    }

    #[test]
    fn normalizes_length_prefixed_nals_and_rejects_hostile_lengths() {
        let input = [0, 0, 0, 2, 0x65, 0xaa, 0, 0, 0, 1, 0x41];
        assert_eq!(
            normalize_annex_b(&input).expect("valid length-prefixed stream"),
            [0, 0, 0, 1, 0x65, 0xaa, 0, 0, 0, 1, 0x41]
        );
        assert!(normalize_annex_b(&[0, 0, 0]).is_err());
        assert!(normalize_annex_b(&[0, 0, 0, 4, 0x65]).is_err());
        assert!(normalize_annex_b(&[0, 0, 0, 0]).is_err());
        assert_eq!(
            normalize_annex_b(&[0, 0, 0, 1, 0x65]).expect("already Annex B"),
            [0, 0, 0, 1, 0x65]
        );
    }

    #[test]
    fn fake_encoder_returns_bounded_annex_b_access_unit() {
        let config = EncoderConfig::new(64, 48, 1_000_000).expect("valid config");
        let mut fake = FakeEncoder::new(config);
        let y = vec![16; 64 * 48];
        let u = vec![128; 32 * 24];
        let v = vec![128; 32 * 24];
        let frame = I420Frame::new(64, 48, 33_333, &y, &u, &v).expect("valid input");
        let packet = fake
            .encode(EncoderInput::I420(&frame))
            .expect("fake encode")
            .expect("packet");
        assert!(packet.bytes.starts_with(&[0, 0, 0, 1]));
        assert!(packet.keyframe);
        assert_eq!(packet.timestamp_us, 33_333);
    }

    #[test]
    fn openh264_keyframe_request_forces_an_idr_and_metadata_tracks_idr_only() {
        let config = EncoderConfig::new(64, 48, 500_000).expect("valid config");
        let mut encoder = OpenH264Encoder::new(config).expect("OpenH264 initializes");
        let y = vec![96; 64 * 48];
        let u = vec![128; 64 * 48 / 4];
        let v = vec![128; 64 * 48 / 4];

        let first = I420Frame::new(64, 48, 0, &y, &u, &v).expect("first frame");
        let first_packet = encoder
            .encode(EncoderInput::I420(&first))
            .expect("first encode")
            .expect("first access unit");
        assert!(first_packet.keyframe, "OpenH264 starts with an IDR");

        let second = I420Frame::new(64, 48, 33_333, &y, &u, &v).expect("second frame");
        let second_packet = encoder
            .encode(EncoderInput::I420(&second))
            .expect("second encode")
            .expect("second access unit");
        assert!(!second_packet.keyframe, "a predictive frame is not an IDR");

        encoder.request_keyframe().expect("force next IDR");
        let third = I420Frame::new(64, 48, 66_666, &y, &u, &v).expect("third frame");
        let third_packet = encoder
            .encode(EncoderInput::I420(&third))
            .expect("forced encode")
            .expect("forced access unit");
        assert!(third_packet.keyframe, "the requested frame is an IDR");
    }

    #[test]
    fn openh264_encodes_synthetic_planar_frames_to_annex_b() {
        let config = EncoderConfig::new(64, 48, 500_000).expect("valid config");
        let mut encoder = OpenH264Encoder::new(config).expect("OpenH264 initialization");
        let y = vec![90; 64 * 48];
        let u = vec![128; 32 * 24];
        let v = vec![128; 32 * 24];
        let frame = I420Frame::new(64, 48, 0, &y, &u, &v).expect("valid input");
        let packet = encoder
            .encode(EncoderInput::I420(&frame))
            .expect("encode")
            .expect("IDR packet");
        assert!(packet.bytes.starts_with(&[0, 0, 0, 1]) || packet.bytes.starts_with(&[0, 0, 1]));
        assert!(packet.keyframe);
        assert!(packet.bytes.len() < MAX_ENCODED_PACKET_BYTES);
    }
}
