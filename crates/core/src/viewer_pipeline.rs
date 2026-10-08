//! Bounded viewer decode-to-render handoff for complete H.264 access units.
//!
//! This worker-side pipeline is independent of the UI event bus. A renderer
//! receives only the latest validated NV12 frame through the core FrameSink.

use racc_decode::{
    DecodeError, DecodedFrame, Decoder, DecoderInputQueue, EncodedAccessUnit, QueuePush,
    MAX_DECODE_HEIGHT, MAX_DECODE_WIDTH,
};
use racc_proto::{StreamCodec, StreamReset, StreamStatus};
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use crate::{CoreError, DisplayId, FramePayload, FrameSink, Nv12Frame, VideoFrame};

/// Fixed-capacity timing summary returned from one decode queue drain.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DecodeBatchStats {
    /// Number of decoded frames published to the frame sink.
    pub published_frames: usize,
    /// Elapsed decoder-call durations in microseconds, in decode order.
    pub decode_durations_us: [u64; racc_decode::DECODER_INPUT_QUEUE_CAPACITY],
    /// Number of valid entries in `decode_durations_us`.
    pub decode_sample_count: usize,
}

/// Outcome of adding an access unit to the bounded viewer decoder queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewerQueueOutcome {
    /// The access unit was queued for sequential decoding.
    Queued,
    /// A newer IDR replaced queued work to reduce latency.
    ReplacedBacklog {
        /// Number of queued units discarded.
        dropped: usize,
    },
    /// Dependent access units were dropped until the next IDR arrives.
    DroppedAwaitingKeyframe {
        /// Number of units discarded, including the incoming one.
        dropped: usize,
    },
    /// The access unit was discarded because the viewer is paused or hidden.
    DroppedInactive,
    /// The unit belongs to a stream epoch other than the configured epoch.
    DroppedWrongEpoch {
        /// Configured epoch.
        expected: u16,
        /// Access-unit epoch.
        received: u16,
    },
}

/// Failure while configuring a stream or publishing decoded output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewerPipelineError {
    /// A stream reset failed core validation before it reached the decoder.
    InvalidReset(&'static str),
    /// A reset epoch was not newer than the configured stream epoch.
    StaleReset {
        /// Currently configured epoch.
        current: u16,
        /// Rejected epoch.
        received: u16,
    },
    /// A decoder rejected configuration or an access unit.
    Decode(DecodeError),
    /// Decoder output did not match the configured stream or was malformed.
    InvalidDecodedFrame(&'static str),
    /// The renderer frame sink rejected a validated frame.
    Publish(CoreError),
    /// A frame arrived before a stream reset was applied.
    NotConfigured,
}

impl fmt::Display for ViewerPipelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidReset(reason) => write!(formatter, "invalid stream reset: {reason}"),
            Self::StaleReset { current, received } => write!(
                formatter,
                "stream reset epoch {received} is not newer than configured epoch {current}"
            ),
            Self::Decode(error) => error.fmt(formatter),
            Self::InvalidDecodedFrame(reason) => {
                write!(formatter, "invalid decoder output: {reason}")
            }
            Self::Publish(error) => write!(formatter, "could not publish decoded frame: {error}"),
            Self::NotConfigured => formatter.write_str("viewer decoder has no active stream"),
        }
    }
}

impl std::error::Error for ViewerPipelineError {}

/// Serial decode worker that keeps the last rendered frame through stream resets.
///
/// The compressed queue has a fixed capacity. Call enqueue_access_unit and
/// decode_pending on a video worker, never on the UI event loop. Queue overflow
/// drops dependent data and waits for an IDR rather than decoding an arbitrary
/// predictive frame. The sink is expected to replace its latest frame.
pub struct ViewerFramePipeline<D, S> {
    decoder: D,
    sink: S,
    reset: Option<StreamReset>,
    queue: Option<DecoderInputQueue>,
    active: bool,
    last_decoded_frame: Option<u32>,
}

impl<D: Decoder, S: FrameSink> ViewerFramePipeline<D, S> {
    /// Creates a ready-to-show pipeline with an unconfigured decoder.
    pub fn new(decoder: D, sink: S) -> Self {
        Self {
            decoder,
            sink,
            reset: None,
            queue: None,
            active: true,
            last_decoded_frame: None,
        }
    }

