use crate::{LossEstimate, LossEstimator, LossSample, NetError};
use racc_proto::{
    parse_video_datagram, CursorUpdate, VideoDatagram, VideoHeader, MAX_FRAGMENTS_PER_FRAME,
    MAX_VIDEO_PAYLOAD, PROTOCOL_VERSION, VIDEO_FLAG_CONFIG, VIDEO_FLAG_KEY,
    VIDEO_FLAG_LAST_FRAGMENT,
};
use std::collections::BTreeMap;

/// Maximum partial or completed frames retained by the reassembler.
pub const MAX_INFLIGHT_FRAMES: usize = 4;
/// Maximum encoded payload bytes retained in flight, including completed reorder holds.
pub const MAX_INFLIGHT_BYTES: usize = 4 * 1024 * 1024;
/// Idle timeout for an incomplete frame, in milliseconds.
pub const REASSEMBLY_TIMEOUT_MS: u64 = 100;
/// Quiet period before a newer completed frame makes an older partial a declared loss.
pub const REORDER_WINDOW_MS: u64 = 8;
/// Initial keyframe-request retry interval, in milliseconds.
pub const KEYFRAME_REQUEST_MIN_INTERVAL_MS: u64 = 200;
/// Maximum keyframe-request retry interval, in milliseconds.
pub const KEYFRAME_REQUEST_MAX_BACKOFF_MS: u64 = 1000;
const MILLIS_TO_MICROS: u64 = 1000;

/// Metadata shared by every fragment in an access unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentMetadata {
    /// Stream epoch.
    pub epoch: u16,
    /// Frame identifier within the epoch.
    pub frame_id: u32,
    /// H.264 IDR/keyframe flag.
    pub keyframe: bool,
    /// SPS/PPS configuration flag.
    pub config: bool,
    /// Host capture timestamp, wrapping microseconds.
    pub capture_ts_us: u32,
    /// Fragment count declared by the first accepted fragment.
    pub fragment_count: u16,
}

/// One complete encoded frame ready for a decoder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameData {
    /// Frame identifier.
    pub frame_id: u32,
    /// Whether this frame is a keyframe.
    pub keyframe: bool,
    /// Whether this frame contains codec configuration.
    pub config: bool,
    /// Host capture timestamp in microseconds.
    pub capture_ts_us: u32,
    /// Fragment count used by the loss estimator.
    pub fragment_count: u16,
    /// Reassembled Annex B access-unit bytes.
    pub bytes: Vec<u8>,
}

/// Why a frame was discarded or declared lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReassemblyReason {
    /// A datagram could not be parsed.
    Parse,
    /// A datagram came from a different stream epoch.
    WrongEpoch,
    /// A repeated fragment was ignored.
    Duplicate,
    /// A fragment belonged to a frame at or before the processed watermark.
    Late,
    /// An incomplete frame was discarded after its timeout.
    Incomplete,
    /// A completed predictive frame was discarded while waiting for a keyframe.
    WaitingForKey,
    /// Fragments disagreed on frame metadata or uniform fragment sizes.
    Inconsistent,
    /// An in-flight frame was evicted to preserve the memory or frame-count bound.
    Evicted,
    /// One or more whole frame identifiers were absent.
    NetworkGap,
}

/// Event emitted by the reassembler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReassemblyEvent {
    /// A complete, ordered frame is ready for decoding.
    FrameReady(FrameData),
    /// The caller should send a RequestKeyframe control message for this epoch.
    NeedKeyframe(u16),
    /// Cursor metadata arrived on the UDP channel.
    Cursor(CursorUpdate),
}

/// Cumulative packet and frame processing counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReassemblyCounters {
    /// Video and cursor datagrams received.
    pub datagrams_received: u64,
    /// Bytes in received datagrams.
    pub bytes_received: u64,
    /// Datagrams rejected by the bounded protocol parser.
    pub parse_errors: u64,
    /// Datagrams ignored because their epoch did not match.
    pub wrong_epoch: u64,
    /// Repeated fragments ignored.
    pub duplicates: u64,
    /// Fragments dropped as late.
    pub late: u64,
    /// Frames for which all fragments arrived.
    pub frames_completed: u64,
    /// Frames returned to the caller in order.
    pub frames_delivered: u64,
    /// Incomplete frames discarded.
    pub dropped_incomplete: u64,
    /// Predictive frames discarded while seeking a keyframe.
    pub dropped_waiting_key: u64,
    /// Frames discarded because their fragments were inconsistent.
    pub dropped_inconsistent: u64,
    /// In-flight frames evicted by a bound.
    pub dropped_evicted: u64,
    /// Whole-frame gaps declared from frame identifiers.
    pub dropped_gap: u64,
    /// Keyframe requests emitted.
    pub keyframe_requests: u64,
    /// Current partial and reorder-held completed frame count.
    pub current_inflight_frames: usize,
    /// Current partial and reorder-held encoded bytes.
    pub current_inflight_bytes: usize,
}

/// Reassembly state and current rolling loss estimate.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReassemblyStats {
    /// Reassembly counters.
    pub counters: ReassemblyCounters,
    /// Rolling loss estimate over the configured interval.
    pub loss: LossEstimate,
    /// Whether only a keyframe may be delivered.
    pub need_keyframe: bool,
}

/// Slicing or bounded-allocation error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliceError {
    /// Empty frames are not valid video access units.
    Empty,
    /// The frame is larger than the protocol's fragment-count bound.
    TooLarge,
    /// Caller-provided output buffer count does not match the frame's fragment count.
    BufferCount,
    /// A bounded allocation could not be reserved.
    AllocationFailed,
}

impl From<SliceError> for NetError {
    fn from(error: SliceError) -> Self {
        match error {
            SliceError::Empty | SliceError::TooLarge => Self::InvalidFrameSize,
            SliceError::BufferCount => Self::BufferCount,
            SliceError::AllocationFailed => Self::AllocationFailed,
        }
    }
}

/// Number of datagrams required by a nonempty encoded frame.
pub fn fragment_count(frame_len: usize) -> Result<usize, SliceError> {
    if frame_len == 0 {
        return Err(SliceError::Empty);
    }
    let rounded = frame_len
        .checked_add(MAX_VIDEO_PAYLOAD - 1)
        .ok_or(SliceError::TooLarge)?;
    let count = rounded / MAX_VIDEO_PAYLOAD;
    if count > MAX_FRAGMENTS_PER_FRAME {
        return Err(SliceError::TooLarge);
    }
    Ok(count)
}

