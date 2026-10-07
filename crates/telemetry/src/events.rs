use core::fmt;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Maximum number of events retained by each event log.
pub const EVENT_LOG_CAPACITY: usize = 256;
/// Maximum UTF-8 detail length retained for one event.
pub const EVENT_DETAIL_MAX_BYTES: usize = 160;

static NEXT_EVENT_ID: AtomicU64 = AtomicU64::new(1);

/// Lifecycle and quality event category exposed to the UI.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EventKind {
    /// A session completed its handshake.
    ConnectionEstablished,
    /// A session lost its connection.
    ConnectionLost,
    /// A monitor switch completed or was requested.
    DisplaySwitch,
    /// The encoded stream epoch changed.
    StreamReset,
    /// The decoder was reset after an error or stall.
    DecoderReset,
    /// The host selected a different quality tier.
    QualityAdjustment,
    /// Packet loss affected a frame or transport interval.
    PacketLossEvent,
    /// Capture stopped producing frames.
    CaptureLost,
    /// Capture resumed after recovery.
    CaptureRecovered,
    /// The encoder fell back to another backend.
    EncoderFallback,
    /// A required operating-system permission is missing.
    PermissionMissing,
    /// A peer was rejected by the host allowlist.
    PeerRejected,
    /// Video was paused.
    Paused,
    /// Video resumed.
    Resumed,
}

/// One bounded event with a process-unique monotonic identifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event {
    /// Monotonic process event identifier.
    pub id: u64,
    /// Injected monotonic timestamp in microseconds.
    pub ts_us: u64,
    /// Event category.
    pub kind: EventKind,
    /// UTF-8 detail truncated at a character boundary.
    pub detail: String,
}

/// Event log construction and identifier errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventLogError {
    /// Process-wide event identifiers have been exhausted.
    IdExhausted,
}

impl fmt::Display for EventLogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("process event identifier space exhausted")
    }
}

impl std::error::Error for EventLogError {}

/// Cheaply cloned immutable event-log view.
#[derive(Clone, Debug, Default)]
pub struct EventLogSnapshot {
    events: Arc<[Event]>,
    /// Number of retained events omitted since this log was created.
    pub dropped: u64,
}

impl EventLogSnapshot {
    /// Returns the retained event slice.
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Returns the newest retained event identifier, if present.
    pub fn newest_id(&self) -> Option<u64> {
        self.events.last().map(|event| event.id)
    }
}

/// A fixed-capacity event ring that drops its oldest event when full.
#[derive(Clone, Debug, Default)]
pub struct EventLog {
    events: VecDeque<Event>,
    dropped: u64,
}

impl EventLog {
    /// Creates an empty event log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an event, truncating long details and assigning a process-wide id.
    pub fn push(
        &mut self,
        ts_us: u64,
        kind: EventKind,
        detail: impl Into<String>,
    ) -> Result<Event, EventLogError> {
        let id = next_event_id()?;
        let mut detail = detail.into();
        if detail.len() > EVENT_DETAIL_MAX_BYTES {
            let mut boundary = EVENT_DETAIL_MAX_BYTES;
            while !detail.is_char_boundary(boundary) {
                boundary -= 1;
            }
            detail.truncate(boundary);
        }
        let event = Event {
            id,
            ts_us,
            kind,
            detail,
        };
        if self.events.len() == EVENT_LOG_CAPACITY {
            self.events.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.events.push_back(event.clone());
        Ok(event)
    }

    /// Returns an immutable, cheaply cloned snapshot of the complete ring.
    pub fn snapshot(&self) -> EventLogSnapshot {
        EventLogSnapshot {
            events: Arc::from(self.events.iter().cloned().collect::<Vec<_>>()),
            dropped: self.dropped,
        }
    }

    /// Returns events whose identifiers are strictly greater than `id`.
    pub fn events_since(&self, id: u64) -> EventLogSnapshot {
        EventLogSnapshot {
            events: Arc::from(
                self.events
                    .iter()
                    .filter(|event| event.id > id)
                    .cloned()
                    .collect::<Vec<_>>(),
            ),
            dropped: self.dropped,
        }
    }

    /// Returns the number of events currently retained.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Returns whether the ring is empty.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Returns how many events have been dropped because the ring was full.
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }
}

fn next_event_id() -> Result<u64, EventLogError> {
    NEXT_EVENT_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| EventLogError::IdExhausted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_is_bounded_drops_oldest_and_events_since_filters() {
        let mut log = EventLog::new();
        let mut ids = Vec::new();
        for index in 0..(EVENT_LOG_CAPACITY + 2) {
            ids.push(
                log.push(index as u64, EventKind::PacketLossEvent, "loss")
                    .expect("id available")
                    .id,
            );
        }
        assert_eq!(log.len(), EVENT_LOG_CAPACITY);
        assert_eq!(log.dropped(), 2);
        let snapshot = log.snapshot();
        assert_eq!(
            snapshot.events().first().map(|event| event.id),
            Some(ids[2])
        );
        assert_eq!(
            snapshot.events().last().map(|event| event.id),
            ids.last().copied()
        );
        assert!(log
            .events_since(ids[EVENT_LOG_CAPACITY])
            .events()
            .iter()
            .all(|event| { event.id > ids[EVENT_LOG_CAPACITY] }));
    }

    #[test]
    fn detail_is_utf8_bounded_and_snapshots_are_independent() {
        let mut log = EventLog::new();
        let long = format!("{}🙂tail", "a".repeat(EVENT_DETAIL_MAX_BYTES - 2));
        let event = log
            .push(10, EventKind::DisplaySwitch, long)
            .expect("id available");
        assert!(event.detail.len() <= EVENT_DETAIL_MAX_BYTES);
        assert!(event.detail.is_char_boundary(event.detail.len()));
        let snapshot = log.snapshot();
        log.push(11, EventKind::StreamReset, "next")
            .expect("id available");
        assert_eq!(snapshot.events().len(), 1);
        assert_eq!(log.snapshot().events().len(), 2);
    }
}
