use std::collections::VecDeque;

/// Default rolling loss-estimation interval in microseconds.
pub const LOSS_WINDOW_US: u64 = 2_000_000;
/// Maximum number of rolling loss buckets retained at once.
pub const MAX_LOSS_BUCKETS: usize = 256;

/// Per-frame packet-loss observation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LossSample {
    /// Number of fragments expected for frames with a known fragment count.
    pub expected_fragments: u64,
    /// Number of those fragments which were missing.
    pub missing_fragments: u64,
    /// Whole frames lost when no fragment count was available.
    pub whole_frames_lost: u64,
    /// Whole frames observed, whether complete or incomplete.
    pub frames_observed: u64,
}

/// Windowed packet and whole-frame loss estimate.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LossEstimate {
    /// Missing fragment fraction among frames with known fragment counts.
    pub packet_loss_fraction: f64,
    /// Missing whole-frame fraction where fragment count was unknown.
    pub whole_frame_loss_fraction: f64,
    /// Expected fragments in the current window.
    pub expected_fragments: u64,
    /// Missing fragments in the current window.
    pub missing_fragments: u64,
    /// Whole frames lost in the current window.
    pub whole_frames_lost: u64,
    /// Frames observed in the current window.
    pub frames_observed: u64,
}

/// Bounded rolling loss estimator over a caller-provided monotonic clock.
#[derive(Clone, Debug)]
pub struct LossEstimator {
    window_us: u64,
    bucket_width_us: u64,
    samples: VecDeque<(u64, LossSample)>,
    total: LossSample,
}

impl Default for LossEstimator {
    fn default() -> Self {
        Self::new(LOSS_WINDOW_US)
    }
}

impl LossEstimator {
    /// Creates an estimator with a window length in microseconds.
    pub fn new(window_us: u64) -> Self {
        let window_us = window_us.max(1);
        let bucket_count = u64::try_from(MAX_LOSS_BUCKETS).unwrap_or(u64::MAX);
        Self {
            window_us,
            bucket_width_us: (window_us / bucket_count).max(1),
            samples: VecDeque::new(),
            total: LossSample::default(),
        }
    }

    /// Adds one completed, dropped, or whole-frame-loss observation.
    pub fn record(&mut self, now_us: u64, sample: LossSample) {
        self.total.expected_fragments = self
            .total
            .expected_fragments
            .saturating_add(sample.expected_fragments);
        self.total.missing_fragments = self
            .total
            .missing_fragments
            .saturating_add(sample.missing_fragments);
        self.total.whole_frames_lost = self
            .total
            .whole_frames_lost
            .saturating_add(sample.whole_frames_lost);
        self.total.frames_observed = self
            .total
            .frames_observed
            .saturating_add(sample.frames_observed);

        let bucket_at = now_us - (now_us % self.bucket_width_us);
        if let Some((last_at, last_sample)) = self.samples.back_mut() {
            if *last_at == bucket_at {
                add_sample(last_sample, sample);
                self.prune(now_us);
                return;
            }
        }
        self.samples.push_back((bucket_at, sample));
        self.prune(now_us);
    }

    /// Returns the current rolling fractions and totals.
    pub fn estimate(&mut self, now_us: u64) -> LossEstimate {
        self.prune(now_us);
        let packet_loss_fraction = if self.total.expected_fragments == 0 {
            0.0
        } else {
            self.total.missing_fragments as f64 / self.total.expected_fragments as f64
        };
        let whole_frame_loss_fraction = if self.total.frames_observed == 0 {
            0.0
        } else {
            self.total.whole_frames_lost as f64 / self.total.frames_observed as f64
        };
        LossEstimate {
            packet_loss_fraction,
            whole_frame_loss_fraction,
            expected_fragments: self.total.expected_fragments,
            missing_fragments: self.total.missing_fragments,
            whole_frames_lost: self.total.whole_frames_lost,
            frames_observed: self.total.frames_observed,
        }
    }

    fn prune(&mut self, now_us: u64) {
        let cutoff = now_us.saturating_sub(self.window_us);
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| at.saturating_add(self.bucket_width_us) <= cutoff)
        {
            if let Some((_, sample)) = self.samples.pop_front() {
                self.total.expected_fragments = self
                    .total
                    .expected_fragments
                    .saturating_sub(sample.expected_fragments);
                self.total.missing_fragments = self
                    .total
                    .missing_fragments
                    .saturating_sub(sample.missing_fragments);
                self.total.whole_frames_lost = self
                    .total
                    .whole_frames_lost
                    .saturating_sub(sample.whole_frames_lost);
                self.total.frames_observed = self
                    .total
                    .frames_observed
                    .saturating_sub(sample.frames_observed);
            }
        }
    }
}

fn add_sample(total: &mut LossSample, sample: LossSample) {
    total.expected_fragments = total
        .expected_fragments
        .saturating_add(sample.expected_fragments);
    total.missing_fragments = total
        .missing_fragments
        .saturating_add(sample.missing_fragments);
    total.whole_frames_lost = total
        .whole_frames_lost
        .saturating_add(sample.whole_frames_lost);
    total.frames_observed = total.frames_observed.saturating_add(sample.frames_observed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loss_estimator_prunes_old_samples_on_the_injected_clock() {
        let mut estimator = LossEstimator::new(100);
        estimator.record(
            0,
            LossSample {
                expected_fragments: 10,
                missing_fragments: 2,
                frames_observed: 1,
                ..LossSample::default()
            },
        );
        estimator.record(
            50,
            LossSample {
                expected_fragments: 10,
                missing_fragments: 0,
                frames_observed: 1,
                ..LossSample::default()
            },
        );
        assert_eq!(estimator.estimate(99).packet_loss_fraction, 0.1);
        assert_eq!(estimator.estimate(101).packet_loss_fraction, 0.0);
    }

    #[test]
    fn high_rate_samples_are_aggregated_into_a_bounded_window() {
        let mut estimator = LossEstimator::default();
        for now_us in 0..1_000_000u64 {
            estimator.record(
                now_us,
                LossSample {
                    expected_fragments: 1,
                    missing_fragments: u64::from(now_us % 2 == 0),
                    frames_observed: 1,
                    ..LossSample::default()
                },
            );
        }
        assert!(estimator.samples.len() <= MAX_LOSS_BUCKETS + 1);
        let estimate = estimator.estimate(999_999);
        assert!(estimate.expected_fragments <= 1_000_000);
        assert!((0.49..=0.51).contains(&estimate.packet_loss_fraction));
        assert_eq!(LOSS_WINDOW_US, 2_000_000);
    }
}