/// Fills caller-owned byte vectors with uniform-size protocol datagrams.
pub fn slice_frame_into(
    frame: &crate::SenderFrame,
    output: &mut [Vec<u8>],
) -> Result<(), SliceError> {
    let count = fragment_count(frame.bytes.len())?;
    if output.len() != count {
        return Err(SliceError::BufferCount);
    }
    for (index, (payload, datagram)) in frame
        .bytes
        .chunks(MAX_VIDEO_PAYLOAD)
        .zip(output.iter_mut())
        .enumerate()
    {
        let index_u16 = u16::try_from(index).map_err(|_| SliceError::TooLarge)?;
        let count_u16 = u16::try_from(count).map_err(|_| SliceError::TooLarge)?;
        let mut flags = 0u8;
        if frame.keyframe {
            flags |= VIDEO_FLAG_KEY;
        }
        if frame.config {
            flags |= VIDEO_FLAG_CONFIG;
        }
        if index + 1 == count {
            flags |= VIDEO_FLAG_LAST_FRAGMENT;
        }
        let header = VideoHeader {
            version: PROTOCOL_VERSION,
            flags,
            epoch: frame.epoch,
            frame_id: frame.frame_id,
            frag_idx: index_u16,
            frag_cnt: count_u16,
            capture_ts_us: frame.capture_ts_us,
        };
        datagram.clear();
        datagram
            .try_reserve(payload.len().saturating_add(racc_proto::VIDEO_HEADER_LEN))
            .map_err(|_| SliceError::AllocationFailed)?;
        racc_proto::encode_video_datagram(header, payload, datagram)
            .map_err(|_| SliceError::TooLarge)?;
    }
    Ok(())
}

#[derive(Debug)]
struct PartialFrame {
    metadata: FragmentMetadata,
    last_activity_us: u64,
    fragments: BTreeMap<u16, Vec<u8>>,
    byte_len: usize,
}

#[derive(Debug)]
struct CompletedFrame {
    data: FrameData,
    completed_at_us: u64,
}

#[derive(Debug)]
struct ReassemblyState {
    epoch: u16,
    last_processed: Option<u32>,
    last_delivered: Option<u32>,
    need_keyframe: bool,
    last_keyframe_request_us: Option<u64>,
    next_request_interval_ms: u64,
    partial: Vec<PartialFrame>,
    completed: Vec<CompletedFrame>,
    inflight_bytes: usize,
    counters: ReassemblyCounters,
    loss: LossEstimator,
}

/// Bounded latest-wins frame reassembler and keyframe-request policy.
#[derive(Debug)]
pub struct Reassembler {
    state: ReassemblyState,
    byte_budget: usize,
}

impl Reassembler {
    /// Creates a reassembler at the epoch supplied by StreamReset.
    pub fn new(epoch: u16) -> Self {
        Self::with_byte_budget(epoch, MAX_INFLIGHT_BYTES)
    }

    /// Creates a reassembler with a configurable in-flight payload budget.
    pub fn with_byte_budget(epoch: u16, byte_budget: usize) -> Self {
        Self {
            state: ReassemblyState {
                epoch,
                last_processed: None,
                last_delivered: None,
                need_keyframe: true,
                last_keyframe_request_us: None,
                next_request_interval_ms: KEYFRAME_REQUEST_MIN_INTERVAL_MS,
                partial: Vec::new(),
                completed: Vec::new(),
                inflight_bytes: 0,
                counters: ReassemblyCounters::default(),
                loss: LossEstimator::default(),
            },
            byte_budget: byte_budget.clamp(MAX_VIDEO_PAYLOAD, MAX_INFLIGHT_BYTES),
        }
    }

    /// Clears in-flight state and begins a new stream epoch.
    pub fn reset(&mut self, epoch: u16, now_us: u64) -> Vec<ReassemblyEvent> {
        self.state.epoch = epoch;
        self.state.last_processed = None;
        self.state.last_delivered = None;
        self.state.partial.clear();
        self.state.completed.clear();
        self.state.inflight_bytes = 0;
        self.state.need_keyframe = true;
        self.state.last_keyframe_request_us = None;
        self.state.next_request_interval_ms = KEYFRAME_REQUEST_MIN_INTERVAL_MS;
        self.refresh_current();
        let mut events = Vec::new();
        self.maybe_request_keyframe(now_us, &mut events);
        events
    }

    /// Accepts one raw UDP datagram and emits any frames or keyframe requests.
    pub fn push_datagram(&mut self, datagram: &[u8], now_us: u64) -> Vec<ReassemblyEvent> {
        let mut events = Vec::new();
        self.state.counters.datagrams_received =
            self.state.counters.datagrams_received.saturating_add(1);
        self.state.counters.bytes_received = self
            .state
            .counters
            .bytes_received
            .saturating_add(u64::try_from(datagram.len()).unwrap_or(u64::MAX));
        match parse_video_datagram(datagram) {
            Ok(VideoDatagram::Cursor(cursor)) => {
                if cursor.epoch == self.state.epoch {
                    events.push(ReassemblyEvent::Cursor(cursor));
                } else {
                    self.state.counters.wrong_epoch =
                        self.state.counters.wrong_epoch.saturating_add(1);
                }
            }
            Ok(VideoDatagram::Video { header, payload }) => {
                if header.epoch != self.state.epoch {
                    self.state.counters.wrong_epoch =
                        self.state.counters.wrong_epoch.saturating_add(1);
                } else if self.is_late(header.frame_id) {
                    self.state.counters.late = self.state.counters.late.saturating_add(1);
                } else if self.valid_uniform_size(header, payload.len()) {
                    self.accept_fragment(header, payload, now_us, &mut events);
                    self.resolve_completed(now_us, &mut events);
                } else {
                    self.state.counters.dropped_inconsistent =
                        self.state.counters.dropped_inconsistent.saturating_add(1);
                    self.drop_partial(header.frame_id, ReassemblyReason::Inconsistent, now_us);
                    self.require_keyframe(now_us, &mut events);
                }
            }
            Err(_) => {
                self.state.counters.parse_errors =
                    self.state.counters.parse_errors.saturating_add(1);
            }
        }
        self.evict_timeouts(now_us, &mut events);
        self.resolve_completed(now_us, &mut events);
        self.maybe_request_keyframe(now_us, &mut events);
        self.refresh_current();
        events
    }

    /// Polls timeout and reorder deadlines on the caller's monotonic clock.
    pub fn poll(&mut self, now_us: u64) -> Vec<ReassemblyEvent> {
        let mut events = Vec::new();
        self.evict_timeouts(now_us, &mut events);
        self.resolve_completed(now_us, &mut events);
        self.maybe_request_keyframe(now_us, &mut events);
        self.refresh_current();
        events
    }

    /// Marks a ready frame as lost outside the reassembler, for example when a bounded output
    /// event channel is full.
    pub fn force_keyframe_request(&mut self, now_us: u64) -> Vec<ReassemblyEvent> {
        let mut events = Vec::new();
        self.require_keyframe(now_us, &mut events);
        self.refresh_current();
        events
    }

    /// Returns current counters and the rolling loss estimate.
    pub fn stats(&mut self, now_us: u64) -> ReassemblyStats {
        ReassemblyStats {
            counters: self.state.counters,
            loss: self.state.loss.estimate(now_us),
            need_keyframe: self.state.need_keyframe,
        }
    }

    /// Returns the active stream epoch.
    pub fn epoch(&self) -> u16 {
        self.state.epoch
    }

