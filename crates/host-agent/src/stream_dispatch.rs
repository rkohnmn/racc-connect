//! Bounded bridge from captured frames through an encoder to the UDP sender.

use racc_capture::GpuFrame;
use racc_encode::{EncodeError, EncodedPacket, MAX_ENCODED_PACKET_BYTES};
use racc_net::SenderFrame;

/// Encoder surface needed by the platform-neutral frame dispatcher.
pub(crate) trait FrameEncoder {
    /// Encodes one capture frame; `None` means the backend has not produced output yet.
    fn encode_frame(&mut self, frame: &GpuFrame) -> Result<Option<EncodedPacket>, EncodeError>;
}

/// Metadata returned after an encoded access unit was accepted by the transport adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DispatchMetadata {
    /// Assigned frame id within the stream epoch.
    pub frame_id: u32,
    /// Whether this packet contains an IDR slice.
    pub keyframe: bool,
    /// Whether SPS and PPS parameter sets are both present.
    pub config: bool,
}

/// Typed error from encoding or handing one bounded access unit to the transport.
pub(crate) enum DispatchError<SendError> {
    /// The encoder rejected a capture frame or produced invalid output.
    Encode(EncodeError),
    /// The UDP sender rejected the access unit.
    Send(SendError),
}

/// Encodes and dispatches at most one access unit without retaining capture frames.
pub(crate) fn dispatch_frame<E, S, SendError>(
    encoder: &mut E,
    frame: &GpuFrame,
    epoch: u16,
    next_frame_id: &mut u32,
    send: S,
) -> Result<Option<DispatchMetadata>, DispatchError<SendError>>
where
    E: FrameEncoder,
    S: FnOnce(SenderFrame) -> Result<(), SendError>,
{
    let Some(packet) = encoder.encode_frame(frame).map_err(DispatchError::Encode)? else {
        return Ok(None);
    };
    if packet.bytes.is_empty() || packet.bytes.len() > MAX_ENCODED_PACKET_BYTES {
        return Err(DispatchError::Encode(EncodeError::InvalidBitstream(
            "encoded access unit is empty or exceeds its bound".to_owned(),
        )));
    }
    let frame_id = *next_frame_id;
    *next_frame_id = next_frame_id.wrapping_add(1);
    let config = contains_sps_and_pps(&packet.bytes);
    let metadata = DispatchMetadata {
        frame_id,
        keyframe: packet.keyframe,
        config,
    };
    send(SenderFrame {
        epoch,
        frame_id,
        keyframe: packet.keyframe,
        config,
        capture_ts_us: frame.capture_ts_us as u32,
        bytes: packet.bytes,
    })
    .map_err(DispatchError::Send)?;
    Ok(Some(metadata))
}

fn contains_sps_and_pps(bytes: &[u8]) -> bool {
    let mut has_sps = false;
    let mut has_pps = false;
    let mut offset = 0;
    while let Some((start, prefix_len)) = find_start_code(bytes, offset) {
        let nal_start = start.saturating_add(prefix_len);
        let next = find_start_code(bytes, nal_start).map_or(bytes.len(), |(index, _)| index);
        if let Some(header) = bytes.get(nal_start) {
            match header & 0x1f {
                7 => has_sps = true,
                8 => has_pps = true,
                _ => {}
            }
        }
        if has_sps && has_pps {
            return true;
        }
        if next <= offset {
            break;
        }
        offset = next;
    }
    false
}

fn find_start_code(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut index = from;
    while index < bytes.len() {
        let tail = bytes.get(index..)?;
        if tail.starts_with(&[0, 0, 0, 1]) {
            return Some((index, 4));
        }
        if tail.starts_with(&[0, 0, 1]) {
            return Some((index, 3));
        }
        index = index.checked_add(1)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_capture::{
        CaptureBackend, CaptureParams, CapturedDisplay, DamageSummary, DisplayIdentitySource,
        FakeCaptureBackend,
    };
    use racc_topology::{Display, DisplayFlags, DisplayId};
    use std::time::Duration;

    struct FakeFrameEncoder;

    impl FrameEncoder for FakeFrameEncoder {
        fn encode_frame(&mut self, frame: &GpuFrame) -> Result<Option<EncodedPacket>, EncodeError> {
            Ok(Some(EncodedPacket {
                bytes: vec![
                    0, 0, 0, 1, 0x67, 1, 2, 3, 0, 0, 1, 0x68, 4, 5, 0, 0, 0, 1, 0x65, 6, 7,
                ],
                timestamp_us: frame.capture_ts_us,
                keyframe: true,
            }))
        }
    }

    fn captured_frame(timestamp_us: u64) -> GpuFrame {
        let Some(display_id) = DisplayId::new(1) else {
            unreachable!("non-zero test display id")
        };
        let display = Display::new(
            display_id,
            "fake display",
            0,
            0,
            1280,
            720,
            1000,
            60_000,
            DisplayFlags::new(true, true, true, false),
        );
        let captured = CapturedDisplay {
            display,
            backend_handle: "fake-output".to_owned(),
            adapter_id: "fake-adapter".to_owned(),
            adapter_model: "fake GPU".to_owned(),
            identity_source: DisplayIdentitySource::ConnectorPathOnly,
        };
        let mut capture = FakeCaptureBackend::new(vec![captured]);
        assert!(capture.start(display_id, CaptureParams::default()).is_ok());
        assert!(capture.emit_frame(timestamp_us, DamageSummary::Full));
        match capture.poll_event(Duration::ZERO) {
            Ok(Some(racc_capture::CaptureEvent::Frame(frame))) => frame,
            other => panic!("expected fake GPU frame, got {other:?}"),
        }
    }

    #[test]
    fn captured_frame_is_encoded_and_dispatched_as_bounded_h264_sender_frame() {
        let frame = captured_frame(u64::from(u32::MAX) + 123);
        let mut encoder = FakeFrameEncoder;
        let mut next_frame_id = 41;
        let mut sent = None;
        let outcome = dispatch_frame(
            &mut encoder,
            &frame,
            7,
            &mut next_frame_id,
            |sender_frame| {
                sent = Some(sender_frame);
                Ok::<(), &'static str>(())
            },
        );
        assert!(matches!(
            outcome,
            Ok(Some(DispatchMetadata {
                frame_id: 41,
                keyframe: true,
                config: true,
            }))
        ));
        assert_eq!(next_frame_id, 42);
        let Some(sender_frame) = sent else {
            panic!("sender frame was not dispatched")
        };
        assert_eq!(sender_frame.epoch, 7);
        assert_eq!(sender_frame.frame_id, 41);
        assert!(sender_frame.keyframe);
        assert!(sender_frame.config);
        assert_eq!(sender_frame.capture_ts_us, 122);
        assert!(sender_frame.bytes.ends_with(&[0, 0, 0, 1, 0x65, 6, 7]));
    }

    #[test]
    fn no_encoded_output_does_not_consume_a_frame_id_or_dispatch() {
        struct DelayedEncoder;
        impl FrameEncoder for DelayedEncoder {
            fn encode_frame(
                &mut self,
                _frame: &GpuFrame,
            ) -> Result<Option<EncodedPacket>, EncodeError> {
                Ok(None)
            }
        }
        let frame = captured_frame(99);
        let mut next_frame_id = 5;
        let outcome = dispatch_frame(&mut DelayedEncoder, &frame, 2, &mut next_frame_id, |_| {
            Err::<(), _>("must not be called")
        });
        assert!(matches!(outcome, Ok(None)));
        assert_eq!(next_frame_id, 5);
    }
}
