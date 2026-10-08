//! Bounded input queue for complete compressed access units.

use std::collections::VecDeque;

use crate::EncodedAccessUnit;

/// Number of compressed access units retained before the decoder worker.
pub const DECODER_INPUT_QUEUE_CAPACITY: usize = 2;

/// Outcome of adding one access unit to the bounded decoder queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuePush {
    /// The access unit was appended to the queue.
    Enqueued,
    /// Older queued data was discarded and the independent IDR was retained.
    ReplacedWithKeyframe {
        /// Number of queued frames discarded in favor of the IDR.
        dropped: usize,
    },
    /// Data was discarded until a future IDR can safely restart decoding.
    DroppedAwaitingKeyframe {
        /// Number of the incoming and queued frames discarded.
        dropped: usize,
    },
    /// The access unit belongs to an epoch other than the queue's current epoch.
    WrongEpoch {
        /// Epoch configured in this queue.
        expected: u16,
        /// Epoch carried by the rejected frame.
        received: u16,
    },
}

/// A fixed-capacity queue that never drops an arbitrary reference frame silently.
///
/// On overflow, queued dependent frames are discarded and predictive frames are
/// rejected until a new IDR arrives. An incoming IDR replaces queued backlog.
pub struct DecoderInputQueue {
    epoch: u16,
    awaiting_keyframe: bool,
    frames: VecDeque<EncodedAccessUnit>,
}

impl DecoderInputQueue {
    /// Creates an empty queue for an epoch; only an IDR is initially accepted.
    pub fn new(epoch: u16) -> Self {
        Self {
            epoch,
            awaiting_keyframe: true,
            frames: VecDeque::with_capacity(DECODER_INPUT_QUEUE_CAPACITY),
        }
    }

    /// Clears pending data for a new epoch and returns to keyframe-waiting state.
    pub fn reset(&mut self, epoch: u16) {
        self.epoch = epoch;
        self.awaiting_keyframe = true;
        self.frames.clear();
    }

    /// Adds one frame while keeping memory bounded and preserving H.264 dependencies.
    pub fn push(&mut self, frame: EncodedAccessUnit) -> QueuePush {
        if frame.epoch != self.epoch {
            return QueuePush::WrongEpoch {
                expected: self.epoch,
                received: frame.epoch,
            };
        }
        if self.awaiting_keyframe && !frame.is_keyframe() {
            return QueuePush::DroppedAwaitingKeyframe { dropped: 1 };
        }
        if frame.is_keyframe() {
            let dropped = self.frames.len();
            self.frames.clear();
            self.frames.push_back(frame);
            self.awaiting_keyframe = false;
            return if dropped == 0 {
                QueuePush::Enqueued
            } else {
                QueuePush::ReplacedWithKeyframe { dropped }
            };
        }
        if self.frames.len() == DECODER_INPUT_QUEUE_CAPACITY {
            let dropped = self.frames.len().saturating_add(1);
            self.frames.clear();
            self.awaiting_keyframe = true;
            return QueuePush::DroppedAwaitingKeyframe { dropped };
        }
        self.frames.push_back(frame);
        QueuePush::Enqueued
    }

    /// Removes the oldest queued access unit for sequential decode.
    pub fn pop(&mut self) -> Option<EncodedAccessUnit> {
        self.frames.pop_front()
    }

    /// Number of queued units, always at most the configured capacity.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether the queue currently has no pending units.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Whether predictive frames are currently being rejected while awaiting an IDR.
    pub const fn awaiting_keyframe(&self) -> bool {
        self.awaiting_keyframe
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EncodedAccessUnit;

    fn unit(epoch: u16, frame_id: u32, idr: bool) -> EncodedAccessUnit {
        let bytes = if idr {
            vec![0, 0, 1, 0x65, 0x88]
        } else {
            vec![0, 0, 1, 0x41, 0x88]
        };
        EncodedAccessUnit::new(epoch, frame_id, frame_id, idr, false, bytes)
            .expect("valid fake access unit")
    }

    #[test]
    fn queue_waits_for_idr_and_keeps_at_most_two_access_units() {
        let mut queue = DecoderInputQueue::new(1);
        assert_eq!(
            queue.push(unit(1, 1, false)),
            QueuePush::DroppedAwaitingKeyframe { dropped: 1 }
        );
        assert_eq!(queue.push(unit(1, 2, true)), QueuePush::Enqueued);
        assert_eq!(queue.push(unit(1, 3, false)), QueuePush::Enqueued);
        assert_eq!(queue.len(), DECODER_INPUT_QUEUE_CAPACITY);
        assert_eq!(
            queue.push(unit(1, 4, false)),
            QueuePush::DroppedAwaitingKeyframe { dropped: 3 }
        );
        assert!(queue.is_empty());
        assert!(queue.awaiting_keyframe());
        assert_eq!(
            queue.push(unit(1, 5, false)),
            QueuePush::DroppedAwaitingKeyframe { dropped: 1 }
        );
        assert_eq!(queue.push(unit(1, 6, true)), QueuePush::Enqueued);
        assert!(!queue.awaiting_keyframe());
    }

    #[test]
    fn newer_idr_replaces_queued_backlog_and_epoch_reset_discards_it() {
        let mut queue = DecoderInputQueue::new(7);
        queue.push(unit(7, 1, true));
        queue.push(unit(7, 2, false));
        assert_eq!(
            queue.push(unit(7, 3, true)),
            QueuePush::ReplacedWithKeyframe { dropped: 2 }
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.pop().map(|frame| frame.frame_id), Some(3));
        queue.push(unit(7, 4, true));
        queue.reset(8);
        assert!(queue.is_empty());
        assert!(queue.awaiting_keyframe());
        assert_eq!(
            queue.push(unit(7, 5, true)),
            QueuePush::WrongEpoch {
                expected: 8,
                received: 7
            }
        );
    }
}