    fn is_late(&self, frame_id: u32) -> bool {
        self.state
            .last_processed
            .is_some_and(|last| !serial_newer(frame_id, last))
    }

    fn valid_uniform_size(&self, header: VideoHeader, payload_len: usize) -> bool {
        let last = usize::from(header.frag_idx) + 1 == usize::from(header.frag_cnt);
        if last {
            (1..=MAX_VIDEO_PAYLOAD).contains(&payload_len)
        } else {
            payload_len == MAX_VIDEO_PAYLOAD
        }
    }

    fn accept_fragment(
        &mut self,
        header: VideoHeader,
        payload: &[u8],
        now_us: u64,
        events: &mut Vec<ReassemblyEvent>,
    ) {
        let metadata = FragmentMetadata {
            epoch: header.epoch,
            frame_id: header.frame_id,
            keyframe: header.flags & VIDEO_FLAG_KEY != 0,
            config: header.flags & VIDEO_FLAG_CONFIG != 0,
            capture_ts_us: header.capture_ts_us,
            fragment_count: header.frag_cnt,
        };
        let existing = self
            .state
            .partial
            .iter()
            .position(|frame| frame.metadata.frame_id == header.frame_id);

        if let Some(index) = existing {
            if self.state.partial[index].metadata != metadata {
                self.state.counters.dropped_inconsistent =
                    self.state.counters.dropped_inconsistent.saturating_add(1);
                self.drop_partial(header.frame_id, ReassemblyReason::Inconsistent, now_us);
                self.require_keyframe(now_us, events);
                return;
            }
            if self.state.partial[index]
                .fragments
                .contains_key(&header.frag_idx)
            {
                self.state.counters.duplicates = self.state.counters.duplicates.saturating_add(1);
                return;
            }
        } else {
            while self.inflight_frames() >= MAX_INFLIGHT_FRAMES {
                if !self.evict_oldest(header.frame_id, now_us) {
                    break;
                }
                self.require_keyframe(now_us, events);
            }
            let empty = PartialFrame {
                metadata,
                last_activity_us: now_us,
                fragments: BTreeMap::new(),
                byte_len: 0,
            };
            self.state.partial.push(empty);
        }

        while self.state.inflight_bytes.saturating_add(payload.len()) > self.byte_budget {
            if !self.evict_oldest(header.frame_id, now_us) {
                self.drop_partial(header.frame_id, ReassemblyReason::Evicted, now_us);
                self.require_keyframe(now_us, events);
                return;
            }
            self.require_keyframe(now_us, events);
        }

        let Some(index) = self
            .state
            .partial
            .iter()
            .position(|frame| frame.metadata.frame_id == header.frame_id)
        else {
            return;
        };
        let Some(frame) = self.state.partial.get_mut(index) else {
            return;
        };
        let mut fragment = Vec::new();
        if fragment.try_reserve_exact(payload.len()).is_err() {
            self.state.counters.dropped_inconsistent =
                self.state.counters.dropped_inconsistent.saturating_add(1);
            self.drop_partial(header.frame_id, ReassemblyReason::Inconsistent, now_us);
            self.require_keyframe(now_us, events);
            return;
        }
        fragment.extend_from_slice(payload);
        frame.fragments.insert(header.frag_idx, fragment);
        frame.last_activity_us = now_us;
        frame.byte_len = frame.byte_len.saturating_add(payload.len());
        self.state.inflight_bytes = self.state.inflight_bytes.saturating_add(payload.len());

        if usize::from(frame.metadata.fragment_count) == frame.fragments.len() {
            self.complete_frame(header.frame_id, now_us, events);
        }
        self.refresh_current();
    }

    fn complete_frame(&mut self, frame_id: u32, now_us: u64, events: &mut Vec<ReassemblyEvent>) {
        let Some(index) = self
            .state
            .partial
            .iter()
            .position(|frame| frame.metadata.frame_id == frame_id)
        else {
            return;
        };
        let Some(frame) = self.state.partial.get(index) else {
            return;
        };
        let expected_len = frame.byte_len;
        while self.state.inflight_bytes.saturating_add(expected_len) > self.byte_budget {
            if !self.evict_oldest(frame_id, now_us) {
                self.drop_partial(frame_id, ReassemblyReason::Evicted, now_us);
                self.require_keyframe(now_us, events);
                return;
            }
            self.require_keyframe(now_us, events);
        }

        let Some(current_index) = self
            .state
            .partial
            .iter()
            .position(|item| item.metadata.frame_id == frame_id)
        else {
            return;
        };
        let Some(frame) = self.state.partial.get(current_index) else {
            return;
        };
        let mut bytes = Vec::new();
        if bytes.try_reserve_exact(expected_len).is_err() {
            self.state.counters.dropped_inconsistent =
                self.state.counters.dropped_inconsistent.saturating_add(1);
            self.drop_partial(frame_id, ReassemblyReason::Inconsistent, now_us);
            self.require_keyframe(now_us, events);
            return;
        }
        for fragment_idx in 0..frame.metadata.fragment_count {
            let Some(fragment) = frame.fragments.get(&fragment_idx) else {
                self.state.counters.dropped_inconsistent =
                    self.state.counters.dropped_inconsistent.saturating_add(1);
                self.drop_partial(frame_id, ReassemblyReason::Inconsistent, now_us);
                return;
            };
            bytes.extend_from_slice(fragment);
        }

        let metadata = frame.metadata;
        let old_bytes = frame.byte_len;
        self.state.partial.remove(current_index);
        self.state.inflight_bytes = self.state.inflight_bytes.saturating_sub(old_bytes);
        self.state.inflight_bytes = self.state.inflight_bytes.saturating_add(bytes.len());
        self.state.counters.frames_completed =
            self.state.counters.frames_completed.saturating_add(1);
        self.state.loss.record(
            now_us,
            LossSample {
                expected_fragments: u64::from(metadata.fragment_count),
                frames_observed: 1,
                ..LossSample::default()
            },
        );
        self.state.completed.push(CompletedFrame {
            data: FrameData {
                frame_id,
                keyframe: metadata.keyframe,
                config: metadata.config,
                capture_ts_us: metadata.capture_ts_us,
                fragment_count: metadata.fragment_count,
                bytes,
            },
            completed_at_us: now_us,
        });
        self.refresh_current();
    }