    /// Configures a validated H.264 30 fps stream reset.
    ///
    /// Existing sink content is retained until an NV12 frame from the new epoch
    /// has decoded and passed validation.
    pub fn apply_stream_reset(&mut self, reset: StreamReset) -> Result<(), ViewerPipelineError> {
        validate_reset(reset)?;
        if let Some(current) = self.reset {
            if !epoch_is_newer(reset.epoch, current.epoch) {
                return Err(ViewerPipelineError::StaleReset {
                    current: current.epoch,
                    received: reset.epoch,
                });
            }
        }
        self.decoder
            .configure(reset)
            .map_err(ViewerPipelineError::Decode)?;
        self.reset = Some(reset);
        self.queue = Some(DecoderInputQueue::new(reset.epoch));
        self.last_decoded_frame = None;
        Ok(())
    }

    /// Enables or disables decode processing while the viewer is visible.
    ///
    /// Disabling clears pending compressed work but leaves the last published
    /// frame visible. Re-enabling starts at an IDR boundary.
    pub fn set_active(&mut self, active: bool) {
        if self.active == active {
            return;
        }
        self.active = active;
        if let (Some(reset), Some(queue)) = (self.reset, self.queue.as_mut()) {
            queue.reset(reset.epoch);
        }
    }

    /// Returns the decoder backend selected for the current configuration.
    pub fn decoder_kind(&self) -> racc_decode::DecoderKind {
        self.decoder.kind()
    }

    /// Whether this pipeline should currently decode video.
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// Queues one complete access unit without decoding it.
    pub fn enqueue_access_unit(
        &mut self,
        access_unit: EncodedAccessUnit,
    ) -> Result<ViewerQueueOutcome, ViewerPipelineError> {
        if !self.active {
            return Ok(ViewerQueueOutcome::DroppedInactive);
        }
        let queue = self
            .queue
            .as_mut()
            .ok_or(ViewerPipelineError::NotConfigured)?;
        Ok(match queue.push(access_unit) {
            QueuePush::Enqueued => ViewerQueueOutcome::Queued,
            QueuePush::ReplacedWithKeyframe { dropped } => {
                ViewerQueueOutcome::ReplacedBacklog { dropped }
            }
            QueuePush::DroppedAwaitingKeyframe { dropped } => {
                ViewerQueueOutcome::DroppedAwaitingKeyframe { dropped }
            }
            QueuePush::WrongEpoch { expected, received } => {
                ViewerQueueOutcome::DroppedWrongEpoch { expected, received }
            }
        })
    }

    /// Decodes queued units in order and publishes valid output.
    ///
    /// Returns the number of frames published. Configuration-only units do not
    /// count. When inactive, pending data is cleared and no decode call is made.
    pub fn decode_pending(&mut self) -> Result<usize, ViewerPipelineError> {
        self.decode_pending_timed()
            .map(|stats| stats.published_frames)
    }

    /// Decodes queued units and returns bounded per-call decoder timings.
    ///
    /// At most `DECODER_INPUT_QUEUE_CAPACITY` samples are returned because the
    /// compressed input queue has that fixed capacity.
    pub fn decode_pending_timed(&mut self) -> Result<DecodeBatchStats, ViewerPipelineError> {
        if !self.active {
            if let (Some(reset), Some(queue)) = (self.reset, self.queue.as_mut()) {
                queue.reset(reset.epoch);
            }
            return Ok(DecodeBatchStats::default());
        }
        let reset = self.reset.ok_or(ViewerPipelineError::NotConfigured)?;
        let mut stats = DecodeBatchStats::default();
        while let Some(unit) = self.queue.as_mut().and_then(DecoderInputQueue::pop) {
            let started = Instant::now();
            let decoded = self
                .decoder
                .decode(&unit)
                .map_err(ViewerPipelineError::Decode)?;
            let decode_duration_us =
                u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
            let sample_index = stats.decode_sample_count;
            if let Some(sample) = stats.decode_durations_us.get_mut(sample_index) {
                *sample = decode_duration_us;
                stats.decode_sample_count = stats.decode_sample_count.saturating_add(1);
            }
            if let Some(decoded) = decoded {
                let frame = self.to_video_frame(reset, &unit, decoded, decode_duration_us)?;
                self.sink
                    .publish_frame(Arc::new(frame))
                    .map_err(ViewerPipelineError::Publish)?;
                self.last_decoded_frame = Some(unit.frame_id);
                stats.published_frames = stats.published_frames.saturating_add(1);
            }
        }
        Ok(stats)
    }

    /// Returns the currently configured reset, if one has been applied.
    pub const fn current_reset(&self) -> Option<StreamReset> {
        self.reset
    }

