use core::fmt;

/// Default number of 100 ms buckets in the one-second rate window.
pub const DEFAULT_RATE_BUCKETS: u8 = 10;
/// Default bucket duration in microseconds.
pub const DEFAULT_RATE_BUCKET_WIDTH_US: u64 = 100_000;
/// Maximum accepted bucket count, bounding all rate-window storage.
pub const MAX_RATE_BUCKETS: u8 = 60;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Bucket {
    index: Option<u64>,
    events: u64,
    bytes: u64,
}

/// Typed construction errors for a rate window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RateWindowError {
    /// Bucket count must be within the fixed supported limit.
    InvalidBucketCount,
    /// Bucket duration must be positive.
    ZeroBucketWidth,
    /// Total window duration cannot fit in u64 microseconds.
    WindowDurationOverflow,
}

impl fmt::Display for RateWindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidBucketCount => "rate bucket count is outside the supported bound",
            Self::ZeroBucketWidth => "rate bucket width must be nonzero",
            Self::WindowDurationOverflow => "rate window duration overflows microseconds",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for RateWindowError {}

/// A bounded rolling count of events and bytes per second.
#[derive(Clone, Debug, PartialEq)]
pub struct RateWindow {
    buckets: Box<[Bucket]>,
    bucket_width_us: u64,
    window_duration_us: u64,
    current_index: Option<u64>,
    last_now_us: Option<u64>,
    backward_timestamps: u64,
}

impl Default for RateWindow {
    fn default() -> Self {
        Self::new(DEFAULT_RATE_BUCKETS, DEFAULT_RATE_BUCKET_WIDTH_US).unwrap_or_else(|_| Self {
            buckets: Box::new([Bucket::default(); DEFAULT_RATE_BUCKETS as usize]),
            bucket_width_us: DEFAULT_RATE_BUCKET_WIDTH_US,
            window_duration_us: 1_000_000,
            current_index: None,
            last_now_us: None,
            backward_timestamps: 0,
        })
    }
}

impl RateWindow {
    /// Creates a fixed-size ring of time buckets.
    pub fn new(bucket_count: u8, bucket_width_us: u64) -> Result<Self, RateWindowError> {
        if bucket_count == 0 || bucket_count > MAX_RATE_BUCKETS {
            return Err(RateWindowError::InvalidBucketCount);
        }
        if bucket_width_us == 0 {
            return Err(RateWindowError::ZeroBucketWidth);
        }
        let window_duration_us = u64::from(bucket_count)
            .checked_mul(bucket_width_us)
            .ok_or(RateWindowError::WindowDurationOverflow)?;
        let buckets = vec![Bucket::default(); usize::from(bucket_count)].into_boxed_slice();
        Ok(Self {
            buckets,
            bucket_width_us,
            window_duration_us,
            current_index: None,
            last_now_us: None,
            backward_timestamps: 0,
        })
    }

    /// Adds event and byte counts at an injected monotonic timestamp.
    ///
    /// A timestamp older than the last accepted timestamp is ignored and counted.
    pub fn record(&mut self, now_us: u64, events: u64, bytes: u64) -> bool {
        if !self.advance(now_us) {
            return false;
        }
        let index = now_us / self.bucket_width_us;
        let slot_index = self.slot_index(index);
        if let Some(bucket) = self.buckets.get_mut(slot_index) {
            if bucket.index != Some(index) {
                *bucket = Bucket {
                    index: Some(index),
                    events: 0,
                    bytes: 0,
                };
            }
            bucket.events = bucket.events.saturating_add(events);
            bucket.bytes = bucket.bytes.saturating_add(bytes);
        }
        true
    }

    /// Returns current totals and rates while advancing expiration to now.
    pub fn snapshot(&mut self, now_us: u64) -> RateSnapshot {
        let _ = self.advance(now_us);
        let mut events = 0u64;
        let mut bytes = 0u64;
        if let Some(current) = self.current_index {
            let bucket_count = self.buckets.len() as u64;
            for bucket in &self.buckets {
                if let Some(index) = bucket.index {
                    if current >= index && current - index < bucket_count {
                        events = events.saturating_add(bucket.events);
                        bytes = bytes.saturating_add(bucket.bytes);
                    }
                }
            }
        }
        let seconds = self.window_duration_us as f64 / 1_000_000.0;
        RateSnapshot {
            events,
            bytes,
            events_per_second: events as f64 / seconds,
            bytes_per_second: bytes as f64 / seconds,
            backward_timestamps: self.backward_timestamps,
        }
    }