    fn resolve_completed(&mut self, now_us: u64, events: &mut Vec<ReassemblyEvent>) {
        loop {
            let Some(candidate_index) = self.next_completed_index() else {
                return;
            };
            let Some(candidate) = self.state.completed.get(candidate_index) else {
                return;
            };
            let candidate_id = candidate.data.frame_id;
            let candidate_key = candidate.data.keyframe;
            let expected = self.state.last_processed.map(wrapping_next);
            let is_expected = expected.is_some_and(|id| id == candidate_id);
            let older_partials: Vec<u32> = self
                .state
                .partial
                .iter()
                .filter(|partial| serial_before(partial.metadata.frame_id, candidate_id))
                .map(|partial| partial.metadata.frame_id)
                .collect();
            let has_recent_older_partial = older_partials.iter().any(|id| {
                self.state
                    .partial
                    .iter()
                    .find(|partial| partial.metadata.frame_id == *id)
                    .is_some_and(|partial| {
                        now_us.saturating_sub(partial.last_activity_us)
                            < REORDER_WINDOW_MS.saturating_mul(MILLIS_TO_MICROS)
                    })
            });

            if !is_expected && has_recent_older_partial {
                return;
            }

            if !is_expected {
                if older_partials.is_empty() {
                    if let Some(expected_id) = expected {
                        let gap = candidate_id.wrapping_sub(expected_id);
                        if gap > 0 && gap < (1u32 << 31) {
                            self.declare_gap(gap, now_us, events);
                        }
                    }
                } else {
                    self.require_keyframe(now_us, events);
                    for id in older_partials {
                        self.drop_partial(id, ReassemblyReason::Incomplete, now_us);
                    }
                }

                if !candidate_key {
                    self.drop_completed(candidate_index);
                    self.state.counters.dropped_waiting_key =
                        self.state.counters.dropped_waiting_key.saturating_add(1);
                    self.state.last_processed = Some(candidate_id);
                    continue;
                }
            }

            if self.state.need_keyframe && !candidate_key {
                self.drop_completed(candidate_index);
                self.state.counters.dropped_waiting_key =
                    self.state.counters.dropped_waiting_key.saturating_add(1);
                self.state.last_processed = Some(candidate_id);
                continue;
            }

            if candidate_key {
                self.drop_older_than(candidate_id, now_us);
                self.state.need_keyframe = false;
                self.state.last_keyframe_request_us = None;
                self.state.next_request_interval_ms = KEYFRAME_REQUEST_MIN_INTERVAL_MS;
            }

            let completed = self.state.completed.remove(candidate_index);
            self.state.inflight_bytes = self
                .state
                .inflight_bytes
                .saturating_sub(completed.data.bytes.len());
            self.state.last_processed = Some(candidate_id);
            self.state.last_delivered = Some(candidate_id);
            self.state.counters.frames_delivered =
                self.state.counters.frames_delivered.saturating_add(1);
            events.push(ReassemblyEvent::FrameReady(completed.data));
            self.refresh_current();
        }
    }

    fn next_completed_index(&self) -> Option<usize> {
        if self.state.completed.is_empty() {
            return None;
        }
        if let Some(next) = self.state.last_processed.map(wrapping_next) {
            if let Some(index) = self
                .state
                .completed
                .iter()
                .position(|frame| frame.data.frame_id == next)
            {
                return Some(index);
            }
        }
        self.state
            .completed
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| serial_cmp(left.data.frame_id, right.data.frame_id))
            .map(|(index, _)| index)
    }

    fn drop_completed(&mut self, index: usize) {
        if index < self.state.completed.len() {
            let frame = Some(self.state.completed.remove(index));
            if let Some(frame) = frame {
                self.state.inflight_bytes = self
                    .state
                    .inflight_bytes
                    .saturating_sub(frame.data.bytes.len());
                self.state.loss.record(
                    frame.completed_at_us,
                    LossSample {
                        expected_fragments: u64::from(frame.data.fragment_count),
                        missing_fragments: u64::from(frame.data.fragment_count),
                        frames_observed: 1,
                        ..LossSample::default()
                    },
                );
            }
        }
        self.refresh_current();
    }

    fn declare_gap(&mut self, missing: u32, now_us: u64, events: &mut Vec<ReassemblyEvent>) {
        self.state.counters.dropped_gap = self
            .state
            .counters
            .dropped_gap
            .saturating_add(u64::from(missing));
        self.state.loss.record(
            now_us,
            LossSample {
                whole_frames_lost: u64::from(missing),
                frames_observed: u64::from(missing),
                ..LossSample::default()
            },
        );
        self.require_keyframe(now_us, events);
    }

    fn drop_partial(&mut self, frame_id: u32, reason: ReassemblyReason, now_us: u64) {
        if let Some(index) = self
            .state
            .partial
            .iter()
            .position(|frame| frame.metadata.frame_id == frame_id)
        {
            let frame = Some(self.state.partial.remove(index));
            if let Some(frame) = frame {
                self.state.inflight_bytes =
                    self.state.inflight_bytes.saturating_sub(frame.byte_len);
                let expected = u64::from(frame.metadata.fragment_count);
                let received = u64::try_from(frame.fragments.len()).unwrap_or(u64::MAX);
                self.state.loss.record(
                    now_us,
                    LossSample {
                        expected_fragments: expected,
                        missing_fragments: expected.saturating_sub(received),
                        frames_observed: 1,
                        ..LossSample::default()
                    },
                );
                match reason {
                    ReassemblyReason::Incomplete => {
                        self.state.counters.dropped_incomplete =
                            self.state.counters.dropped_incomplete.saturating_add(1);
                    }
                    ReassemblyReason::Inconsistent => {
                        // Counted at the point where the inconsistency is detected.
                    }
                    ReassemblyReason::Evicted => {
                        self.state.counters.dropped_evicted =
                            self.state.counters.dropped_evicted.saturating_add(1);
                    }
                    _ => {}
                }
            }
        }
        self.refresh_current();
    }

    fn evict_oldest(&mut self, incoming_id: u32, now_us: u64) -> bool {
        let partial_oldest = self
            .state
            .partial
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| serial_cmp(a.metadata.frame_id, b.metadata.frame_id));
        let completed_oldest = self
            .state
            .completed
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| serial_cmp(a.data.frame_id, b.data.frame_id));
        match (partial_oldest, completed_oldest) {
            (Some((pi, p)), Some((ci, c))) => {
                if serial_before(p.metadata.frame_id, c.data.frame_id) {
                    let id = p.metadata.frame_id;
                    self.drop_partial(id, ReassemblyReason::Evicted, now_us);
                } else {
                    self.drop_completed(ci);
                    self.state.counters.dropped_evicted =
                        self.state.counters.dropped_evicted.saturating_add(1);
                }
                let _ = pi;
                true
            }
            (Some((_, p)), None) => {
                let id = p.metadata.frame_id;
                self.drop_partial(id, ReassemblyReason::Evicted, now_us);
                true
            }
            (None, Some((ci, _))) => {
                self.drop_completed(ci);
                self.state.counters.dropped_evicted =
                    self.state.counters.dropped_evicted.saturating_add(1);
                true
            }
            (None, None) => {
                let _ = incoming_id;
                false
            }
        }
    }

    fn evict_timeouts(&mut self, now_us: u64, events: &mut Vec<ReassemblyEvent>) {
        let timeout_us = REASSEMBLY_TIMEOUT_MS.saturating_mul(MILLIS_TO_MICROS);
        let expired: Vec<u32> = self
            .state
            .partial
            .iter()
            .filter(|frame| now_us.saturating_sub(frame.last_activity_us) >= timeout_us)
            .map(|frame| frame.metadata.frame_id)
            .collect();
        for frame_id in expired {
            self.drop_partial(frame_id, ReassemblyReason::Incomplete, now_us);
            self.require_keyframe(now_us, events);
        }
    }

    fn drop_older_than(&mut self, frame_id: u32, now_us: u64) {
        let older_partial: Vec<u32> = self
            .state
            .partial
            .iter()
            .filter(|frame| serial_before(frame.metadata.frame_id, frame_id))
            .map(|frame| frame.metadata.frame_id)
            .collect();
        for id in older_partial {
            self.drop_partial(id, ReassemblyReason::Incomplete, now_us);
        }
        let older_completed: Vec<usize> = self
            .state
            .completed
            .iter()
            .enumerate()
            .filter(|(_, frame)| serial_before(frame.data.frame_id, frame_id))
            .map(|(index, _)| index)
            .collect();
        for index in older_completed.into_iter().rev() {
            self.drop_completed(index);
        }
    }

    fn inflight_frames(&self) -> usize {
        self.state
            .partial
            .len()
            .saturating_add(self.state.completed.len())
    }

    fn require_keyframe(&mut self, now_us: u64, events: &mut Vec<ReassemblyEvent>) {
        self.state.need_keyframe = true;
        if self.state.last_keyframe_request_us.is_none() {
            self.maybe_request_keyframe(now_us, events);
        }
    }

    fn maybe_request_keyframe(&mut self, now_us: u64, events: &mut Vec<ReassemblyEvent>) {
        if !self.state.need_keyframe {
            return;
        }
        if self
            .state
            .partial
            .iter()
            .any(|frame| frame.metadata.keyframe)
            || self.state.completed.iter().any(|frame| frame.data.keyframe)
        {
            return;
        }
        let previous_request = self.state.last_keyframe_request_us;
        let due = previous_request.is_none_or(|last| {
            now_us.saturating_sub(last)
                >= self
                    .state
                    .next_request_interval_ms
                    .saturating_mul(MILLIS_TO_MICROS)
        });
        if due {
            events.push(ReassemblyEvent::NeedKeyframe(self.state.epoch));
            self.state.last_keyframe_request_us = Some(now_us);
            self.state.counters.keyframe_requests =
                self.state.counters.keyframe_requests.saturating_add(1);
            if previous_request.is_some() {
                self.state.next_request_interval_ms = self
                    .state
                    .next_request_interval_ms
                    .saturating_mul(2)
                    .min(KEYFRAME_REQUEST_MAX_BACKOFF_MS);
            }
        }
    }

    fn refresh_current(&mut self) {
        self.state.counters.current_inflight_frames = self.inflight_frames();
        self.state.counters.current_inflight_bytes = self.state.inflight_bytes;
    }
}

