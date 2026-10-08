//! Synthetic encode-to-file probes. These helpers generate test patterns and never capture a screen.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::{
    normalize_annex_b, EncodeError, EncodedPacket, Encoder, EncoderConfig, EncoderInput,
    EncoderKind, I420Frame, OpenH264Encoder, MAX_ENCODED_PACKET_BYTES, MAX_PROBE_FRAMES,
};

/// Summary of a synthetic H.264 elementary-stream probe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbeReport {
    /// Output `.h264` path.
    pub path: PathBuf,
    /// Backend that actually encoded frames.
    pub backend: EncoderKind,
    /// Whether the requested hardware encoder was replaced by software fallback.
    pub used_fallback: bool,
    /// Reason for selecting software fallback, when one was used.
    pub fallback_reason: Option<String>,
    /// Encoded dimensions.
    pub width: u32,
    /// Encoded dimensions.
    pub height: u32,
    /// Product frame rate.
    pub fps: u32,
    /// Requested synthetic input count.
    pub frames_requested: u32,
    /// Input frames that produced a non-empty access unit.
    pub frames_encoded: u32,
    /// Bytes written to the Annex B stream.
    pub bytes_written: u64,
}

/// Runs the BSD OpenH264 fallback against a synthetic I420 pattern and writes raw Annex B H.264.
pub fn run_openh264_synthetic_probe(
    path: impl AsRef<Path>,
    config: EncoderConfig,
    frame_count: u32,
) -> Result<ProbeReport, EncodeError> {
    validate_frame_count(frame_count)?;
    let mut encoder = OpenH264Encoder::new(config)?;
    run_i420_probe(path.as_ref(), config, frame_count, &mut encoder)
}

fn run_i420_probe(
    path: &Path,
    config: EncoderConfig,
    frame_count: u32,
    encoder: &mut dyn Encoder,
) -> Result<ProbeReport, EncodeError> {
    let mut output = AnnexBFile::create(path)?;
    encoder.request_keyframe()?;
    let width = config.width() as usize;
    let height = config.height() as usize;
    let y_len = width
        .checked_mul(height)
        .ok_or_else(|| EncodeError::InvalidFrame("pattern size overflow".to_owned()))?;
    let c_len = y_len / 4;
    let mut y = vec![0u8; y_len];
    let mut u = vec![0u8; c_len];
    let mut v = vec![0u8; c_len];
    let mut frames_encoded = 0u32;

    for frame_index in 0..frame_count {
        fill_i420_test_pattern(width, height, frame_index, &mut y, &mut u, &mut v);
        let timestamp_us = u64::from(frame_index) * 1_000_000 / u64::from(config.fps());
        let frame = I420Frame::new(config.width(), config.height(), timestamp_us, &y, &u, &v)?;
        if let Some(packet) = encoder.encode(EncoderInput::I420(&frame))? {
            output.write_packet(&packet)?;
            frames_encoded = frames_encoded.saturating_add(1);
        }
    }
    let bytes_written = output.finish()?;
    if frames_encoded == 0 || bytes_written == 0 {
        return Err(EncodeError::InvalidBitstream(
            "probe encoder produced no access units".to_owned(),
        ));
    }
    Ok(ProbeReport {
        path: path.to_path_buf(),
        backend: encoder.kind(),
        used_fallback: false,
        fallback_reason: None,
        width: config.width(),
        height: config.height(),
        fps: config.fps(),
        frames_requested: frame_count,
        frames_encoded,
        bytes_written,
    })
}

/// Maximum total size accepted for a probe output file (512 MiB).
///
/// This bound applies to synthetic and real-capture probes.
pub const MAX_PROBE_FILE_BYTES: u64 = 512 * 1024 * 1024;

/// Output writer shared with the Windows hardware probe.
pub struct AnnexBFile {
    writer: BufWriter<File>,
    bytes_written: u64,
}

impl AnnexBFile {
    /// Creates a new bounded Annex B output file with a `.h264` extension.
    pub fn create(path: &Path) -> Result<Self, EncodeError> {
        validate_probe_path(path)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        Ok(Self {
            writer: BufWriter::with_capacity(256 * 1024, file),
            bytes_written: 0,
        })
    }

    /// Appends one bounded Annex B access unit.
    pub fn write_packet(&mut self, packet: &EncodedPacket) -> Result<(), EncodeError> {
        if packet.bytes.is_empty() || packet.bytes.len() > MAX_ENCODED_PACKET_BYTES {
            return Err(EncodeError::InvalidBitstream(
                "probe access unit is empty or oversized".to_owned(),
            ));
        }
        let bytes =
            if packet.bytes.starts_with(&[0, 0, 1]) || packet.bytes.starts_with(&[0, 0, 0, 1]) {
                packet.bytes.as_slice()
            } else {
                let normalized = normalize_annex_b(&packet.bytes)?;
                return self.write_normalized(&normalized);
            };
        self.write_normalized(bytes)
    }