    fn to_video_frame(
        &self,
        reset: StreamReset,
        unit: &EncodedAccessUnit,
        decoded: DecodedFrame,
        decode_duration_us: u64,
    ) -> Result<VideoFrame, ViewerPipelineError> {
        let width = u32::from(reset.width);
        let height = u32::from(reset.height);
        if decoded.epoch != reset.epoch {
            return Err(ViewerPipelineError::InvalidDecodedFrame(
                "decoder returned a frame from another epoch",
            ));
        }
        if decoded.frame_id != unit.frame_id {
            return Err(ViewerPipelineError::InvalidDecodedFrame(
                "decoder output frame id does not match its access unit",
            ));
        }
        if decoded.capture_ts_us != unit.capture_ts_us {
            return Err(ViewerPipelineError::InvalidDecodedFrame(
                "decoder output capture timestamp does not match its access unit",
            ));
        }
        if self
            .last_decoded_frame
            .is_some_and(|previous| decoded.frame_id <= previous)
        {
            return Err(ViewerPipelineError::InvalidDecodedFrame(
                "decoder output frame id is not increasing",
            ));
        }
        if decoded.width != width || decoded.height != height {
            return Err(ViewerPipelineError::InvalidDecodedFrame(
                "decoded dimensions disagree with the stream reset",
            ));
        }
        let y_len = usize::try_from(width)
            .ok()
            .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
            .ok_or(ViewerPipelineError::InvalidDecodedFrame(
                "decoded plane dimensions overflow",
            ))?;
        if decoded.y.len() != y_len || decoded.uv.len() != y_len / 2 {
            return Err(ViewerPipelineError::InvalidDecodedFrame(
                "NV12 planes do not match decoded dimensions",
            ));
        }
        let display_id = DisplayId::new(reset.display_id).ok_or(
            ViewerPipelineError::InvalidReset("display identifier is reserved"),
        )?;
        let frame = VideoFrame {
            epoch: reset.epoch,
            frame_id: decoded.frame_id,
            display_id: Some(display_id),
            width: reset.width,
            height: reset.height,
            fps: reset.fps,
            capture_ts_us: Some(unit.capture_ts_us),
            decode_duration_us: Some(decode_duration_us),
            payload: FramePayload::Nv12(Nv12Frame {
                y: Arc::from(decoded.y.into_boxed_slice()),
                uv: Arc::from(decoded.uv.into_boxed_slice()),
                y_stride: width,
                uv_stride: width,
            }),
        };
        frame.validate().map_err(ViewerPipelineError::Publish)?;
        Ok(frame)
    }
}

fn validate_reset(reset: StreamReset) -> Result<(), ViewerPipelineError> {
    if reset.status != StreamStatus::Ok {
        return Err(ViewerPipelineError::InvalidReset(
            "stream status is not ready",
        ));
    }
    if reset.codec != StreamCodec::H264 {
        return Err(ViewerPipelineError::InvalidReset("codec is not H.264"));
    }
    if reset.fps != 30 {
        return Err(ViewerPipelineError::InvalidReset(
            "frame rate is not 30 fps",
        ));
    }
    let width = u32::from(reset.width);
    let height = u32::from(reset.height);
    if width < 16
        || height < 480
        || width > MAX_DECODE_WIDTH
        || height > MAX_DECODE_HEIGHT
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
    {
        return Err(ViewerPipelineError::InvalidReset(
            "dimensions are outside the supported even dimensions with 480p30 minimum and 1920x1080 maximum",
        ));
    }
    if reset.display_id == 0 {
        return Err(ViewerPipelineError::InvalidReset(
            "display identifier is reserved",
        ));
    }
    Ok(())
}

