use core::fmt;
use std::collections::VecDeque;

/// Maximum retained RTT samples and outstanding ping entries.
pub const MAX_RTT_SAMPLES: usize = 256;
/// Default rolling minimum RTT window in microseconds.
pub const DEFAULT_RTT_MIN_WINDOW_US: u64 = 10_000_000;
/// Maximum number of pings tracked at once.
pub const MAX_OUTSTANDING_PINGS: usize = 16;

/// RTT estimator configuration error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RttEstimatorError {
    /// The rolling minimum window must be positive.
    ZeroWindow,
}

impl fmt::Display for RttEstimatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RTT minimum window must be positive")
    }
}

impl std::error::Error for RttEstimatorError {}

#[derive(Clone, Copy, Debug)]
struct Sample {
    at_us: u64,
    rtt_us: u64,
}

/// RTT summary using RFC 6298-style smoothing and a bounded recent sample window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RttSnapshot {
    /// Most recently accepted RTT sample in microseconds.
    pub last_rtt_us: Option<u64>,
    /// Minimum RTT in the configured recent window in microseconds.
    pub min_rtt_us: Option<u64>,
    /// Smoothed RTT (SRTT) in microseconds.
    pub srtt_us: Option<f64>,
    /// RTT variation (RTTVAR) in microseconds.
    pub rttvar_us: Option<f64>,
    /// Mean absolute difference between adjacent recent RTT samples.
    pub jitter_us: Option<f64>,
    /// Number of accepted RTT samples retained for the minimum and jitter.
    pub sample_count: usize,
}

/// Bounded RTT estimator; callers supply monotonic timestamps.
#[derive(Clone, Debug)]
pub struct RttEstimator {
    min_window_us: u64,
    samples: VecDeque<Sample>,
    last_rtt_us: Option<u64>,
    srtt_us: Option<f64>,
    rttvar_us: Option<f64>,
}

impl Default for RttEstimator {
    fn default() -> Self {
        Self::new(DEFAULT_RTT_MIN_WINDOW_US).unwrap_or_else(|_| Self {
            min_window_us: DEFAULT_RTT_MIN_WINDOW_US,
            samples: VecDeque::new(),
            last_rtt_us: None,
            srtt_us: None,
            rttvar_us: None,
        })
    }
}

impl RttEstimator {
    /// Creates an estimator with a rolling minimum window.
    pub fn new(min_window_us: u64) -> Result<Self, RttEstimatorError> {
        if min_window_us == 0 {
            return Err(RttEstimatorError::ZeroWindow);
        }
        Ok(Self {
            min_window_us,
            samples: VecDeque::with_capacity(MAX_RTT_SAMPLES),
            last_rtt_us: None,
            srtt_us: None,
            rttvar_us: None,
        })
    }

    /// Adds a nonnegative RTT sample at the supplied monotonic timestamp.
    pub fn record(&mut self, now_us: u64, rtt_us: u64) {
        match (self.srtt_us, self.rttvar_us) {
            (None, None) => {
                self.srtt_us = Some(rtt_us as f64);
                self.rttvar_us = Some(rtt_us as f64 / 2.0);
            }
            (Some(srtt), Some(rttvar)) => {
                let deviation = (srtt - rtt_us as f64).abs();
                self.rttvar_us = Some(0.75 * rttvar + 0.25 * deviation);
                self.srtt_us = Some(0.875 * srtt + 0.125 * rtt_us as f64);
            }
            _ => {
                self.srtt_us = Some(rtt_us as f64);
                self.rttvar_us = Some(rtt_us as f64 / 2.0);
            }
        }
        self.last_rtt_us = Some(rtt_us);
        if self.samples.len() == MAX_RTT_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(Sample {
            at_us: now_us,
            rtt_us,
        });
        self.prune(now_us);
    }

    /// Returns current RTT figures, expiring samples outside the minimum window.
    pub fn snapshot(&mut self, now_us: u64) -> RttSnapshot {
        self.prune(now_us);
        let min_rtt_us = self.samples.iter().map(|sample| sample.rtt_us).min();
        let jitter_us = if self.samples.len() < 2 {
            None
        } else {
            let (sum, count) = self.samples.iter().zip(self.samples.iter().skip(1)).fold(
                (0.0, 0usize),
                |(sum, count), (left, right)| {
                    (sum + left.rtt_us.abs_diff(right.rtt_us) as f64, count + 1)
                },
            );
            Some(sum / count as f64)
        };
        RttSnapshot {
            last_rtt_us: self.last_rtt_us,
            min_rtt_us,
            srtt_us: self.srtt_us,
            rttvar_us: self.rttvar_us,
            jitter_us,
            sample_count: self.samples.len(),
        }
    }

    /// Returns the configured rolling minimum window.
    pub const fn min_window_us(&self) -> u64 {
        self.min_window_us
    }

    fn prune(&mut self, now_us: u64) {
        while self.samples.front().is_some_and(|sample| {
            now_us >= sample.at_us && now_us - sample.at_us >= self.min_window_us
        }) {
            self.samples.pop_front();
        }
    }
}

/// Result of adding a caller-supplied ping nonce to the bounded tracker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PingRecordOutcome {
    /// The ping was recorded without evicting another entry.
    Recorded,
    /// The oldest outstanding ping was dropped to make room.
    DroppedOldest,
    /// A nonce already outstanding was supplied and the new ping was ignored.
    DuplicateNonce,
}