    fn write_normalized(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
        let next = self
            .bytes_written
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| {
                EncodeError::InvalidBitstream("probe output size overflow".to_owned())
            })?;
        let maximum = MAX_PROBE_FILE_BYTES;
        if next > maximum {
            return Err(EncodeError::InvalidBitstream(
                "probe output exceeds the configured bound".to_owned(),
            ));
        }
        self.writer.write_all(bytes)?;
        self.bytes_written = next;
        Ok(())
    }

    /// Flushes and syncs the completed file and returns its byte count.
    pub fn finish(mut self) -> Result<u64, EncodeError> {
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        Ok(self.bytes_written)
    }
}

pub(crate) fn validate_frame_count(frame_count: u32) -> Result<(), EncodeError> {
    if frame_count == 0 || frame_count > MAX_PROBE_FRAMES {
        return Err(EncodeError::InvalidConfig(format!(
            "probe frame count must be 1..={MAX_PROBE_FRAMES}"
        )));
    }
    Ok(())
}

fn validate_probe_path(path: &Path) -> Result<(), EncodeError> {
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("h264"))
    {
        return Err(EncodeError::InvalidConfig(
            "probe output path must end in .h264".to_owned(),
        ));
    }
    Ok(())
}

fn fill_i420_test_pattern(
    width: usize,
    height: usize,
    frame_index: u32,
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
) {
    const LUMA_BARS: [u8; 8] = [235, 210, 170, 145, 106, 81, 41, 16];
    const U_BARS: [u8; 8] = [128, 16, 166, 54, 202, 90, 240, 128];
    const V_BARS: [u8; 8] = [128, 146, 16, 34, 222, 240, 110, 128];
    let band_width = (width / LUMA_BARS.len()).max(1);
    for row in 0..height {
        for column in 0..width {
            let band = (column / band_width).min(LUMA_BARS.len() - 1);
            y[row * width + column] = LUMA_BARS[band];
        }
    }
    let chroma_width = width / 2;
    let chroma_height = height / 2;
    let chroma_band_width = (chroma_width / LUMA_BARS.len()).max(1);
    for row in 0..chroma_height {
        for column in 0..chroma_width {
            let band = (column / chroma_band_width).min(LUMA_BARS.len() - 1);
            let offset = row * chroma_width + column;
            u[offset] = U_BARS[band];
            v[offset] = V_BARS[band];
        }
    }
    let marker_width = (width / 12).max(2);
    let marker_height = (height / 16).max(2);
    let marker_x = (frame_index as usize * width / 60) % width.saturating_sub(marker_width).max(1);
    let marker_y = height.saturating_sub(marker_height + 4);
    for row in marker_y..(marker_y + marker_height).min(height) {
        for column in marker_x..(marker_x + marker_width).min(width) {
            y[row * width + column] = 235;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FakeEncoder, MAX_PROBE_FRAMES};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("racc-{name}-{}-{nonce}.h264", std::process::id()))
    }

    #[test]
    fn probe_bounds_frame_count_and_requires_annex_b_extension() {
        assert!(validate_frame_count(1).is_ok());
        assert!(validate_frame_count(0).is_err());
        assert!(validate_frame_count(MAX_PROBE_FRAMES + 1).is_err());
        assert!(validate_probe_path(Path::new("recording.h264")).is_ok());
        assert!(validate_probe_path(Path::new("recording.mp4")).is_err());
    }

    #[test]
    fn fake_probe_writes_annex_b_bytes_without_a_container() {
        let config = EncoderConfig::new(64, 48, 500_000).expect("valid config");
        let path = temp_file("fake-encode");
        let mut encoder = FakeEncoder::new(config);
        let report = run_i420_probe(&path, config, 2, &mut encoder).expect("fake probe");
        assert_eq!(report.frames_encoded, 2);
        assert!(report.bytes_written > 0);
        let bytes = std::fs::read(&path).expect("read probe output");
        assert!(bytes.starts_with(&[0, 0, 0, 1]));
        assert!(!bytes.starts_with(b"RIFF"));
        std::fs::remove_file(path).expect("remove probe output");
    }

    #[test]
    fn software_probe_writes_real_openh264_annex_b_stream() {
        let config = EncoderConfig::new(64, 48, 500_000).expect("valid config");
        let path = temp_file("openh264-encode");
        let report = run_openh264_synthetic_probe(&path, config, 2).expect("OpenH264 probe");
        let bytes = std::fs::read(&path).expect("read OpenH264 output");
        assert_eq!(report.backend, EncoderKind::OpenH264);
        assert!(report.bytes_written > 0);
        assert!(bytes.starts_with(&[0, 0, 0, 1]) || bytes.starts_with(&[0, 0, 1]));
        assert!(!bytes.starts_with(b"RIFF"));
        std::fs::remove_file(path).expect("remove probe output");
    }
}