fn wrapping_next(value: u32) -> u32 {
    value.wrapping_add(1)
}

fn serial_newer(candidate: u32, reference: u32) -> bool {
    let delta = candidate.wrapping_sub(reference);
    delta != 0 && delta < (1u32 << 31)
}

fn serial_before(left: u32, right: u32) -> bool {
    serial_newer(right, left)
}

fn serial_cmp(left: u32, right: u32) -> std::cmp::Ordering {
    if left == right {
        std::cmp::Ordering::Equal
    } else if serial_before(left, right) {
        std::cmp::Ordering::Less
    } else {
        std::cmp::Ordering::Greater
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fragment_count, slice_frame_into, SenderFrame};
    use proptest::prelude::*;
    use racc_proto::{encode_video_datagram, parse_video_datagram};
    use std::time::{Duration, Instant};

    fn sender_frame(frame_id: u32, keyframe: bool, len: usize) -> SenderFrame {
        let mut bytes = Vec::with_capacity(len);
        for index in 0..len {
            bytes.push((index.wrapping_add(frame_id as usize) % 251) as u8);
        }
        SenderFrame {
            epoch: 7,
            frame_id,
            keyframe,
            config: keyframe,
            capture_ts_us: frame_id.wrapping_mul(33_333),
            bytes,
        }
    }

    fn datagrams(frame: &SenderFrame) -> Vec<Vec<u8>> {
        let count = fragment_count(frame.bytes.len());
        assert!(count.is_ok());
        let count = match count {
            Ok(value) => value,
            Err(_) => return Vec::new(),
        };
        let mut output = (0..count)
            .map(|_| Vec::with_capacity(1200))
            .collect::<Vec<_>>();
        let sliced = slice_frame_into(frame, &mut output);
        assert!(sliced.is_ok());
        output
    }

    fn delivered(events: Vec<ReassemblyEvent>) -> Vec<FrameData> {
        events
            .into_iter()
            .filter_map(|event| match event {
                ReassemblyEvent::FrameReady(frame) => Some(frame),
                ReassemblyEvent::NeedKeyframe(_) | ReassemblyEvent::Cursor(_) => None,
            })
            .collect()
    }

    fn send_all(
        reassembler: &mut Reassembler,
        packets: &[Vec<u8>],
        now: &mut u64,
    ) -> Vec<FrameData> {
        let mut ready = Vec::new();
        for packet in packets {
            ready.extend(delivered(reassembler.push_datagram(packet, *now)));
            *now = now.saturating_add(100);
        }
        ready
    }

    fn one_fragment(frame_id: u32, keyframe: bool) -> Vec<u8> {
        datagrams(&sender_frame(frame_id, keyframe, 32))
            .into_iter()
            .next()
            .unwrap_or_default()
    }

    fn permute(values: &mut [usize], offset: usize, all: &mut Vec<Vec<usize>>) {
        if offset == values.len() {
            all.push(values.to_vec());
            return;
        }
        for index in offset..values.len() {
            values.swap(offset, index);
            permute(values, offset + 1, all);
            values.swap(offset, index);
        }
    }

    #[test]
    fn m2_constants_match_the_transport_contract() {
        assert_eq!(racc_proto::MAX_DATAGRAM, 1200);
        assert_eq!(racc_proto::VIDEO_HEADER_LEN, 18);
        assert_eq!(racc_proto::MAX_VIDEO_PAYLOAD, 1182);
        assert_eq!(racc_proto::MAX_FRAGMENTS_PER_FRAME, 1024);
        assert_eq!(MAX_INFLIGHT_FRAMES, 4);
        assert_eq!(MAX_INFLIGHT_BYTES, 4 * 1024 * 1024);
        assert_eq!(REASSEMBLY_TIMEOUT_MS, 100);
        assert_eq!(REORDER_WINDOW_MS, 8);
        assert_eq!(KEYFRAME_REQUEST_MIN_INTERVAL_MS, 200);
        assert_eq!(KEYFRAME_REQUEST_MAX_BACKOFF_MS, 1000);
        assert_eq!(crate::LOSS_WINDOW_US, 2_000_000);
        assert_eq!(crate::MAX_LOSS_BUCKETS, 256);
        assert_eq!(crate::THREAD_JOIN_TIMEOUT_MS, 1000);
        assert_eq!(crate::DEFAULT_FRAME_INTERVAL_US, 33_333);
        assert_eq!(crate::PACING_FRACTION_PERCENT, 60);
        assert_eq!(crate::SEND_QUEUE_MAX_FRAMES, 2);
        assert_eq!(crate::SEND_BUFFER_BYTES, 1024 * 1024);
        assert_eq!(crate::RECEIVE_BUFFER_BYTES, 2 * 1024 * 1024);
        assert_eq!(crate::RECEIVE_POLL_INTERVAL_MS, 5);
        assert_eq!(crate::DEFAULT_KEEPALIVE_IDLE, Duration::from_secs(10));
        assert_eq!(crate::DEFAULT_CONTROL_WRITE_TIMEOUT, Duration::from_secs(2));
    }

    #[test]
    #[ignore = "manual release-mode M2 reassembly throughput measurement"]
    fn release_mode_reassembly_microbenchmark_reports_datagrams_per_second() {
        use std::hint::black_box;

        let packet_count = 100_000u32;
        let mut packets = Vec::with_capacity(packet_count as usize);
        for frame_id in 0..packet_count {
            let frame = SenderFrame {
                epoch: 7,
                frame_id,
                keyframe: frame_id == 0,
                config: frame_id == 0,
                capture_ts_us: frame_id.wrapping_mul(33_333),
                bytes: vec![0x41; 32],
            };
            let mut output = vec![Vec::with_capacity(64)];
            assert!(slice_frame_into(&frame, &mut output).is_ok());
            if let Some(packet) = output.pop() {
                packets.push(packet);
            }
        }
        assert_eq!(packets.len(), packet_count as usize);

        let mut reassembler = Reassembler::new(7);
        let started = Instant::now();
        for (index, packet) in packets.iter().enumerate() {
            let now_us = u64::try_from(index)
                .unwrap_or(u64::MAX)
                .saturating_mul(1000);
            black_box(reassembler.push_datagram(black_box(packet), now_us));
        }
        let elapsed = started.elapsed();
        let stats = reassembler.stats(u64::MAX);
        assert_eq!(stats.counters.frames_delivered, u64::from(packet_count));
        let packets_per_second = f64::from(packet_count) / elapsed.as_secs_f64();
        println!(
            "REASSEMBLER release datagrams={} elapsed_ms={:.3} datagrams_per_second={:.0} peak_inflight_bytes={}",
            packet_count,
            elapsed.as_secs_f64() * 1000.0,
            packets_per_second,
            stats.counters.current_inflight_bytes
        );
    }
    #[test]
    fn slicer_boundaries_produce_uniform_protocol_fragments_and_round_trip() {
        let max = MAX_FRAGMENTS_PER_FRAME * MAX_VIDEO_PAYLOAD;
        for len in [1, 1181, 1182, 1183, 2364, 2365, max] {
            let frame = sender_frame(0x1234, true, len);
            let packets = datagrams(&frame);
            assert_eq!(packets.len(), len.div_ceil(MAX_VIDEO_PAYLOAD));
            let mut reassembler = Reassembler::new(frame.epoch);
            let ready = send_all(&mut reassembler, &packets, &mut 10);
            assert_eq!(ready.len(), 1, "length {len}");
            assert_eq!(ready[0].bytes, frame.bytes, "length {len}");
            for (index, packet) in packets.iter().enumerate() {
                let parsed = parse_video_datagram(packet);
                assert!(matches!(parsed, Ok(VideoDatagram::Video { .. })));
                if let Ok(VideoDatagram::Video { header, payload }) = parsed {
                    let is_last = index + 1 == packets.len();
                    assert_eq!(header.frag_idx as usize, index);
                    assert_eq!(header.frag_cnt as usize, packets.len());
                    assert_eq!(header.flags & VIDEO_FLAG_LAST_FRAGMENT != 0, is_last);
                    assert!(packet.len() <= racc_proto::MAX_DATAGRAM);
                    assert_eq!(
                        payload.len(),
                        if is_last {
                            len - index * MAX_VIDEO_PAYLOAD
                        } else {
                            MAX_VIDEO_PAYLOAD
                        }
                    );
                }
            }
        }
        assert_eq!(fragment_count(0), Err(SliceError::Empty));
        assert_eq!(fragment_count(max + 1), Err(SliceError::TooLarge));
    }

    #[test]
    fn five_fragment_frame_delivers_for_every_fragment_permutation() {
        let frame = sender_frame(1, true, 4 * MAX_VIDEO_PAYLOAD + 9);
        let packets = datagrams(&frame);
        assert_eq!(packets.len(), 5);
        let mut orders = Vec::new();
        permute(&mut [0, 1, 2, 3, 4], 0, &mut orders);
        assert_eq!(orders.len(), 120);
        for order in orders {
            let mut reassembler = Reassembler::new(frame.epoch);
            let mut ready = Vec::new();
            for index in order {
                ready.extend(delivered(reassembler.push_datagram(&packets[index], 100)));
            }
            assert_eq!(ready.len(), 1);
            assert_eq!(ready[0].bytes, frame.bytes);
        }
    }

    #[test]
    fn in_order_duplicates_missing_fragments_and_stale_epochs_are_counted() {
        let frame = sender_frame(10, true, 2 * MAX_VIDEO_PAYLOAD);
        let packets = datagrams(&frame);
        let mut reassembler = Reassembler::new(frame.epoch);
        let mut all = Vec::new();
        all.extend(delivered(reassembler.push_datagram(&packets[0], 0)));
        all.extend(delivered(reassembler.push_datagram(&packets[0], 1)));
        all.extend(delivered(reassembler.push_datagram(&packets[1], 2)));
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].bytes, frame.bytes);
        let _ = reassembler.push_datagram(&one_fragment(11, true), 3);
        let stats = reassembler.stats(3);
        assert_eq!(stats.counters.duplicates, 1);

        let wrong_epoch = sender_frame(12, true, 1);
        let mut wrong = datagrams(&wrong_epoch).remove(0);
        wrong[4] = 8;
        let _ = reassembler.push_datagram(&wrong, 4);
        assert_eq!(reassembler.stats(4).counters.wrong_epoch, 1);

        let reset = reassembler.reset(8, 5);
        assert!(reset.contains(&ReassemblyEvent::NeedKeyframe(8)));
        assert_eq!(reassembler.epoch(), 8);
        assert_eq!(reassembler.stats(5).counters.current_inflight_frames, 0);
    }

    #[test]
    fn missing_fragment_times_out_and_requests_a_keyframe() {
        let frame = sender_frame(1, true, 2 * MAX_VIDEO_PAYLOAD);
        let packets = datagrams(&frame);
        let mut reassembler = Reassembler::new(frame.epoch);
        let first = reassembler.push_datagram(&packets[0], 1000);
        assert!(first
            .iter()
            .all(|event| !matches!(event, ReassemblyEvent::NeedKeyframe(7))));
        let expired = reassembler.poll(101_000);
        assert!(expired
            .iter()
            .any(|event| matches!(event, ReassemblyEvent::NeedKeyframe(7))));
        let early_retry = reassembler.poll(300_999);
        assert!(early_retry.is_empty());
        let retried = reassembler.poll(301_000);
        assert!(retried
            .iter()
            .any(|event| matches!(event, ReassemblyEvent::NeedKeyframe(7))));
        let stats = reassembler.stats(301_000);
        assert_eq!(stats.counters.dropped_incomplete, 1);
        assert!(stats.loss.packet_loss_fraction > 0.0);
        assert_eq!(stats.counters.current_inflight_frames, 0);
    }

    #[test]
    fn frame_gap_discards_predictive_frames_until_a_keyframe_arrives() {
        let mut reassembler = Reassembler::new(7);
        let mut time = 0;
        assert_eq!(
            send_all(&mut reassembler, &[one_fragment(10, true)], &mut time).len(),
            1
        );
        assert!(send_all(&mut reassembler, &[one_fragment(12, false)], &mut time).is_empty());
        assert!(send_all(&mut reassembler, &[one_fragment(13, false)], &mut time).is_empty());
        let recovered = send_all(&mut reassembler, &[one_fragment(14, true)], &mut time);
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].frame_id, 14);
        let stats = reassembler.stats(time);
        assert_eq!(stats.counters.dropped_gap, 1);
        assert_eq!(stats.counters.dropped_waiting_key, 2);
        assert!(!stats.need_keyframe);
    }

    #[test]
    fn reordering_inside_window_delivers_in_order_without_a_request() {
        let mut reassembler = Reassembler::new(7);
        let mut time = 0;
        let mut ready = send_all(&mut reassembler, &[one_fragment(0, true)], &mut time);
        let first = sender_frame(1, false, 2 * MAX_VIDEO_PAYLOAD);
        let first_packets = datagrams(&first);
        let later = one_fragment(2, false);
        ready.extend(delivered(
            reassembler.push_datagram(&first_packets[0], 1000),
        ));
        ready.extend(delivered(reassembler.push_datagram(&later, 2000)));
        assert!(delivered(reassembler.poll(3000)).is_empty());
        ready.extend(delivered(
            reassembler.push_datagram(&first_packets[1], 4000),
        ));
        assert_eq!(
            ready.iter().map(|frame| frame.frame_id).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(reassembler.stats(4000).counters.keyframe_requests, 0);
    }

    #[test]
    fn reordering_past_window_declares_loss_and_rejects_predictive_frame() {
        let mut reassembler = Reassembler::new(7);
        let mut time = 0;
        let _ = send_all(&mut reassembler, &[one_fragment(0, true)], &mut time);
        let earlier = sender_frame(1, false, 2 * MAX_VIDEO_PAYLOAD);
        let earlier_packets = datagrams(&earlier);
        let _ = reassembler.push_datagram(&earlier_packets[0], 1000);
        let _ = reassembler.push_datagram(&one_fragment(2, false), 2000);
        let expired = reassembler.poll(10_000);
        assert!(delivered(expired.clone()).is_empty());
        assert!(expired
            .iter()
            .any(|event| matches!(event, ReassemblyEvent::NeedKeyframe(7))));
        let stats = reassembler.stats(10_000);
        assert_eq!(stats.counters.dropped_incomplete, 1);
        assert_eq!(stats.counters.dropped_gap, 0);
        assert!(stats.need_keyframe);
        assert!(send_all(&mut reassembler, &[one_fragment(3, true)], &mut time).len() == 1);
    }

    #[test]
    fn inconsistent_metadata_and_nonuniform_nonlast_fragment_drop_the_frame() {
        let first_frame = sender_frame(1, false, 2 * MAX_VIDEO_PAYLOAD);
        let first_packets = datagrams(&first_frame);
        let mut reassembler = Reassembler::new(7);
        let _ = reassembler.push_datagram(&first_packets[0], 0);

        let mut inconsistent_header = VideoHeader {
            version: PROTOCOL_VERSION,
            flags: VIDEO_FLAG_CONFIG | VIDEO_FLAG_KEY | VIDEO_FLAG_LAST_FRAGMENT,
            epoch: 7,
            frame_id: 1,
            frag_idx: 1,
            frag_cnt: 2,
            capture_ts_us: first_frame.capture_ts_us,
        };
        let mut inconsistent = Vec::new();
        let encoded = encode_video_datagram(
            inconsistent_header,
            &first_frame.bytes[MAX_VIDEO_PAYLOAD..],
            &mut inconsistent,
        );
        assert!(encoded.is_ok());
        let _ = reassembler.push_datagram(&inconsistent, 1);
        assert_eq!(reassembler.stats(1).counters.dropped_inconsistent, 1);

        inconsistent_header.frame_id = 2;
        inconsistent_header.flags = 0;
        inconsistent_header.frag_idx = 0;
        inconsistent_header.capture_ts_us = 1;
        let mut short_nonlast = Vec::new();
        let encoded = encode_video_datagram(inconsistent_header, &[1, 2, 3], &mut short_nonlast);
        assert!(encoded.is_ok());
        let _ = reassembler.push_datagram(&short_nonlast, 2);
        assert!(reassembler.stats(2).counters.dropped_inconsistent >= 2);
    }

    #[test]
    fn stale_frames_and_wrapping_frame_identifiers_follow_serial_arithmetic() {
        let mut reassembler = Reassembler::new(7);
        let mut time = 0;
        let _ = send_all(&mut reassembler, &[one_fragment(10, true)], &mut time);
        let _ = reassembler.push_datagram(&one_fragment(9, true), time);
        assert_eq!(reassembler.stats(time).counters.late, 1);

        let mut wrap = Reassembler::new(7);
        let ready = send_all(
            &mut wrap,
            &[
                one_fragment(u32::MAX - 1, true),
                one_fragment(u32::MAX, false),
                one_fragment(0, false),
            ],
            &mut 0,
        );
        assert_eq!(
            ready.iter().map(|frame| frame.frame_id).collect::<Vec<_>>(),
            [u32::MAX - 1, u32::MAX, 0]
        );
    }

    #[test]
    fn completion_eviction_recomputes_the_partial_frame_index() {
        let mut reassembler = Reassembler::new(7);
        let mut now = 0u64;
        for frame_id in 0..2 {
            let frame = sender_frame(frame_id, false, 900_000);
            let packets = datagrams(&frame);
            for packet in packets.iter().take(packets.len().saturating_sub(1)) {
                let _ = reassembler.push_datagram(packet, now);
                now = now.saturating_add(10);
            }
        }
        let keyframe = sender_frame(2, true, MAX_FRAGMENTS_PER_FRAME * MAX_VIDEO_PAYLOAD);
        let packets = datagrams(&keyframe);
        let mut ready = Vec::new();
        for packet in &packets {
            ready.extend(delivered(reassembler.push_datagram(packet, now)));
            now = now.saturating_add(10);
        }
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].frame_id, 2);
        assert_eq!(ready[0].bytes, keyframe.bytes);
        let stats = reassembler.stats(now);
        assert_eq!(stats.counters.dropped_evicted, 1);
        assert!(stats.counters.current_inflight_bytes <= MAX_INFLIGHT_BYTES);
    }
    #[test]
    fn frame_count_and_byte_budget_evict_oldest_state() {
        let mut by_count = Reassembler::new(7);
        for frame_id in 0..=MAX_INFLIGHT_FRAMES as u32 {
            let frame = sender_frame(frame_id, false, 2 * MAX_VIDEO_PAYLOAD);
            let packets = datagrams(&frame);
            let _ = by_count.push_datagram(&packets[0], u64::from(frame_id));
        }
        let count_stats = by_count.stats(10);
        assert_eq!(
            count_stats.counters.current_inflight_frames,
            MAX_INFLIGHT_FRAMES
        );
        assert_eq!(count_stats.counters.dropped_evicted, 1);

        let mut by_bytes = Reassembler::with_byte_budget(7, MAX_VIDEO_PAYLOAD);
        for frame_id in 0..2 {
            let frame = sender_frame(frame_id, false, 2 * MAX_VIDEO_PAYLOAD);
            let packets = datagrams(&frame);
            let _ = by_bytes.push_datagram(&packets[0], u64::from(frame_id));
        }
        let byte_stats = by_bytes.stats(10);
        assert!(byte_stats.counters.current_inflight_bytes <= MAX_VIDEO_PAYLOAD);
        assert_eq!(byte_stats.counters.current_inflight_frames, 1);
        assert!(byte_stats.counters.dropped_evicted >= 1);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn arbitrary_datagrams_never_panic_and_memory_stays_bounded(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
            let mut reassembler = Reassembler::new(7);
            let _ = reassembler.push_datagram(&bytes, 0);
            let stats = reassembler.stats(0);
            prop_assert!(stats.counters.current_inflight_frames <= MAX_INFLIGHT_FRAMES);
            prop_assert!(stats.counters.current_inflight_bytes <= MAX_INFLIGHT_BYTES);
        }

        #[test]
        fn randomized_loss_and_duplicates_preserve_identity_order_and_gap_safety(
            operations in prop::collection::vec((any::<bool>(), any::<bool>(), any::<bool>()), 1..80)
        ) {
            let mut reassembler = Reassembler::new(7);
            let mut delivered_frames: Vec<FrameData> = Vec::new();
            let mut time = 0u64;
            for (index, (key, drop_frame, duplicate)) in operations.into_iter().enumerate() {
                let frame_id = 100u32.wrapping_add(index as u32);
                let key = index == 0 || key;
                if drop_frame && index != 0 {
                    time = time.saturating_add(1000);
                    continue;
                }
                let packet = one_fragment(frame_id, key);
                let mut events = reassembler.push_datagram(&packet, time);
                if duplicate {
                    events.extend(reassembler.push_datagram(&packet, time.saturating_add(1)));
                }
                for frame in delivered(events) {
                    prop_assert_eq!(&frame.bytes, &sender_frame(frame.frame_id, frame.keyframe, 32).bytes);
                    if let Some(previous) = delivered_frames.last() {
                        prop_assert!(serial_newer(frame.frame_id, previous.frame_id));
                        if !frame.keyframe {
                            prop_assert_eq!(frame.frame_id, previous.frame_id.wrapping_add(1));
                        }
                    }
                    delivered_frames.push(frame);
                }
                time = time.saturating_add(1000);
            }
        }

        #[test]
        fn hostile_valid_headers_never_exceed_frame_or_byte_budgets(
            packets in prop::collection::vec((any::<u32>(), any::<u16>(), any::<u8>()), 1..128)
        ) {
            let mut reassembler = Reassembler::new(7);
            for (frame_id, random_index, marker) in packets {
                let index = random_index % (MAX_FRAGMENTS_PER_FRAME as u16);
                let last = index + 1 == MAX_FRAGMENTS_PER_FRAME as u16;
                let payload = vec![marker; if last { 7 } else { MAX_VIDEO_PAYLOAD }];
                let header = VideoHeader {
                    version: PROTOCOL_VERSION,
                    flags: if last { VIDEO_FLAG_LAST_FRAGMENT } else { 0 },
                    epoch: 7,
                    frame_id,
                    frag_idx: index,
                    frag_cnt: MAX_FRAGMENTS_PER_FRAME as u16,
                    capture_ts_us: frame_id,
                };
                let mut datagram = Vec::new();
                let encoded = encode_video_datagram(header, &payload, &mut datagram);
                prop_assert!(encoded.is_ok());
                let _ = reassembler.push_datagram(&datagram, u64::from(frame_id));
                let stats = reassembler.stats(u64::from(frame_id));
                prop_assert!(stats.counters.current_inflight_frames <= MAX_INFLIGHT_FRAMES);
                prop_assert!(stats.counters.current_inflight_bytes <= MAX_INFLIGHT_BYTES);
            }
        }
    }
}

