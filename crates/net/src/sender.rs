use std::collections::VecDeque;

/// One owned encoded frame waiting for paced packet transmission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SenderFrame {
    /// Protocol stream epoch.
    pub epoch: u16,
    /// Monotonic frame identifier within the epoch.
    pub frame_id: u32,
    /// Whether this frame is an IDR/keyframe.
    pub keyframe: bool,
    /// Whether this frame contains SPS/PPS configuration.
    pub config: bool,
    /// Host capture time in wrapping microseconds.
    pub capture_ts_us: u32,
    /// Encoded Annex B access-unit bytes.
    pub bytes: Vec<u8>,
}

/// Request that the encoder produce a fresh IDR after a frame is dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForceKeyframe {
    /// Stream epoch to which the request applies.
    pub epoch: u16,
    /// Frame whose queued bytes were discarded.
    pub dropped_frame_id: u32,
}

/// Result of inserting a frame into the bounded sender queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuePush {
    /// No frame was evicted.
    Queued,
    /// The oldest frame which had not started sending was evicted.
    DroppedOldest(ForceKeyframe),
}

/// A monotonically timed frame-send schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PacerSchedule {
    /// Absolute microsecond send times on the caller's monotonic timeline.
    pub send_at_us: Vec<u64>,
    /// First-send time used for this frame.
    pub starts_at_us: u64,
    /// Last-send time used for this frame.
    pub ends_at_us: u64,
}

/// One scheduled datagram and its send time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduledSend {
    /// Zero-based fragment index.
    pub fragment_index: usize,
    /// Absolute microsecond send time.
    pub send_at_us: u64,
}

/// Deterministic packet pacer. It does not read the system clock or sleep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pacer {
    next_available_us: u64,
}

impl Pacer {
    /// Creates an idle pacer.
    pub const fn new() -> Self {
        Self {
            next_available_us: 0,
        }
    }

    /// Schedules a frame's packets evenly within the configured send window.
    ///
    /// The caller supplies the microsecond clock, frame rate interval and
    /// fraction limit. Frames arriving during an earlier schedule start only
    /// after it has completed.
    pub fn schedule(
        &mut self,
        now_us: u64,
        datagram_count: usize,
        frame_interval_us: u64,
        fraction_percent: u8,
    ) -> PacerSchedule {
        let start = now_us.max(self.next_available_us);
        let span = u128::from(frame_interval_us).saturating_mul(u128::from(fraction_percent)) / 100;
        let span_u64 = u64::try_from(span).unwrap_or(u64::MAX);
        let mut send_at_us = Vec::with_capacity(datagram_count);
        if datagram_count == 1 {
            send_at_us.push(start);
        } else if datagram_count > 1 {
            let denominator = u128::try_from(datagram_count - 1).unwrap_or(u128::MAX);
            for index in 0..datagram_count {
                let offset =
                    span.saturating_mul(u128::try_from(index).unwrap_or(u128::MAX)) / denominator;
                let offset = u64::try_from(offset).unwrap_or(u64::MAX);
                send_at_us.push(start.saturating_add(offset));
            }
        }
        let end = send_at_us.last().copied().unwrap_or(start);
        self.next_available_us = end.saturating_add(1).max(start.saturating_add(span_u64));
        PacerSchedule {
            send_at_us,
            starts_at_us: start,
            ends_at_us: end,
        }
    }

    /// Returns the next scheduled time, for diagnostics and tests.
    pub const fn next_available_us(&self) -> u64 {
        self.next_available_us
    }
}

/// Bounded latest-wins queue for frames which have not started sending.
#[derive(Clone, Debug)]
pub struct FrameQueue {
    capacity: usize,
    frames: VecDeque<SenderFrame>,
}

impl FrameQueue {
    /// Creates a frame queue with a fixed nonzero waiting capacity.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            frames: VecDeque::with_capacity(capacity),
        }
    }

    /// Queues a frame, evicting the oldest waiting frame if the queue is full.
    pub fn push(&mut self, frame: SenderFrame) -> QueuePush {
        let result = if self.frames.len() >= self.capacity {
            match self.frames.pop_front() {
                Some(oldest) => QueuePush::DroppedOldest(ForceKeyframe {
                    epoch: oldest.epoch,
                    dropped_frame_id: oldest.frame_id,
                }),
                None => QueuePush::Queued,
            }
        } else {
            QueuePush::Queued
        };
        self.frames.push_back(frame);
        result
    }

    /// Removes the next frame when a sender is ready to begin it.
    pub fn pop(&mut self) -> Option<SenderFrame> {
        self.frames.pop_front()
    }

    /// Number of frames which have not started sending.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether no frames are waiting.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacer_spreads_packets_within_sixty_percent_and_serializes_frames() {
        for count in [1usize, 2, 5, 1024] {
            let mut pacer = Pacer::new();
            let schedule = pacer.schedule(100, count, 33_333, 60);
            assert_eq!(schedule.send_at_us.len(), count);
            assert!(schedule.ends_at_us.saturating_sub(schedule.starts_at_us) <= 19_999);
            assert!(schedule
                .send_at_us
                .windows(2)
                .all(|pair| pair[0] <= pair[1]));
            if count > 2 {
                let intervals = schedule
                    .send_at_us
                    .windows(2)
                    .map(|pair| pair[1].saturating_sub(pair[0]))
                    .collect::<Vec<_>>();
                let min_interval = intervals.iter().copied().min().unwrap_or(0);
                let max_interval = intervals.iter().copied().max().unwrap_or(0);
                assert!(max_interval.saturating_sub(min_interval) <= 1);
            }
            let next = pacer.schedule(200, 2, 33_333, 60);
            assert!(next.starts_at_us > schedule.ends_at_us);
        }
    }

    #[test]
    fn queue_overflow_drops_only_the_oldest_unstarted_frame() {
        let mut queue = FrameQueue::new(2);
        for frame_id in 1..=2 {
            assert_eq!(
                queue.push(SenderFrame {
                    epoch: 4,
                    frame_id,
                    keyframe: false,
                    config: false,
                    capture_ts_us: frame_id,
                    bytes: vec![frame_id as u8],
                }),
                QueuePush::Queued
            );
        }
        assert_eq!(
            queue.push(SenderFrame {
                epoch: 4,
                frame_id: 3,
                keyframe: false,
                config: false,
                capture_ts_us: 3,
                bytes: vec![3],
            }),
            QueuePush::DroppedOldest(ForceKeyframe {
                epoch: 4,
                dropped_frame_id: 1
            })
        );
        assert_eq!(queue.pop().map(|frame| frame.frame_id), Some(2));
        assert_eq!(queue.pop().map(|frame| frame.frame_id), Some(3));
    }
}