fn epoch_is_newer(candidate: u16, current: u16) -> bool {
    let distance = candidate.wrapping_sub(current);
    distance != 0 && distance < 32768
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_decode::FakeDecoder;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeFrameSink {
        latest: Mutex<Option<Arc<VideoFrame>>>,
        count: Mutex<usize>,
    }

    impl FrameSink for FakeFrameSink {
        fn publish_frame(&self, frame: Arc<VideoFrame>) -> Result<(), CoreError> {
            *self.latest.lock().map_err(|_| CoreError::Closed)? = Some(frame);
            let mut count = self.count.lock().map_err(|_| CoreError::Closed)?;
            *count = count.saturating_add(1);
            Ok(())
        }
    }

    impl FakeFrameSink {
        fn latest(&self) -> Option<Arc<VideoFrame>> {
            self.latest.lock().ok().and_then(|frame| frame.clone())
        }
        fn count(&self) -> usize {
            self.count.lock().map_or(0, |count| *count)
        }
    }

    fn reset(epoch: u16, width: u16, height: u16) -> StreamReset {
        StreamReset {
            req_id: 0,
            epoch,
            codec: StreamCodec::H264,
            width,
            height,
            fps: 30,
            topology_rev: 1,
            display_id: 7,
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
            .unwrap_or_else(|error| panic!("valid test access unit: {error}"))
    }

    #[test]
    fn timed_decode_returns_one_sample_per_queued_access_unit() {
        let mut pipeline = ViewerFramePipeline::new(FakeDecoder::new(), FakeFrameSink::default());
        pipeline
            .apply_stream_reset(reset(1, 640, 480))
            .expect("reset");
        pipeline
            .enqueue_access_unit(unit(1, 1, true, true))
            .expect("queue IDR");
        pipeline
            .enqueue_access_unit(unit(1, 2, false, false))
            .expect("queue predictive frame");

        let stats = pipeline.decode_pending_timed().expect("decode batch");
        assert_eq!(stats.published_frames, 2);
        assert_eq!(stats.decode_sample_count, 2);
        let latest = pipeline.sink.latest().expect("latest decoded frame");
        assert_eq!(latest.capture_ts_us, Some(2 * 33_333));
        assert!(latest.decode_duration_us.is_some());
    }

    #[test]
    fn rendered_frame_preserves_capture_clock_and_exact_decode_duration_metadata() {
        let mut pipeline = ViewerFramePipeline::new(FakeDecoder::new(), FakeFrameSink::default());
        let stream_reset = reset(1, 640, 480);
        pipeline
            .apply_stream_reset(stream_reset)
            .expect("valid reset");
        let access_unit = unit(1, 1, true, true);
        let decoded = DecodedFrame::new(
            1,
            1,
            access_unit.capture_ts_us,
            640,
            480,
            vec![16; 640 * 480],
            vec![128; 640 * 480 / 2],
        )
        .expect("valid decoded planes");

        let frame = pipeline
            .to_video_frame(stream_reset, &access_unit, decoded, 789)
            .expect("validated render frame");
        assert_eq!(frame.capture_ts_us, Some(access_unit.capture_ts_us));
        assert_eq!(frame.decode_duration_us, Some(789));
    }

    #[test]
    fn reset_holds_old_frame_until_valid_new_epoch_idr() {
        let mut pipeline = ViewerFramePipeline::new(FakeDecoder::new(), FakeFrameSink::default());
        pipeline
            .apply_stream_reset(reset(1, 640, 480))
            .expect("reset");
        pipeline
            .enqueue_access_unit(unit(1, 1, true, true))
            .expect("queue IDR");
        assert_eq!(pipeline.decode_pending(), Ok(1));
        let old = pipeline.sink.latest().expect("old frame");
        pipeline
            .apply_stream_reset(reset(2, 800, 480))
            .expect("switch");
        assert!(Arc::ptr_eq(
            &old,
            &pipeline.sink.latest().expect("held frame")
        ));
        assert_eq!(
            pipeline.enqueue_access_unit(unit(2, 1, false, false)),
            Ok(ViewerQueueOutcome::DroppedAwaitingKeyframe { dropped: 1 })
        );
        assert_eq!(pipeline.decode_pending(), Ok(0));
        assert!(Arc::ptr_eq(
            &old,
            &pipeline.sink.latest().expect("still held")
        ));
        pipeline
            .enqueue_access_unit(unit(2, 2, true, true))
            .expect("new IDR");
        assert_eq!(pipeline.decode_pending(), Ok(1));
        let current = pipeline.sink.latest().expect("new frame");
        assert_eq!(
            (current.epoch, current.frame_id, current.width),
            (2, 2, 800)
        );
        assert!(!Arc::ptr_eq(&old, &current));
    }

    #[test]
    fn hidden_pause_drops_decode_and_keeps_last_published_frame() {
        let mut pipeline = ViewerFramePipeline::new(FakeDecoder::new(), FakeFrameSink::default());
        pipeline
            .apply_stream_reset(reset(4, 640, 480))
            .expect("reset");
        pipeline
            .enqueue_access_unit(unit(4, 1, true, true))
            .expect("queue");
        assert_eq!(pipeline.decode_pending(), Ok(1));
        let last = pipeline.sink.latest().expect("frame");
        pipeline.set_active(false);
        assert_eq!(
            pipeline.enqueue_access_unit(unit(4, 2, false, false)),
            Ok(ViewerQueueOutcome::DroppedInactive)
        );
        assert_eq!(pipeline.decode_pending(), Ok(0));
        pipeline.set_active(true);
        assert_eq!(
            pipeline.enqueue_access_unit(unit(4, 3, false, false)),
            Ok(ViewerQueueOutcome::DroppedAwaitingKeyframe { dropped: 1 })
        );
        assert_eq!(pipeline.decode_pending(), Ok(0));
        assert!(Arc::ptr_eq(
            &last,
            &pipeline.sink.latest().expect("frame retained")
        ));
        assert_eq!(pipeline.sink.count(), 1);
    }

    #[test]
    fn fixed_queue_overflow_waits_for_idr_then_publishes_latest_frame() {
        let mut pipeline = ViewerFramePipeline::new(FakeDecoder::new(), FakeFrameSink::default());
        pipeline
            .apply_stream_reset(reset(8, 640, 480))
            .expect("reset");
        pipeline
            .enqueue_access_unit(unit(8, 1, true, true))
            .expect("IDR");
        pipeline
            .enqueue_access_unit(unit(8, 2, false, false))
            .expect("P frame");
        assert_eq!(
            pipeline.enqueue_access_unit(unit(8, 3, false, false)),
            Ok(ViewerQueueOutcome::DroppedAwaitingKeyframe { dropped: 3 })
        );
        assert_eq!(
            pipeline.enqueue_access_unit(unit(8, 4, false, false)),
            Ok(ViewerQueueOutcome::DroppedAwaitingKeyframe { dropped: 1 })
        );
        pipeline
            .enqueue_access_unit(unit(8, 5, true, true))
            .expect("recovery IDR");
        assert_eq!(pipeline.decode_pending(), Ok(1));
        assert_eq!(pipeline.sink.latest().map(|frame| frame.frame_id), Some(5));
    }

    #[test]
    fn newer_idr_replaces_queued_backlog_before_decode() {
        let mut pipeline = ViewerFramePipeline::new(FakeDecoder::new(), FakeFrameSink::default());
        pipeline
            .apply_stream_reset(reset(9, 640, 480))
            .expect("reset");
        pipeline
            .enqueue_access_unit(unit(9, 1, true, true))
            .expect("first IDR");
        pipeline
            .enqueue_access_unit(unit(9, 2, false, false))
            .expect("predictive frame");
        assert_eq!(
            pipeline.enqueue_access_unit(unit(9, 3, true, true)),
            Ok(ViewerQueueOutcome::ReplacedBacklog { dropped: 2 })
        );
        assert_eq!(pipeline.decode_pending(), Ok(1));
        assert_eq!(pipeline.sink.latest().map(|frame| frame.frame_id), Some(3));
    }

    #[test]
    fn invalid_and_stale_resets_leave_current_stream_usable() {
        let mut pipeline = ViewerFramePipeline::new(FakeDecoder::new(), FakeFrameSink::default());
        pipeline
            .apply_stream_reset(reset(10, 640, 480))
            .expect("reset");
        assert!(matches!(
            pipeline.apply_stream_reset(reset(11, 641, 480)),
            Err(ViewerPipelineError::InvalidReset(_))
        ));
        assert!(matches!(
            pipeline.apply_stream_reset(reset(10, 640, 480)),
            Err(ViewerPipelineError::StaleReset { .. })
        ));
        assert!(matches!(
            pipeline.apply_stream_reset(reset(11, 640, 479)),
            Err(ViewerPipelineError::InvalidReset(_))
        ));
        assert!(validate_reset(reset(11, 640, 480)).is_ok());
        let mut invalid = reset(11, 640, 480);
        invalid.display_id = 0;
        assert!(matches!(
            pipeline.apply_stream_reset(invalid),
            Err(ViewerPipelineError::InvalidReset(_))
        ));
        pipeline
            .enqueue_access_unit(unit(10, 1, true, true))
            .expect("current stream");
        assert_eq!(pipeline.decode_pending(), Ok(1));
    }

    #[test]
    fn wrapping_epoch_order_accepts_zero_after_maximum() {
        let mut pipeline = ViewerFramePipeline::new(FakeDecoder::new(), FakeFrameSink::default());
        pipeline
            .apply_stream_reset(reset(u16::MAX, 640, 480))
            .expect("reset");
        pipeline
            .apply_stream_reset(reset(0, 640, 480))
            .expect("wrapped epoch");
        assert!(matches!(
            pipeline.apply_stream_reset(reset(u16::MAX, 640, 480)),
            Err(ViewerPipelineError::StaleReset { .. })
        ));
    }
}