#[derive(Clone, Copy, Debug)]
struct OutstandingPing {
    nonce: u64,
    echo_ts_us: u64,
    sent_at_us: u64,
}

/// Bounded ping tracker; nonce generation is left to the caller.
#[derive(Clone, Debug, Default)]
pub struct PingTracker {
    outstanding: VecDeque<OutstandingPing>,
    dropped_oldest: u64,
    timed_out: u64,
    unknown_or_duplicate_pongs: u64,
}

/// Ping-tracker counters and current occupancy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PingTrackerSnapshot {
    /// Number of outstanding pings.
    pub outstanding: usize,
    /// Number of oldest outstanding pings evicted at capacity.
    pub dropped_oldest: u64,
    /// Number of pings removed due to timeout.
    pub timed_out: u64,
    /// Unknown, duplicate, or malformed pong count.
    pub unknown_or_duplicate_pongs: u64,
}

impl PingTracker {
    /// Creates an empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a ping with caller nonce, echoed timestamp, and send time.
    pub fn record_ping(&mut self, nonce: u64, echo_ts_us: u64, now_us: u64) -> PingRecordOutcome {
        if self.outstanding.iter().any(|ping| ping.nonce == nonce) {
            return PingRecordOutcome::DuplicateNonce;
        }
        let outcome = if self.outstanding.len() == MAX_OUTSTANDING_PINGS {
            self.outstanding.pop_front();
            self.dropped_oldest = self.dropped_oldest.saturating_add(1);
            PingRecordOutcome::DroppedOldest
        } else {
            PingRecordOutcome::Recorded
        };
        self.outstanding.push_back(OutstandingPing {
            nonce,
            echo_ts_us,
            sent_at_us: now_us,
        });
        outcome
    }

    /// Matches a pong and returns elapsed RTT; unknown or duplicate pongs count and are ignored.
    pub fn match_pong(&mut self, nonce: u64, echo_ts_us: u64, now_us: u64) -> Option<u64> {
        let Some(index) = self.outstanding.iter().position(|ping| ping.nonce == nonce) else {
            self.unknown_or_duplicate_pongs = self.unknown_or_duplicate_pongs.saturating_add(1);
            return None;
        };
        let Some(ping) = self.outstanding.remove(index) else {
            self.unknown_or_duplicate_pongs = self.unknown_or_duplicate_pongs.saturating_add(1);
            return None;
        };
        if ping.echo_ts_us != echo_ts_us || now_us < ping.sent_at_us {
            self.unknown_or_duplicate_pongs = self.unknown_or_duplicate_pongs.saturating_add(1);
            return None;
        }
        Some(now_us - ping.sent_at_us)
    }

    /// Removes pings whose age is at least `timeout_us` and returns the number expired.
    pub fn expire(&mut self, now_us: u64, timeout_us: u64) -> usize {
        let before = self.outstanding.len();
        self.outstanding
            .retain(|ping| now_us < ping.sent_at_us || now_us - ping.sent_at_us < timeout_us);
        let expired = before - self.outstanding.len();
        self.timed_out = self.timed_out.saturating_add(expired as u64);
        expired
    }

    /// Returns counters and current occupancy.
    pub fn snapshot(&self) -> PingTrackerSnapshot {
        PingTrackerSnapshot {
            outstanding: self.outstanding.len(),
            dropped_oldest: self.dropped_oldest,
            timed_out: self.timed_out,
            unknown_or_duplicate_pongs: self.unknown_or_duplicate_pongs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6298_style_updates_and_recent_minimum_and_jitter() {
        let mut estimator = RttEstimator::new(1_000).expect("valid window");
        estimator.record(10, 100);
        let first = estimator.snapshot(10);
        assert_eq!(first.srtt_us, Some(100.0));
        assert_eq!(first.rttvar_us, Some(50.0));
        estimator.record(20, 200);
        let second = estimator.snapshot(20);
        assert_eq!(second.srtt_us, Some(112.5));
        assert_eq!(second.rttvar_us, Some(62.5));
        assert_eq!(second.min_rtt_us, Some(100));
        assert_eq!(second.jitter_us, Some(100.0));
        assert_eq!(estimator.snapshot(1_020).min_rtt_us, None);
    }

    #[test]
    fn tracker_is_bounded_matches_expires_and_counts_unknown_pongs() {
        let mut tracker = PingTracker::new();
        for nonce in 0..MAX_OUTSTANDING_PINGS as u64 {
            assert_eq!(
                tracker.record_ping(nonce, nonce + 1, nonce),
                PingRecordOutcome::Recorded
            );
        }
        assert_eq!(
            tracker.record_ping(16, 17, 16),
            PingRecordOutcome::DroppedOldest
        );
        assert_eq!(tracker.snapshot().outstanding, MAX_OUTSTANDING_PINGS);
        assert_eq!(tracker.snapshot().dropped_oldest, 1);
        assert_eq!(tracker.match_pong(1, 2, 30), Some(29));
        assert_eq!(tracker.match_pong(1, 2, 31), None);
        assert_eq!(tracker.match_pong(2, 999, 32), None);
        assert_eq!(tracker.expire(100, 50), MAX_OUTSTANDING_PINGS - 2);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.outstanding, 0);
        assert_eq!(snapshot.timed_out, (MAX_OUTSTANDING_PINGS - 2) as u64);
        assert_eq!(snapshot.unknown_or_duplicate_pongs, 2);
    }
}
