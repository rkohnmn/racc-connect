//! Portable selection and bounds for the Windows BGRA→NV12 and OpenH264 I420 paths.

use crate::{EncodeError, Encoder, EncoderConfig};

/// Number of output surfaces retained by the nonblocking NV12 conversion pool.
pub const NV12_SURFACE_POOL_CAPACITY: usize = 3;
/// Maximum input capture dimension accepted by the bounded scaler.
pub const MAX_CAPTURE_DIMENSION: u32 = 16_384;
/// Input format presented to the encode pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputPixelFormat {
    /// GPU-resident 8-bit BGRA capture texture.
    Bgra8,
    /// Normalized planar 4:2:0 software input.
    I420,
}

/// Selected input route; the I420 path does not perform a hidden pixel conversion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncoderInputPath {
    /// Same-device D3D11 video processor converts/scales to an NV12 surface for Media Foundation.
    GpuBgraToNv12,
    /// Caller supplies validated I420 planes directly to OpenH264.
    SoftwareI420,
}

/// Bounded conversion decision for a source frame and configured encoder dimensions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConversionPlan {
    /// Selected encoder input route.
    pub path: EncoderInputPath,
    /// Source texture or plane dimensions.
    pub source_width: u32,
    /// Source texture or plane dimensions.
    pub source_height: u32,
    /// Encoder surface dimensions.
    pub output_width: u32,
    /// Encoder surface dimensions.
    pub output_height: u32,
    /// Bytes in one NV12 output surface; zero for direct I420 input.
    pub output_surface_bytes: usize,
    /// Upper bound for the fixed pool allocation.
    pub pool_bytes: usize,
}