    /// Returns the configured bucket count.
    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }

    /// Returns the configured bucket duration in microseconds.
    pub const fn bucket_width_us(&self) -> u64 {
        self.bucket_width_us
    }

    fn advance(&mut self, now_us: u64) -> bool {
        if self.last_now_us.is_some_and(|last| now_us < last) {
            self.backward_timestamps = self.backward_timestamps.saturating_add(1);
            return false;
        }
        self.last_now_us = Some(now_us);
        let next_index = now_us / self.bucket_width_us;
        match self.current_index {
            None => {
                self.current_index = Some(next_index);
            }
            Some(current) if next_index > current => {
                let distance = next_index - current;
                if distance >= self.buckets.len() as u64 {
                    self.clear_all();
                } else {
                    for step in 1..=distance {
                        let index = current + step;
                        let slot = self.slot_index(index);
                        if let Some(bucket) = self.buckets.get_mut(slot) {
                            *bucket = Bucket::default();
                        }
                    }
                }
                self.current_index = Some(next_index);
            }
            Some(_) => {}
        }
        let slot = self.slot_index(next_index);
        if let Some(bucket) = self.buckets.get_mut(slot) {
            if bucket.index != Some(next_index) {
                *bucket = Bucket {
                    index: Some(next_index),
                    events: 0,
                    bytes: 0,
                };
            }
        }
        true
    }

    fn clear_all(&mut self) {
        for bucket in &mut self.buckets {
            *bucket = Bucket::default();
        }
    }

    fn slot_index(&self, index: u64) -> usize {
        (index % self.buckets.len() as u64) as usize
    }
}

/// Rolling event and byte totals with rates over the configured window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RateSnapshot {
    /// Event count in the active window.
    pub events: u64,
    /// Byte count in the active window.
    pub bytes: u64,
    /// Events per second in the active window.
    pub events_per_second: f64,
    /// Bytes per second in the active window.
    pub bytes_per_second: f64,
    /// Cumulative count of ignored backward timestamps.
    pub backward_timestamps: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_window_tracks_one_second_of_events_and_bytes() {
        let mut rates = RateWindow::default();
        assert!(rates.record(0, 1, 100));
        assert!(rates.record(100_000, 1, 200));
        assert!(rates.record(900_000, 1, 300));
        let snapshot = rates.snapshot(999_999);
        assert_eq!(snapshot.events, 3);
        assert_eq!(snapshot.bytes, 600);
        assert_eq!(snapshot.events_per_second, 3.0);
        assert_eq!(snapshot.bytes_per_second, 600.0);
    }

    #[test]
    fn large_time_jump_clears_buckets_and_backward_time_is_ignored() {
        let mut rates = RateWindow::default();
        assert!(rates.record(100, 4, 500));
        assert!(!rates.record(99, 100, 100));
        assert_eq!(rates.snapshot(100).backward_timestamps, 1);
        assert!(rates.record(9_000_000_000, 2, 80));
        let snapshot = rates.snapshot(9_000_000_000);
        assert_eq!((snapshot.events, snapshot.bytes), (2, 80));
        assert!(snapshot.events_per_second.is_finite());
    }

    #[test]
    fn constructor_enforces_fixed_memory_bounds() {
        assert_eq!(
            RateWindow::new(0, 1),
            Err(RateWindowError::InvalidBucketCount)
        );
        assert_eq!(
            RateWindow::new(MAX_RATE_BUCKETS + 1, 1),
            Err(RateWindowError::InvalidBucketCount)
        );
        assert_eq!(RateWindow::new(1, 0), Err(RateWindowError::ZeroBucketWidth));
        assert_eq!(
            RateWindow::new(2, u64::MAX),
            Err(RateWindowError::WindowDurationOverflow)
        );
        assert_eq!(
            RateWindow::new(60, 1).map(|window| window.bucket_count()),
            Ok(60)
        );
    }
}
