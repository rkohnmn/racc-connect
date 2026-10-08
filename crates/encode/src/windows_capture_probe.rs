//! Explicitly opt-in real capture→hardware encode-to-file probe for a human at the console.

use std::path::Path;
use std::time::{Duration, Instant};

use racc_capture::windows::WindowsCaptureBackend;
use racc_capture::{CaptureBackend, CaptureEvent, CaptureParams};
use racc_topology::DisplayId;

use crate::pipeline::{select_encoder, EncoderSelection, InputPixelFormat};
use crate::probe::{validate_frame_count, AnnexBFile, ProbeReport};
use crate::windows::MediaFoundationH264Encoder;
use crate::windows_software_fallback::WindowsI420Readback;
use crate::windows_video_processor::WindowsNv12Converter;
use crate::{EncodeError, EncoderConfig, EncoderInput};

/// Captures at most 1,800 visible frames or 60 seconds, whichever comes first, and writes Annex B.
///
/// The caller must require a deliberate visible-screen acknowledgement before invoking this
/// function. Tests and synthetic probes must use the fake or pattern-based APIs instead.
pub fn run_capture_encode_probe(
    path: impl AsRef<Path>,
    display_id: DisplayId,
    config: EncoderConfig,
    frame_count: u32,
) -> Result<ProbeReport, EncodeError> {
    validate_frame_count(frame_count)?;
    let mut capture = WindowsCaptureBackend::new();
    let result = (|| {
        let mut output = AnnexBFile::create(path.as_ref())?;
        let displays = capture.enumerate_displays().map_err(|error| {
            EncodeError::Backend(format!("enumerate capture displays: {error}"))
        })?;
        if !displays
            .iter()
            .any(|display| display.display.id() == display_id)
        {
            return Err(EncodeError::InvalidConfig(format!(
                "capture display {} is unavailable",
                display_id.get()
            )));
        }
        capture
            .start(display_id, CaptureParams::default())
            .map_err(|error| {
                EncodeError::Backend(format!("start visible-screen capture: {error}"))
            })?;

        let deadline = Instant::now() + Duration::from_secs(60);
        let mut captured = 0u32;
        let mut encoded = 0u32;
        let mut converter: Option<WindowsNv12Converter> = None;
        let mut software_readback: Option<WindowsI420Readback> = None;
        let mut encoder: Option<EncoderSelection> = None;
        while captured < frame_count && Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match capture
                .poll_event(remaining.min(Duration::from_millis(16)))
                .map_err(|error| EncodeError::Backend(format!("poll desktop capture: {error}")))?
            {
                Some(CaptureEvent::Frame(frame)) => {
                    if converter.is_none() {
                        converter = Some(WindowsNv12Converter::new(&frame, config)?);
                        let hardware = MediaFoundationH264Encoder::new_for_capture(&frame, config)
                            .map(|encoder| Box::new(encoder) as Box<dyn crate::Encoder>);
                        let mut selection =
                            select_encoder(config, InputPixelFormat::Bgra8, hardware)?;
                        if selection.backend == crate::EncoderKind::OpenH264 {
                            software_readback =
                                Some(WindowsI420Readback::new_for_capture(&frame, config)?);
                        }
                        selection.encoder.request_keyframe()?;
                        encoder = Some(selection);
                    }
                    let converter_ref = converter.as_mut().ok_or_else(|| {
                        EncodeError::Backend("NV12 converter did not initialize".to_owned())
                    })?;
                    let encoder_ref = encoder.as_mut().ok_or_else(|| {
                        EncodeError::Backend(
                            "Media Foundation encoder did not initialize".to_owned(),
                        )
                    })?;
                    let surface = converter_ref.convert(&frame)?;
                    let packet =
                        match encoder_ref.backend {
                            crate::EncoderKind::WindowsMediaFoundationHardware => encoder_ref
                                .encoder
                                .encode(EncoderInput::PooledNv12(surface))?,
                            crate::EncoderKind::OpenH264 => {
                                let readback = software_readback.as_mut().ok_or_else(|| {
                                    EncodeError::Backend(
                                        "OpenH264 fallback readback did not initialize".to_owned(),
                                    )
                                })?;
                                readback.with_i420(surface.as_encoder_frame(), |i420| {
                                    encoder_ref.encoder.encode(EncoderInput::I420(i420))
                                })??
                            }
                            crate::EncoderKind::MacVideoToolbox => {
                                return Err(EncodeError::UnsupportedInput(
                                    "VideoToolbox cannot process Windows capture surfaces",
                                ));
                            }
                            crate::EncoderKind::Fake => {
                                return Err(EncodeError::UnsupportedInput(
                                    "fake encoder cannot process captured frames",
                                ));
                            }
                        };
                    if let Some(packet) = packet {
                        output.write_packet(&packet)?;
                        encoded = encoded.saturating_add(1);
                    }
                    captured = captured.saturating_add(1);
                }
                Some(CaptureEvent::AccessLost(reason)) => {
                    return Err(EncodeError::Backend(format!(
                        "desktop capture access lost: {reason:?}"
                    )));
                }
                Some(CaptureEvent::DisplayLost) => {
                    return Err(EncodeError::Backend(
                        "capture display disappeared during encode probe".to_owned(),
                    ));
                }
                Some(CaptureEvent::DeviceLost) => {
                    return Err(EncodeError::Backend(
                        "D3D11 device was lost during encode probe".to_owned(),
                    ));
                }
                Some(CaptureEvent::Error(kind)) => {
                    return Err(EncodeError::Backend(format!(
                        "desktop capture failed: {kind:?}"
                    )));
                }
                Some(_) | None => {}
            }
        }
        if captured != frame_count {
            return Err(EncodeError::Backend(format!(
                "captured {captured} of {frame_count} requested frames within 60 seconds"
            )));
        }
        let mut encoder = encoder.ok_or_else(|| {
            EncodeError::Backend("no visible capture frames were received".to_owned())
        })?;
        if let Some(packet) = encoder.encoder.finish()? {
            output.write_packet(&packet)?;
            encoded = encoded.saturating_add(1);
        }
        let bytes_written = output.finish()?;
        if encoded == 0 || bytes_written == 0 {
            return Err(EncodeError::InvalidBitstream(
                "capture encode probe produced no Annex B output".to_owned(),
            ));
        }
        Ok(ProbeReport {
            path: path.as_ref().to_path_buf(),
            backend: encoder.backend,
            used_fallback: encoder.used_fallback,
            fallback_reason: encoder.fallback_reason.clone(),
            width: config.width(),
            height: config.height(),
            fps: config.fps(),
            frames_requested: frame_count,
            frames_encoded: encoded,
            bytes_written,
        })
    })();
    capture.stop();
    result
}