/// Selects a conversion route and validates its dimensions and memory bounds.
pub fn conversion_plan(
    format: InputPixelFormat,
    source_width: u32,
    source_height: u32,
    output: EncoderConfig,
) -> Result<ConversionPlan, EncodeError> {
    if source_width == 0
        || source_height == 0
        || source_width > MAX_CAPTURE_DIMENSION
        || source_height > MAX_CAPTURE_DIMENSION
    {
        return Err(EncodeError::InvalidFrame(
            "capture dimensions must be between 1 and 16384 pixels per axis".to_owned(),
        ));
    }
    let (path, output_surface_bytes, pool_bytes) = match format {
        InputPixelFormat::Bgra8 => {
            let pixels = usize::try_from(output.width())
                .ok()
                .and_then(|width| {
                    usize::try_from(output.height())
                        .ok()
                        .and_then(|height| width.checked_mul(height))
                })
                .ok_or_else(|| EncodeError::InvalidFrame("NV12 size overflow".to_owned()))?;
            let surface_bytes = pixels
                .checked_mul(3)
                .map(|bytes| bytes / 2)
                .ok_or_else(|| EncodeError::InvalidFrame("NV12 size overflow".to_owned()))?;
            let pool_bytes = surface_bytes
                .checked_mul(NV12_SURFACE_POOL_CAPACITY)
                .ok_or_else(|| EncodeError::InvalidFrame("NV12 pool size overflow".to_owned()))?;
            (EncoderInputPath::GpuBgraToNv12, surface_bytes, pool_bytes)
        }
        InputPixelFormat::I420
            if source_width == output.width() && source_height == output.height() =>
        {
            (EncoderInputPath::SoftwareI420, 0, 0)
        }
        InputPixelFormat::I420 => {
            return Err(EncodeError::InvalidFrame(
                "I420 software input must match encoder dimensions".to_owned(),
            ));
        }
    };
    Ok(ConversionPlan {
        path,
        source_width,
        source_height,
        output_width: output.width(),
        output_height: output.height(),
        output_surface_bytes,
        pool_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_uses_bounded_gpu_nv12_conversion() {
        let output = EncoderConfig::new(1280, 720, 4_000_000).expect("valid output");
        let plan = conversion_plan(InputPixelFormat::Bgra8, 1920, 1080, output)
            .expect("bounded GPU conversion plan");
        assert_eq!(plan.path, EncoderInputPath::GpuBgraToNv12);
        assert_eq!(plan.output_surface_bytes, 1280 * 720 * 3 / 2);
        assert_eq!(
            plan.pool_bytes,
            plan.output_surface_bytes * NV12_SURFACE_POOL_CAPACITY
        );
        assert_eq!(NV12_SURFACE_POOL_CAPACITY, 3);
    }

    #[test]
    fn i420_fallback_uses_direct_software_input_without_extra_surface() {
        let output = EncoderConfig::new(640, 480, 1_000_000).expect("valid output");
        let plan =
            conversion_plan(InputPixelFormat::I420, 640, 480, output).expect("software input plan");
        assert_eq!(plan.path, EncoderInputPath::SoftwareI420);
        assert_eq!(plan.output_surface_bytes, 0);
        assert_eq!(plan.pool_bytes, 0);
    }

    #[test]
    fn conversion_plan_rejects_unbounded_source_dimensions() {
        let output = EncoderConfig::new(1920, 1080, 6_000_000).expect("valid output");
        assert!(conversion_plan(
            InputPixelFormat::Bgra8,
            MAX_CAPTURE_DIMENSION + 1,
            1080,
            output
        )
        .is_err());
        assert!(conversion_plan(InputPixelFormat::I420, 640, 0, output).is_err());
    }
}

/// A selected encoder instance and the backend label actually reported by that instance.
pub struct EncoderSelection {
    /// Encoder used for the stream or probe.
    pub encoder: Box<dyn crate::Encoder>,
    /// Backend that will encode the input.
    pub backend: crate::EncoderKind,
    /// Whether selection moved from the requested hardware path to software fallback.
    pub used_fallback: bool,
    /// Why the software backend was chosen, when fallback occurred.
    pub fallback_reason: Option<String>,
}

/// Selects Media Foundation for GPU BGRA capture input or OpenH264 for I420 input.
///
/// If BGRA capture has no usable hardware MFT, the caller can use the same-device NV12 readback
/// bridge before passing I420 frames to the selected OpenH264 encoder.
pub fn select_encoder(
    config: EncoderConfig,
    input: InputPixelFormat,
    hardware: Result<Box<dyn crate::Encoder>, EncodeError>,
) -> Result<EncoderSelection, EncodeError> {
    match input {
        InputPixelFormat::Bgra8 => match hardware {
            Ok(encoder) if encoder.kind() != crate::EncoderKind::OpenH264 => {
                let backend = encoder.kind();
                Ok(EncoderSelection {
                    encoder,
                    backend,
                    used_fallback: false,
                    fallback_reason: None,
                })
            }
            Ok(_) => Err(EncodeError::UnsupportedInput(
                "OpenH264 requires normalized I420 input; captured BGRA needs the NV12 readback bridge",
            )),
            Err(error) => {
                let fallback_reason = error.to_string();
                let encoder = crate::OpenH264Encoder::new(config)?;
                Ok(EncoderSelection {
                    backend: encoder.kind(),
                    encoder: Box::new(encoder),
                    used_fallback: true,
                    fallback_reason: Some(fallback_reason),
                })
            }
        },
        InputPixelFormat::I420 => match hardware {
            Ok(encoder) if encoder.kind() == crate::EncoderKind::OpenH264 => {
                let backend = encoder.kind();
                Ok(EncoderSelection {
                    encoder,
                    backend,
                    used_fallback: false,
                    fallback_reason: None,
                })
            }
            Ok(_incompatible_hardware) => {
                let fallback_reason =
                    "the selected hardware encoder accepts D3D11 NV12, while input is I420"
                        .to_owned();
                let encoder = crate::OpenH264Encoder::new(config)?;
                Ok(EncoderSelection {
                    backend: encoder.kind(),
                    encoder: Box::new(encoder),
                    used_fallback: true,
                    fallback_reason: Some(fallback_reason),
                })
            }
            Err(error) => {
                let fallback_reason = error.to_string();
                let encoder = crate::OpenH264Encoder::new(config)?;
                Ok(EncoderSelection {
                    backend: encoder.kind(),
                    encoder: Box::new(encoder),
                    used_fallback: true,
                    fallback_reason: Some(fallback_reason),
                })
            }
        },
    }
}
#[cfg(test)]
mod selection_tests {
    use super::*;
    use crate::{EncodedPacket, Encoder, EncoderInput, FakeEncoder};

    struct FakeHardwareEncoder(FakeEncoder);

    impl Encoder for FakeHardwareEncoder {
        fn kind(&self) -> crate::EncoderKind {
            crate::EncoderKind::WindowsMediaFoundationHardware
        }

        fn config(&self) -> EncoderConfig {
            self.0.config()
        }

        fn encode(
            &mut self,
            input: EncoderInput<'_>,
        ) -> Result<Option<EncodedPacket>, EncodeError> {
            self.0.encode(input)
        }
    }

    #[test]
    fn selector_reports_the_actual_hardware_backend_label() {
        let config = EncoderConfig::new(64, 48, 500_000).expect("valid config");
        let fake = FakeHardwareEncoder(FakeEncoder::new(config));
        let selection = select_encoder(config, InputPixelFormat::Bgra8, Ok(Box::new(fake)))
            .expect("available hardware encoder");
        assert_eq!(
            selection.backend,
            crate::EncoderKind::WindowsMediaFoundationHardware
        );
        assert!(!selection.used_fallback);
        assert!(selection.fallback_reason.is_none());
    }

    #[test]
    fn selector_uses_openh264_for_i420_after_hardware_failure_and_keeps_reason() {
        let config = EncoderConfig::new(64, 48, 500_000).expect("valid config");
        let selection = select_encoder(
            config,
            InputPixelFormat::I420,
            Err(EncodeError::NoHardwareEncoder(
                "no compatible MFT".to_owned(),
            )),
        )
        .expect("software fallback");
        assert_eq!(selection.backend, crate::EncoderKind::OpenH264);
        assert!(selection.used_fallback);
        assert!(selection
            .fallback_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("no compatible MFT")));
    }

    #[test]
    fn selector_uses_openh264_for_bgra_after_hardware_failure() {
        let config = EncoderConfig::new(64, 48, 500_000).expect("valid config");
        let selection = select_encoder(
            config,
            InputPixelFormat::Bgra8,
            Err(EncodeError::NoHardwareEncoder(
                "no compatible MFT".to_owned(),
            )),
        )
        .expect("OpenH264 fallback selected");
        assert_eq!(selection.backend, crate::EncoderKind::OpenH264);
        assert!(selection.used_fallback);
        assert!(selection
            .fallback_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("no compatible MFT")));
    }
}
