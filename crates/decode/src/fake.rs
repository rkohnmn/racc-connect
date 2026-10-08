//! Deterministic fake decoder for tests and synthetic pipelines.

use crate::{validate_reset, DecodeError, DecodedFrame, Decoder, DecoderKind, EncodedAccessUnit};
use racc_proto::StreamReset;

/// Fake decoder that validates stream state and emits deterministic NV12 test frames.
///
/// This backend does not decode the encoded H.264 pixels. It is suitable for unit
/// tests of epoch, configuration, keyframe, and frame-handoff behavior only.
pub struct FakeDecoder {
    reset: Option<StreamReset>,
    configured: bool,
    awaiting_keyframe: bool,
    last_frame_id: Option<u32>,
}

impl FakeDecoder {
    /// Creates an unconfigured fake decoder.
    pub const fn new() -> Self {
        Self {
            reset: None,
            configured: false,
            awaiting_keyframe: true,
            last_frame_id: None,
        }
    }
}

impl Default for FakeDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for FakeDecoder {
    fn kind(&self) -> DecoderKind {
        DecoderKind::Fake
    }

    fn configure(&mut self, reset: StreamReset) -> Result<(), DecodeError> {
        self.reset = None;
        self.configured = false;
        self.awaiting_keyframe = true;
        self.last_frame_id = None;
        validate_reset(reset)?;
        self.reset = Some(reset);
        Ok(())
    }

    fn decode(
        &mut self,
        access_unit: &EncodedAccessUnit,
    ) -> Result<Option<DecodedFrame>, DecodeError> {
        let reset = self.reset.ok_or(DecodeError::InvalidConfig(
            "decoder has not been configured",
        ))?;
        if access_unit.epoch != reset.epoch {
            return Err(DecodeError::WrongEpoch {
                expected: reset.epoch,
                received: access_unit.epoch,
            });
        }
        if let Some(previous) = self.last_frame_id {
            if access_unit.frame_id <= previous {
                return Err(DecodeError::OutOfOrderFrame {
                    previous,
                    received: access_unit.frame_id,
                });
            }
        }
        if access_unit.config && !access_unit.has_parameter_sets() {
            return Err(DecodeError::InvalidBitstream(
                "config flag is set without SPS/PPS",
            ));
        }
        if access_unit.has_parameter_sets() {
            self.configured = true;
        }
        self.last_frame_id = Some(access_unit.frame_id);

        if !access_unit.is_keyframe() {
            if self.awaiting_keyframe {
                if access_unit.has_parameter_sets() {
                    // Parameter-set-only or predictive config units are retained as state,
                    // but cannot produce an image before an IDR.
                    return Ok(None);
                }
                return Err(DecodeError::AwaitingKeyframe);
            }
            return self.make_frame(reset, access_unit).map(Some);
        }

        if !self.configured {
            self.awaiting_keyframe = true;
            return Err(DecodeError::MissingCodecConfiguration);
        }
        self.awaiting_keyframe = false;
        self.make_frame(reset, access_unit).map(Some)
    }
}

impl FakeDecoder {
    fn make_frame(
        &self,
        reset: StreamReset,
        access_unit: &EncodedAccessUnit,
    ) -> Result<DecodedFrame, DecodeError> {
        let width = u32::from(reset.width);
        let height = u32::from(reset.height);
        let y_len = usize::try_from(width)
            .ok()
            .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
            .ok_or(DecodeError::InvalidFrame("plane size overflow"))?;
        let uv_len = y_len / 2;
        let luma = 32u8.saturating_add((access_unit.frame_id % 192) as u8);
        DecodedFrame::new(
            access_unit.epoch,
            access_unit.frame_id,
            access_unit.capture_ts_us,
            width,
            height,
            vec![luma; y_len],
            vec![128; uv_len],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DecodeError;
    use racc_proto::{StreamCodec, StreamStatus};

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

    fn unit(epoch: u16, frame_id: u32, idr: bool, config: bool) -> EncodedAccessUnit {
        let mut bytes = Vec::new();
        if config {
            bytes.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0, 0x1e]);
            bytes.extend_from_slice(&[0, 0, 1, 0x68, 0xce]);
        }
        bytes.extend_from_slice(&[0, 0, 0, 1, if idr { 0x65 } else { 0x41 }, 0x88]);
        EncodedAccessUnit::new(epoch, frame_id, frame_id * 33_333, idr, config, bytes)
            .expect("well-formed fake access unit")
    }

    #[test]
    fn fake_decoder_requires_parameter_sets_and_an_idr_after_reset() {
        let mut decoder = FakeDecoder::new();
        decoder.configure(reset(2, 64, 48)).expect("reset");
        assert_eq!(
            decoder.decode(&unit(2, 1, false, false)),
            Err(DecodeError::AwaitingKeyframe)
        );
        let frame = decoder
            .decode(&unit(2, 2, true, true))
            .expect("decode")
            .expect("frame");
        assert_eq!(
            (frame.epoch, frame.frame_id, frame.width, frame.height),
            (2, 2, 64, 48)
        );
        assert_eq!(frame.y.len(), 64 * 48);
        assert_eq!(frame.uv.len(), 64 * 24);
        assert!(frame.y.iter().all(|pixel| *pixel == 34));
        assert!(frame.uv.iter().all(|pixel| *pixel == 128));
    }

    #[test]
    fn parameter_sets_can_arrive_before_idr_and_are_retained() {
        let mut decoder = FakeDecoder::new();
        decoder.configure(reset(3, 64, 48)).expect("reset");
        assert!(decoder
            .decode(&unit(3, 10, false, true))
            .expect("config only")
            .is_none());
        assert!(decoder
            .decode(&unit(3, 11, true, false))
            .expect("idr")
            .is_some());
    }

    #[test]
    fn fake_decoder_rejects_stale_epoch_and_nonmonotonic_frames() {
        let mut decoder = FakeDecoder::new();
        decoder.configure(reset(4, 64, 48)).expect("reset");
        assert_eq!(
            decoder.decode(&unit(3, 1, true, true)),
            Err(DecodeError::WrongEpoch {
                expected: 4,
                received: 3
            })
        );
        decoder.decode(&unit(4, 2, true, true)).expect("first IDR");
        assert_eq!(
            decoder.decode(&unit(4, 2, false, false)),
            Err(DecodeError::OutOfOrderFrame {
                previous: 2,
                received: 2
            })
        );
    }

    #[test]
    fn successful_stream_reset_clears_codec_configuration_and_waits_for_new_idr() {
        let mut decoder = FakeDecoder::new();
        decoder.configure(reset(1, 64, 48)).expect("first reset");
        decoder.decode(&unit(1, 1, true, true)).expect("first IDR");
        decoder.configure(reset(2, 64, 48)).expect("new epoch");
        assert_eq!(
            decoder.decode(&unit(2, 1, true, false)),
            Err(DecodeError::MissingCodecConfiguration)
        );
    }
}