#[cfg(test)]
mod keyframe_backoff_tests {
    use super::*;
    use crate::SenderFrame;

    #[test]
    fn requests_are_immediate_then_exponentially_backed_off_and_reset_after_keyframe() {
        let frame = SenderFrame {
            epoch: 7,
            frame_id: 1,
            keyframe: false,
            config: false,
            capture_ts_us: 1,
            bytes: vec![1],
        };
        let mut packets = vec![Vec::new()];
        assert!(slice_frame_into(&frame, &mut packets).is_ok());
        let mut reassembler = Reassembler::new(7);
        let immediate = reassembler.push_datagram(&packets[0], 0);
        assert!(immediate
            .iter()
            .any(|event| matches!(event, ReassemblyEvent::NeedKeyframe(7))));
        for (now, expected) in [
            (199_999, false),
            (200_000, true),
            (599_999, false),
            (600_000, true),
            (1_399_999, false),
            (1_400_000, true),
            (2_399_999, false),
            (2_400_000, true),
            (3_399_999, false),
        ] {
            let events = reassembler.poll(now);
            assert_eq!(
                events
                    .iter()
                    .any(|event| matches!(event, ReassemblyEvent::NeedKeyframe(7))),
                expected,
                "unexpected retry at {now}"
            );
        }
        let keyframe = SenderFrame {
            epoch: 7,
            frame_id: 2,
            keyframe: true,
            config: true,
            capture_ts_us: 2,
            bytes: vec![2],
        };
        let mut key_packet = vec![Vec::new()];
        assert!(slice_frame_into(&keyframe, &mut key_packet).is_ok());
        let recovered = reassembler.push_datagram(&key_packet[0], 3_400_000);
        assert!(recovered
            .iter()
            .any(|event| matches!(event, ReassemblyEvent::FrameReady(_))));
        assert!(reassembler.poll(10_000_000).is_empty());
        assert!(!reassembler.stats(10_000_000).need_keyframe);
    }
}
