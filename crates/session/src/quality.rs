//! Deterministic host-side quality adaptation with virtual time.
use std::collections::VecDeque;

/// Minimum supported stream height in pixels.
pub const QUALITY_MIN_HEIGHT: u16 = 480;
/// Maximum supported stream height in pixels.
pub const QUALITY_MAX_HEIGHT: u16 = 1080;
/// Sustained loss thresholds are measured in basis points (200 = 2.00%).
pub const QUALITY_LOSS_THRESHOLD_BPS: u32 = 200;
/// Frame loss threshold in basis points (500 = 5.00%).
pub const QUALITY_FRAME_LOSS_THRESHOLD_BPS: u32 = 500;
/// Packet and frame loss must both remain below 0.5% for upward recovery.
pub const QUALITY_STABLE_LOSS_LIMIT_BPS: u32 = 50;
/// Loss must remain high for this long before a tier step-down.
pub const QUALITY_LOSS_DWELL_US: u64 = 3_000_000;
/// RTT inflation must remain high for this long before a tier step-down.
pub const QUALITY_RTT_DWELL_US: u64 = 5_000_000;
/// Queue-overflow events are considered within this rolling window.
pub const QUALITY_OVERFLOW_WINDOW_US: u64 = 2_000_000;
/// Stable operation required before a tier step-up.
pub const QUALITY_STABLE_DWELL_US: u64 = 20_000_000;
/// Minimum time between automatic tier step-ups.
pub const QUALITY_STEP_UP_INTERVAL_US: u64 = 30_000_000;
/// Interval between bitrate reductions while a stream remains degraded.
pub const QUALITY_BITRATE_TRIM_INTERVAL_US: u64 = 1_000_000;
/// Each staged bitrate adjustment changes the active tier target by 10%.
pub const QUALITY_BITRATE_TRIM_STEP_PERCENT: u8 = 10;
/// Lowest bitrate target before automatic tier reduction.
pub const QUALITY_MIN_BITRATE_PERCENT: u8 = 70;

/// Default bitrate for the 480p30 tier in bits per second.
pub const BITRATE_480_BPS: u32 = 1_500_000;
/// Default bitrate for the 720p30 tier in bits per second.
pub const BITRATE_720_BPS: u32 = 3_500_000;
/// Default bitrate for the 1080p30 tier in bits per second.
pub const BITRATE_1080_BPS: u32 = 7_000_000;

/// Supported fixed stream quality tier.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum QualityTier {
    /// 480p30 stream.
    P480,
    /// 720p30 stream.
    P720,
    /// 1080p30 stream.
    P1080,
}

impl QualityTier {
    /// Returns this tier's height in pixels.
    pub const fn height(self) -> u16 {
        match self {
            Self::P480 => 480,
            Self::P720 => 720,
            Self::P1080 => 1080,
        }
    }

    /// Returns the default bitrate for this tier.
    pub const fn default_bitrate_bps(self) -> u32 {
        match self {
            Self::P480 => BITRATE_480_BPS,
            Self::P720 => BITRATE_720_BPS,
            Self::P1080 => BITRATE_1080_BPS,
        }
    }

    fn previous(self) -> Option<Self> {
        match self {
            Self::P480 => None,
            Self::P720 => Some(Self::P480),
            Self::P1080 => Some(Self::P720),
        }
    }

    fn next(self) -> Option<Self> {
        match self {
            Self::P480 => Some(Self::P720),
            Self::P720 => Some(Self::P1080),
            Self::P1080 => None,
        }
    }

    fn at_or_below(height: u16) -> Self {
        if height >= Self::P1080.height() {
            Self::P1080
        } else if height >= Self::P720.height() {
            Self::P720
        } else {
            Self::P480
        }
    }
}

/// Automatic adaptation or an owner-selected fixed tier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QualityPreference {
    /// Allow loss and stability feedback to change quality.
    Auto,
    /// Keep this tier fixed, clamped by host and display limits.
    Fixed(QualityTier),
}

/// Quality feedback sampled at a caller-supplied virtual timestamp.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QualityFeedback {
    /// Packet loss in basis points (200 = 2.00%).
    pub loss_bps: u32,
    /// Dropped-frame loss in basis points (500 = 5.00%).
    pub frame_loss_bps: u32,
    /// Current round-trip time in microseconds.
    pub rtt_us: u64,
    /// Stable RTT baseline in microseconds; zero means not yet established.
    pub rtt_baseline_us: u64,
    /// 95th percentile decode time in milliseconds, when available.
    ///
    /// This is carried for telemetry; the host tier controller does not infer
    /// a threshold from it.
    pub decode_ms_p95: Option<u32>,
    /// Number of dropped frames since the previous feedback sample.
    /// Report normalized frame_loss_bps as well; this count is not a rate.
    pub dropped_frames: u32,
    /// Encoder lag in milliseconds, when available; no threshold is inferred.
    pub encoder_lag_ms: Option<u32>,
    /// True when this sample reports one sender-queue overflow.
    pub queue_overflow: bool,
}

/// Why the controller selected a new tier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QualityChangeReason {
    /// Sustained packet or frame loss.
    Loss,
    /// Sustained RTT inflation over the supplied baseline.
    RttInflation,
    /// At least two sender queue overflows in the rolling window.
    QueueOverflow,
    /// A stable period allowed a gradual step-up.
    Stable,
    /// A fixed owner preference selected another tier.
    Preference,
}

/// Ordered operations for a host adapter to apply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QualityEvent {
    /// Trim the active bitrate before applying a tier change.
    BitrateTrim {
        /// Previous bitrate in bits per second.
        from_bps: u32,
        /// Temporary bitrate in bits per second.
        to_bps: u32,
        /// Percentage of the previous bitrate, always from 70 through 100.
        percent: u8,
    },
    /// Apply a new stream tier and its target bitrate.
    TierChanged {
        /// Previous tier.
        from: QualityTier,
        /// New tier, already clamped to host and display limits.
        to: QualityTier,
        /// Controller reason.
        reason: QualityChangeReason,
        /// Default target bitrate for the new tier.
        target_bitrate_bps: u32,
    },
}

/// Invalid controller bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QualityConfigError {
    /// Host or display limit is below the supported 480p minimum.
    BelowMinimumHeight,
}

impl std::fmt::Display for QualityConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("host and display height must permit at least 480p")
    }
}

impl std::error::Error for QualityConfigError {}

/// Pure host quality policy; all time is supplied by the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QualityController {
    preference: QualityPreference,
    current_tier: QualityTier,
    maximum_tier: QualityTier,
    current_bitrate_bps: u32,
    loss_bad_since_us: Option<u64>,
    rtt_bad_since_us: Option<u64>,
    stable_since_us: Option<u64>,
    overflow_times_us: VecDeque<u64>,
    last_auto_tier_change_us: Option<u64>,
    last_update_us: Option<u64>,
    bitrate_trim_started_us: Option<u64>,
    bitrate_trim_stage: u8,
    pending_downshift: Option<QualityChangeReason>,
}

impl QualityController {
    /// Creates a controller clamped by both host maximum and display height.
    ///
    /// A source or host that cannot produce at least 480p is rejected rather
    /// than silently violating the product's minimum-height requirement.
    pub fn new(
        host_max_height: u16,
        display_height: u16,
        initial_tier: QualityTier,
        preference: QualityPreference,
    ) -> Result<Self, QualityConfigError> {
        let max_height = host_max_height.min(display_height).min(QUALITY_MAX_HEIGHT);
        if max_height < QUALITY_MIN_HEIGHT {
            return Err(QualityConfigError::BelowMinimumHeight);
        }
        let maximum_tier = QualityTier::at_or_below(max_height);
        let requested = match preference {
            QualityPreference::Auto => initial_tier,
            QualityPreference::Fixed(tier) => tier,
        };
        let current_tier = requested.min(maximum_tier);
        Ok(Self {
            preference,
            current_tier,
            maximum_tier,
            current_bitrate_bps: current_tier.default_bitrate_bps(),
            loss_bad_since_us: None,
            rtt_bad_since_us: None,
            stable_since_us: None,
            overflow_times_us: VecDeque::new(),
            last_auto_tier_change_us: None,
            last_update_us: None,
            bitrate_trim_started_us: None,
            bitrate_trim_stage: 0,
            pending_downshift: None,
        })
    }

    /// Current adaptation preference.
    pub const fn preference(&self) -> QualityPreference {
        self.preference
    }

    /// Current tier after host/display clamping.
    pub const fn current_tier(&self) -> QualityTier {
        self.current_tier
    }

    /// Highest tier allowed by both host and selected display.
    pub const fn maximum_tier(&self) -> QualityTier {
        self.maximum_tier
    }

    /// Current controller bitrate target in bits per second.
    pub const fn current_bitrate_bps(&self) -> u32 {
        self.current_bitrate_bps
    }

    /// Changes the preference. Fixed preferences disable automatic tier steps.
    pub fn set_preference(
        &mut self,
        preference: QualityPreference,
        now_us: u64,
    ) -> Vec<QualityEvent> {
        self.monotonic_time(now_us);
        self.preference = preference;
        self.loss_bad_since_us = None;
        self.rtt_bad_since_us = None;
        self.stable_since_us = None;
        self.overflow_times_us.clear();
        self.bitrate_trim_started_us = None;
        self.pending_downshift = None;
        if let QualityPreference::Fixed(requested) = preference {
            let target = requested.min(self.maximum_tier);
            if target == self.current_tier {
                self.adjust_bitrate_stage(0)
            } else {
                self.set_tier(target, QualityChangeReason::Preference)
            }
        } else {
            Vec::new()
        }
    }

    /// Applies one feedback sample using a deterministic virtual clock.
    ///
    /// Loss is sustained above 2% packet loss or 5% frame loss for 3 seconds.
    /// RTT inflation is sustained above three times baseline for 5 seconds.
    /// Two queue overflows in a 2-second rolling window step down immediately.
    /// Queue overflow applies the 70% bitrate trim before the tier step in the
    /// same returned action set. Sustained loss/RTT exhausts the in-tier trim
    /// before stepping down on the next sample. Recovery requires packet and frame loss below
    /// 0.5%, RTT at most 1.5 times baseline, and 20 stable seconds; automatic
    /// tier step-ups are separated by at least 30 seconds.
    /// Fixed preferences never tier-step.
    pub fn update(&mut self, now_us: u64, feedback: QualityFeedback) -> Vec<QualityEvent> {
        let now_us = self.monotonic_time(now_us);
        self.prune_overflows(now_us);
        if feedback.queue_overflow {
            self.overflow_times_us.push_back(now_us);
            // Only the two newest timestamps can affect the two-overflow
            // threshold. Keep this bounded even if an adapter reports an
            // overflow for every queued packet.
            while self.overflow_times_us.len() > 2 {
                self.overflow_times_us.pop_front();
            }
        }
        let loss_bad = feedback.loss_bps > QUALITY_LOSS_THRESHOLD_BPS
            || feedback.frame_loss_bps > QUALITY_FRAME_LOSS_THRESHOLD_BPS;
        update_dwell(&mut self.loss_bad_since_us, loss_bad, now_us);
        let rtt_bad = feedback.rtt_baseline_us > 0
            && u128::from(feedback.rtt_us) > u128::from(feedback.rtt_baseline_us).saturating_mul(3);
        update_dwell(&mut self.rtt_bad_since_us, rtt_bad, now_us);
        let stable = feedback.loss_bps < QUALITY_STABLE_LOSS_LIMIT_BPS
            && feedback.frame_loss_bps < QUALITY_STABLE_LOSS_LIMIT_BPS
            && feedback.rtt_baseline_us > 0
            && u128::from(feedback.rtt_us).saturating_mul(2)
                <= u128::from(feedback.rtt_baseline_us).saturating_mul(3)
            && self.overflow_times_us.is_empty();
        update_dwell(&mut self.stable_since_us, stable, now_us);

        if let Some(reason) = self.pending_downshift.take() {
            let pressure_still_active = match reason {
                QualityChangeReason::Loss => loss_bad,
                QualityChangeReason::RttInflation => rtt_bad,
                QualityChangeReason::QueueOverflow
                | QualityChangeReason::Stable
                | QualityChangeReason::Preference => false,
            };
            if pressure_still_active {
                if let Some(target) = self.current_tier.previous() {
                    self.last_auto_tier_change_us = Some(now_us);
                    let events = self.set_tier(target, reason);
                    self.clear_feedback_windows();
                    return events;
                }
            }
        }

        let reason = if self.overflow_times_us.len() >= 2 {
            Some(QualityChangeReason::QueueOverflow)
        } else if self
            .loss_bad_since_us
            .is_some_and(|start| now_us.saturating_sub(start) >= QUALITY_LOSS_DWELL_US)
        {
            Some(QualityChangeReason::Loss)
        } else if self
            .rtt_bad_since_us
            .is_some_and(|start| now_us.saturating_sub(start) >= QUALITY_RTT_DWELL_US)
        {
            Some(QualityChangeReason::RttInflation)
        } else {
            None
        };

        let degradation_active = loss_bad || rtt_bad || !self.overflow_times_us.is_empty();
        let mut events = Vec::new();
        if degradation_active {
            let started = *self.bitrate_trim_started_us.get_or_insert(now_us);
            let elapsed = now_us.saturating_sub(started);
            let mut stage = ((elapsed / QUALITY_BITRATE_TRIM_INTERVAL_US).min(3) as u8)
                .max(self.bitrate_trim_stage);
            if reason.is_some() {
                if self.preference == QualityPreference::Auto {
                    // Queue pressure uses the immediate path below. Sustained
                    // loss/RTT reaches the full 70% floor before its pending
                    // tier step is considered on the next feedback sample.
                    stage = 3;
                } else {
                    // A fixed tier has no resolution step, so it can use the
                    // full trim range down to 70%.
                    stage = 3;
                }
            }
            events.extend(self.adjust_bitrate_stage(stage));
        } else {
            self.bitrate_trim_started_us = None;
        }

        if let Some(reason) = reason.filter(|_| self.preference == QualityPreference::Auto) {
            if let Some(target) = self.current_tier.previous() {
                if reason != QualityChangeReason::QueueOverflow {
                    self.pending_downshift = Some(reason);
                    self.loss_bad_since_us = loss_bad.then_some(now_us);
                    self.rtt_bad_since_us = rtt_bad.then_some(now_us);
                    self.stable_since_us = None;
                    self.overflow_times_us.clear();
                    return events;
                }
                self.last_auto_tier_change_us = Some(now_us);
                events.extend(self.set_tier(target, reason));
                self.clear_feedback_windows();
                return events;
            }
            self.loss_bad_since_us = loss_bad.then_some(now_us);
            self.rtt_bad_since_us = rtt_bad.then_some(now_us);
            self.overflow_times_us.clear();
            self.stable_since_us = None;
            return events;
        }

        if stable {
            events.extend(self.restore_bitrate_gradually(now_us));
        }

        // A fixed tier prevents resolution changes, not bitrate adaptation.
        // Keep the same staged trim and recovery behavior while honoring the
        // user's selected resolution.
        if matches!(self.preference, QualityPreference::Fixed(_)) {
            return events;
        }
        let stable_long_enough = self
            .stable_since_us
            .is_some_and(|start| now_us.saturating_sub(start) >= QUALITY_STABLE_DWELL_US);
        let step_up_ready = self
            .last_auto_tier_change_us
            .is_none_or(|last| now_us.saturating_sub(last) >= QUALITY_STEP_UP_INTERVAL_US);
        if stable && stable_long_enough && step_up_ready {
            if let Some(target) = self
                .current_tier
                .next()
                .filter(|tier| *tier <= self.maximum_tier)
            {
                self.last_auto_tier_change_us = Some(now_us);
                events.extend(self.set_tier(target, QualityChangeReason::Stable));
                self.stable_since_us = Some(now_us);
            }
        }
        events
    }

    /// Clears stale feedback windows after a stream reset or display switch.
    ///
    /// The current tier and bitrate remain in place; fresh feedback must earn
    /// any later downshift or recovery.
    pub fn reset_feedback_window(&mut self, now_us: u64) {
        self.monotonic_time(now_us);
        self.clear_feedback_windows();
        self.bitrate_trim_started_us = None;
        self.pending_downshift = None;
    }

    fn clear_feedback_windows(&mut self) {
        self.loss_bad_since_us = None;
        self.rtt_bad_since_us = None;
        self.stable_since_us = None;
        self.overflow_times_us.clear();
    }

    fn set_tier(&mut self, target: QualityTier, reason: QualityChangeReason) -> Vec<QualityEvent> {
        let target = target.min(self.maximum_tier);
        if target == self.current_tier {
            return Vec::new();
        }
        let from = self.current_tier;
        let target_bitrate_bps = target.default_bitrate_bps();
        self.current_tier = target;
        self.current_bitrate_bps = target_bitrate_bps;
        self.bitrate_trim_stage = 0;
        self.bitrate_trim_started_us = None;
        vec![QualityEvent::TierChanged {
            from,
            to: target,
            reason,
            target_bitrate_bps,
        }]
    }

    fn adjust_bitrate_stage(&mut self, stage: u8) -> Vec<QualityEvent> {
        let stage = stage.min(3);
        if stage == self.bitrate_trim_stage {
            return Vec::new();
        }
        let percent = 100 - stage * QUALITY_BITRATE_TRIM_STEP_PERCENT;
        let target =
            (u64::from(self.current_tier.default_bitrate_bps()) * u64::from(percent) / 100) as u32;
        let from = self.current_bitrate_bps;
        self.current_bitrate_bps = target;
        self.bitrate_trim_stage = stage;
        if target == from {
            return Vec::new();
        }
        vec![QualityEvent::BitrateTrim {
            from_bps: from,
            to_bps: target,
            percent,
        }]
    }

    fn restore_bitrate_gradually(&mut self, now_us: u64) -> Vec<QualityEvent> {
        let Some(started) = self.stable_since_us else {
            return Vec::new();
        };
        let restored_stages =
            (now_us.saturating_sub(started) / QUALITY_BITRATE_TRIM_INTERVAL_US).min(3) as u8;
        let target_stage = self.bitrate_trim_stage.saturating_sub(restored_stages);
        self.adjust_bitrate_stage(target_stage)
    }

    fn prune_overflows(&mut self, now_us: u64) {
        while self
            .overflow_times_us
            .front()
            .is_some_and(|event_us| now_us.saturating_sub(*event_us) > QUALITY_OVERFLOW_WINDOW_US)
        {
            self.overflow_times_us.pop_front();
        }
    }

    fn monotonic_time(&mut self, now_us: u64) -> u64 {
        let now_us = self.last_update_us.map_or(now_us, |last| now_us.max(last));
        self.last_update_us = Some(now_us);
        now_us
    }
}

fn update_dwell(start: &mut Option<u64>, condition: bool, now_us: u64) {
    if condition {
        start.get_or_insert(now_us);
    } else {
        *start = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn controller(initial: QualityTier, preference: QualityPreference) -> QualityController {
        QualityController::new(1080, 1080, initial, preference)
            .unwrap_or_else(|error| panic!("controller: {error}"))
    }

    fn feedback(
        loss_bps: u32,
        frame_loss_bps: u32,
        rtt_us: u64,
        baseline_us: u64,
    ) -> QualityFeedback {
        QualityFeedback {
            loss_bps,
            frame_loss_bps,
            rtt_us,
            rtt_baseline_us: baseline_us,
            decode_ms_p95: None,
            dropped_frames: 0,
            encoder_lag_ms: None,
            queue_overflow: false,
        }
    }

    proptest! {
        #[test]
        fn arbitrary_feedback_keeps_tier_and_bitrate_within_product_bounds(
            host_height in 480u16..=2160,
            display_height in 480u16..=2160,
            fixed_preference in any::<bool>(),
            requested_tier in 0u8..=2,
            samples in prop::collection::vec(
                (0u32..=4_000, 0u32..=4_000, 0u8..=10, any::<bool>()),
                1..=96,
            ),
        ) {
            let tier = match requested_tier {
                0 => QualityTier::P480,
                1 => QualityTier::P720,
                _ => QualityTier::P1080,
            };
            let preference = if fixed_preference {
                QualityPreference::Fixed(tier)
            } else {
                QualityPreference::Auto
            };
            let Ok(mut policy) = QualityController::new(
                host_height,
                display_height,
                QualityTier::P1080,
                preference,
            ) else {
                prop_assert!(host_height.min(display_height) < QUALITY_MIN_HEIGHT);
                return Ok(());
            };

            for (index, (loss_bps, frame_loss_bps, rtt_multiplier, queue_overflow))
                in samples.into_iter().enumerate()
            {
                let now_us = index as u64 * 500_000;
                let events = policy.update(now_us, QualityFeedback {
                    loss_bps,
                    frame_loss_bps,
                    rtt_us: u64::from(rtt_multiplier) * 10_000,
                    rtt_baseline_us: 10_000,
                    decode_ms_p95: None,
                    dropped_frames: 0,
                    encoder_lag_ms: None,
                    queue_overflow,
                });
                prop_assert!(policy.current_tier() >= QualityTier::P480);
                prop_assert!(policy.current_tier() <= policy.maximum_tier());
                let tier_bitrate = policy.current_tier().default_bitrate_bps();
                let minimum_bitrate = tier_bitrate * u32::from(QUALITY_MIN_BITRATE_PERCENT) / 100;
                prop_assert!(policy.current_bitrate_bps() >= minimum_bitrate);
                prop_assert!(policy.current_bitrate_bps() <= tier_bitrate);
                for event in events {
                    if let QualityEvent::TierChanged { to, .. } = event {
                        prop_assert!(to >= QualityTier::P480);
                        prop_assert!(to <= policy.maximum_tier());
                    }
                }
            }
        }

        #[test]
        fn stable_auto_stepups_obey_the_minimum_interval(
            deltas in prop::collection::vec(1u64..=2_000_000, 64..=96),
        ) {
            let mut policy = controller(QualityTier::P480, QualityPreference::Auto);
            let mut now_us = 0u64;
            let mut last_step_up = None;
            let mut step_ups = 0usize;
            for delta in deltas {
                now_us = now_us.saturating_add(delta);
                let events = policy.update(now_us, feedback(0, 0, 10_000, 10_000));
                for event in events {
                    if let QualityEvent::TierChanged {
                        from,
                        to,
                        reason: QualityChangeReason::Stable,
                        ..
                    } = event
                    {
                        prop_assert!(to > from);
                        if let Some(previous) = last_step_up {
                            prop_assert!(now_us.saturating_sub(previous) >= QUALITY_STEP_UP_INTERVAL_US);
                        }
                        last_step_up = Some(now_us);
                        step_ups += 1;
                    }
                }
                prop_assert!(policy.current_tier() <= policy.maximum_tier());
            }
            prop_assert!(step_ups <= 2);
        }
    }

    fn changed_to(events: &[QualityEvent], tier: QualityTier) -> bool {
        events.iter().any(|event| {
            matches!(
                event,
                QualityEvent::TierChanged { to, .. } if *to == tier
            )
        })
    }

    #[test]
    fn packet_loss_requires_three_continuous_seconds_and_trims_before_stepdown() {
        let mut policy = controller(QualityTier::P1080, QualityPreference::Auto);
        let high_loss = feedback(201, 0, 40_000, 40_000);
        assert!(policy.update(0, high_loss).is_empty());
        assert_eq!(
            policy.update(1_000_000, high_loss),
            vec![QualityEvent::BitrateTrim {
                from_bps: BITRATE_1080_BPS,
                to_bps: 6_300_000,
                percent: 90,
            }]
        );
        assert_eq!(
            policy.update(2_000_000, high_loss),
            vec![QualityEvent::BitrateTrim {
                from_bps: 6_300_000,
                to_bps: 5_600_000,
                percent: 80,
            }]
        );
        assert_eq!(
            policy.update(3_000_000, high_loss),
            vec![QualityEvent::BitrateTrim {
                from_bps: 5_600_000,
                to_bps: 4_900_000,
                percent: 70,
            }]
        );
        let events = policy.update(4_000_000, high_loss);
        assert_eq!(
            events,
            vec![QualityEvent::TierChanged {
                from: QualityTier::P1080,
                to: QualityTier::P720,
                reason: QualityChangeReason::Loss,
                target_bitrate_bps: BITRATE_720_BPS,
            }]
        );
        assert_eq!(policy.current_tier(), QualityTier::P720);
        assert_eq!(policy.current_bitrate_bps(), BITRATE_720_BPS);
    }

    #[test]
    fn frame_loss_uses_the_same_three_second_dwell_and_threshold_is_strict() {
        let mut policy = controller(QualityTier::P720, QualityPreference::Auto);
        assert!(policy
            .update(0, feedback(200, 500, 10_000, 10_000))
            .is_empty());
        let high_frame_loss = feedback(0, 501, 10_000, 10_000);
        assert!(policy.update(1_000_000, high_frame_loss).is_empty());
        assert_eq!(
            policy.update(3_999_999, high_frame_loss),
            vec![QualityEvent::BitrateTrim {
                from_bps: BITRATE_720_BPS,
                to_bps: 2_800_000,
                percent: 80,
            }]
        );
        assert_eq!(
            policy.update(4_000_000, high_frame_loss),
            vec![QualityEvent::BitrateTrim {
                from_bps: 2_800_000,
                to_bps: 2_450_000,
                percent: 70,
            }]
        );
        assert!(changed_to(
            &policy.update(5_000_000, high_frame_loss),
            QualityTier::P480
        ));
    }

    #[test]
    fn pending_loss_downshift_is_cancelled_when_pressure_recovers() {
        let mut policy = controller(QualityTier::P1080, QualityPreference::Auto);
        let high_loss = feedback(201, 0, 40_000, 40_000);
        assert!(policy.update(0, high_loss).is_empty());
        assert_eq!(policy.update(1_000_000, high_loss).len(), 1);
        assert_eq!(policy.update(2_000_000, high_loss).len(), 1);
        assert_eq!(
            policy.update(3_000_000, high_loss),
            vec![QualityEvent::BitrateTrim {
                from_bps: 5_600_000,
                to_bps: 4_900_000,
                percent: 70,
            }]
        );

        assert!(policy
            .update(4_000_000, feedback(0, 0, 40_000, 40_000))
            .is_empty());
        assert_eq!(policy.current_tier(), QualityTier::P1080);
        assert_eq!(policy.current_bitrate_bps(), 4_900_000);
    }

    #[test]
    fn rtt_inflation_requires_five_seconds_above_three_times_baseline() {
        let mut policy = controller(QualityTier::P1080, QualityPreference::Auto);
        assert!(policy.update(0, feedback(0, 0, 30_000, 10_000)).is_empty());
        let high_rtt = feedback(0, 0, 30_001, 10_000);
        assert!(policy.update(10, high_rtt).is_empty());
        assert_eq!(
            policy.update(5_000_009, high_rtt),
            vec![QualityEvent::BitrateTrim {
                from_bps: BITRATE_1080_BPS,
                to_bps: 4_900_000,
                percent: 70,
            }]
        );
        assert!(policy.update(5_000_010, high_rtt).is_empty());
        let events = policy.update(5_000_011, high_rtt);
        assert!(matches!(
            events.last(),
            Some(QualityEvent::TierChanged {
                to: QualityTier::P720,
                reason: QualityChangeReason::RttInflation,
                ..
            })
        ));
    }

    #[test]
    fn two_overflows_within_two_seconds_step_down_once_and_hold_hysteresis() {
        let mut policy = controller(QualityTier::P1080, QualityPreference::Auto);
        let mut overflow = feedback(0, 0, 20_000, 20_000);
        overflow.queue_overflow = true;
        assert!(policy.update(1_000_000, overflow).is_empty());
        assert_eq!(
            policy.update(3_000_000, overflow),
            vec![
                QualityEvent::BitrateTrim {
                    from_bps: BITRATE_1080_BPS,
                    to_bps: 4_900_000,
                    percent: 70,
                },
                QualityEvent::TierChanged {
                    from: QualityTier::P1080,
                    to: QualityTier::P720,
                    reason: QualityChangeReason::QueueOverflow,
                    target_bitrate_bps: BITRATE_720_BPS,
                },
            ]
        );
        assert!(policy
            .update(3_000_001, feedback(0, 0, 20_000, 20_000))
            .is_empty());
        assert_eq!(policy.current_tier(), QualityTier::P720);
    }

    #[test]
    fn marginal_loss_noise_does_not_oscillate_and_recovery_respects_stepup_rate() {
        let mut policy = controller(QualityTier::P480, QualityPreference::Auto);
        let mut last_step_up = None;
        let mut step_ups = Vec::new();

        // Alternate just below and just above the packet-loss threshold. Every
        // above-threshold burst ends before the 3-second dwell can expire.
        for tick in 0..600u64 {
            let loss_bps = if tick % 2 == 0 { 199 } else { 201 };
            let now_us = tick * 250_000;
            let events = policy.update(now_us, feedback(loss_bps, 0, 10_000, 10_000));
            assert!(
                events.is_empty(),
                "marginal loss changed quality at {now_us}"
            );
            assert_eq!(policy.current_tier(), QualityTier::P480);
            assert_eq!(policy.current_bitrate_bps(), BITRATE_480_BPS);
        }

        // Once loss becomes stable, tier increases are gradual and remain at
        // least 30 seconds apart, with the host/display tier cap still held.
        for tick in 0..=100u64 {
            let now_us = 150_000_000 + tick * 1_000_000;
            for event in policy.update(now_us, feedback(0, 0, 10_000, 10_000)) {
                if let QualityEvent::TierChanged {
                    from,
                    to,
                    reason: QualityChangeReason::Stable,
                    ..
                } = event
                {
                    assert!(to > from);
                    if let Some(previous) = last_step_up {
                        assert!(now_us - previous >= QUALITY_STEP_UP_INTERVAL_US);
                    }
                    last_step_up = Some(now_us);
                    step_ups.push(now_us);
                }
            }
            assert!(policy.current_tier() <= policy.maximum_tier());
        }

        assert_eq!(step_ups.len(), 2);
        assert!(step_ups[1] - step_ups[0] >= QUALITY_STEP_UP_INTERVAL_US);
        assert_eq!(policy.current_tier(), QualityTier::P1080);
    }

    #[test]
    fn stable_upshift_waits_twenty_seconds_and_then_respects_thirty_second_limit() {
        let mut policy = controller(QualityTier::P480, QualityPreference::Auto);
        let stable = feedback(0, 0, 40_000, 40_000);
        assert!(policy.update(0, stable).is_empty());
        assert!(changed_to(
            &policy.update(20_000_000, stable),
            QualityTier::P720
        ));
        assert!(policy.update(49_999_999, stable).is_empty());
        assert!(changed_to(
            &policy.update(50_000_000, stable),
            QualityTier::P1080
        ));
        assert!(policy.update(80_000_000, stable).is_empty());
    }

    #[test]
    fn automatic_downshift_cannot_step_back_up_before_the_cooldown() {
        let mut policy = controller(QualityTier::P1080, QualityPreference::Auto);
        let high_loss = feedback(201, 0, 40_000, 40_000);
        assert!(policy.update(0, high_loss).is_empty());
        assert_eq!(policy.update(1_000_000, high_loss).len(), 1);
        assert_eq!(policy.update(2_000_000, high_loss).len(), 1);
        assert_eq!(
            policy.update(3_000_000, high_loss),
            vec![QualityEvent::BitrateTrim {
                from_bps: 5_600_000,
                to_bps: 4_900_000,
                percent: 70,
            }]
        );
        assert!(changed_to(
            &policy.update(4_000_000, high_loss),
            QualityTier::P720
        ));

        let stable = feedback(0, 0, 40_000, 40_000);
        assert!(policy.update(4_000_001, stable).is_empty());
        assert!(policy.update(24_000_001, stable).is_empty());
        assert!(policy.update(33_999_999, stable).is_empty());
        assert!(changed_to(
            &policy.update(34_000_000, stable),
            QualityTier::P1080
        ));
    }

    #[test]
    fn fixed_preference_disables_automatic_steps_and_all_tiers_are_clamped() {
        let mut fixed = QualityController::new(
            1080,
            720,
            QualityTier::P1080,
            QualityPreference::Fixed(QualityTier::P1080),
        )
        .unwrap_or_else(|error| panic!("controller: {error}"));
        assert_eq!(fixed.current_tier(), QualityTier::P720);
        assert_eq!(fixed.maximum_tier(), QualityTier::P720);
        let high = feedback(1_000, 2_000, 100_000, 10_000);
        assert!(fixed.update(0, high).is_empty());
        assert_eq!(
            fixed.update(60_000_000, high),
            vec![QualityEvent::BitrateTrim {
                from_bps: BITRATE_720_BPS,
                to_bps: 2_450_000,
                percent: 70,
            }]
        );
        assert_eq!(fixed.current_tier(), QualityTier::P720);
        let stable = feedback(0, 0, 10_000, 10_000);
        assert!(fixed.update(61_000_000, stable).is_empty());
        assert_eq!(
            fixed.update(64_000_000, stable),
            vec![QualityEvent::BitrateTrim {
                from_bps: 2_450_000,
                to_bps: BITRATE_720_BPS,
                percent: 100,
            }]
        );

        let capped = QualityController::new(1080, 700, QualityTier::P1080, QualityPreference::Auto)
            .unwrap_or_else(|error| panic!("controller: {error}"));
        assert_eq!(capped.maximum_tier(), QualityTier::P480);
        assert_eq!(capped.current_tier(), QualityTier::P480);
        assert_eq!(
            QualityController::new(720, 400, QualityTier::P720, QualityPreference::Auto),
            Err(QualityConfigError::BelowMinimumHeight)
        );
    }

    #[test]
    fn overflow_history_stays_bounded_under_a_sustained_overflow_signal() {
        let mut fixed = controller(
            QualityTier::P1080,
            QualityPreference::Fixed(QualityTier::P1080),
        );
        let mut overflow = feedback(0, 0, 10_000, 10_000);
        overflow.queue_overflow = true;
        for tick in 0..1_000 {
            fixed.update(tick * 100, overflow);
            assert!(fixed.overflow_times_us.len() <= 2);
        }
    }

    #[test]
    fn minimum_tier_can_trim_to_seventy_percent_without_stepping_lower() {
        let mut policy = controller(QualityTier::P480, QualityPreference::Auto);
        let high_loss = feedback(201, 0, 10_000, 10_000);
        assert!(policy.update(0, high_loss).is_empty());
        assert_eq!(
            policy.update(3_000_000, high_loss),
            vec![QualityEvent::BitrateTrim {
                from_bps: BITRATE_480_BPS,
                to_bps: 1_050_000,
                percent: 70,
            }]
        );
        assert!(policy.update(4_000_000, high_loss).is_empty());
        assert_eq!(policy.current_tier(), QualityTier::P480);
        assert_eq!(policy.current_bitrate_bps(), 1_050_000);
    }

    #[test]
    fn fixed_preference_change_is_clamped_and_reports_bitrate_before_tier() {
        let mut policy = controller(QualityTier::P480, QualityPreference::Auto);
        let events = policy.set_preference(QualityPreference::Fixed(QualityTier::P1080), 100);
        assert!(matches!(
            events.as_slice(),
            [QualityEvent::TierChanged {
                to: QualityTier::P1080,
                reason: QualityChangeReason::Preference,
                ..
            }]
        ));
        assert_eq!(
            policy.preference(),
            QualityPreference::Fixed(QualityTier::P1080)
        );
        assert!(policy
            .update(100_000_000, feedback(0, 0, 10, 10))
            .is_empty());
        assert_eq!(policy.current_tier(), QualityTier::P1080);
    }

    #[test]
    fn recovery_requires_low_loss_and_rtt_at_most_one_and_a_half_times_baseline() {
        let mut policy = controller(QualityTier::P480, QualityPreference::Auto);
        let mut threshold = feedback(50, 0, 15_000, 10_000);
        assert!(policy.update(0, threshold).is_empty());
        assert!(policy.update(25_000_000, threshold).is_empty());

        threshold.loss_bps = 49;
        assert!(policy.update(26_000_000, threshold).is_empty());
        threshold.rtt_us = 15_001;
        assert!(policy.update(50_000_000, threshold).is_empty());
        threshold.rtt_us = 15_000;
        assert!(policy.update(51_000_000, threshold).is_empty());
        assert!(changed_to(
            &policy.update(71_000_000, threshold),
            QualityTier::P720
        ));
    }

    #[test]
    fn decoder_and_encoder_observations_are_carried_without_invented_thresholds() {
        let mut policy = controller(QualityTier::P480, QualityPreference::Auto);
        let mut sample = feedback(0, 0, 10_000, 10_000);
        sample.decode_ms_p95 = Some(10_000);
        sample.dropped_frames = u32::MAX;
        sample.encoder_lag_ms = Some(10_000);
        assert!(policy.update(0, sample).is_empty());
        assert!(policy.update(20_000_000, sample).iter().any(|event| {
            matches!(
                event,
                QualityEvent::TierChanged {
                    to: QualityTier::P720,
                    ..
                }
            )
        }));
    }

    #[test]
    fn stream_reset_clears_old_dwell_and_overflow_history() {
        let mut pending = controller(QualityTier::P1080, QualityPreference::Auto);
        let mut overflow = feedback(0, 0, 10_000, 10_000);
        overflow.queue_overflow = true;
        assert!(pending.update(0, overflow).is_empty());
        assert!(changed_to(
            &pending.update(1_000_000, overflow),
            QualityTier::P720
        ));
        pending.reset_feedback_window(1_500_000);
        assert!(pending
            .update(2_000_000, feedback(0, 0, 10_000, 10_000))
            .is_empty());
        assert_eq!(pending.current_tier(), QualityTier::P720);

        let mut policy = controller(QualityTier::P1080, QualityPreference::Auto);
        let high_loss = feedback(201, 0, 40_000, 40_000);
        assert!(policy.update(0, high_loss).is_empty());
        assert_eq!(policy.update(1_000_000, high_loss).len(), 1);
        policy.reset_feedback_window(1_500_000);
        assert_eq!(policy.current_tier(), QualityTier::P1080);
        assert!(policy.update(2_000_000, high_loss).is_empty());
        assert_eq!(
            policy.update(4_999_999, high_loss),
            vec![QualityEvent::BitrateTrim {
                from_bps: 6_300_000,
                to_bps: 5_600_000,
                percent: 80,
            }]
        );
        assert_eq!(
            policy.update(5_000_000, high_loss),
            vec![QualityEvent::BitrateTrim {
                from_bps: 5_600_000,
                to_bps: 4_900_000,
                percent: 70,
            }]
        );
        assert!(changed_to(
            &policy.update(5_000_001, high_loss),
            QualityTier::P720
        ));
    }

    #[test]
    fn tier_caps_and_automatic_change_spacing_hold_across_height_combinations() {
        let stable = feedback(0, 0, 10_000, 10_000);
        for host_height in (QUALITY_MIN_HEIGHT..=QUALITY_MAX_HEIGHT).step_by(37) {
            for display_height in (QUALITY_MIN_HEIGHT..=QUALITY_MAX_HEIGHT).step_by(41) {
                let permitted_height = host_height.min(display_height);
                let mut policy = QualityController::new(
                    host_height,
                    display_height,
                    QualityTier::P480,
                    QualityPreference::Auto,
                )
                .unwrap_or_else(|error| panic!("valid bounds: {error}"));
                let mut last_change = None;
                for tick in 0..=8 {
                    let now_us = tick * 10_000_000;
                    for event in policy.update(now_us, stable) {
                        if let QualityEvent::TierChanged {
                            to,
                            reason: QualityChangeReason::Stable,
                            ..
                        } = event
                        {
                            assert!(to.height() <= permitted_height);
                            if let Some(previous) = last_change {
                                assert!(now_us - previous >= QUALITY_STEP_UP_INTERVAL_US);
                            }
                            last_change = Some(now_us);
                        }
                    }
                    assert!(policy.current_tier().height() <= permitted_height);
                }
            }
        }
    }

    #[test]
    fn backwards_virtual_time_cannot_create_a_premature_stepdown() {
        let mut policy = controller(QualityTier::P1080, QualityPreference::Auto);
        let high = feedback(201, 0, 1, 1);
        assert!(policy.update(5_000_000, high).is_empty());
        assert!(policy.update(1, high).is_empty());
        assert_eq!(
            policy.update(7_999_999, high),
            vec![QualityEvent::BitrateTrim {
                from_bps: BITRATE_1080_BPS,
                to_bps: 5_600_000,
                percent: 80,
            }]
        );
        assert_eq!(
            policy.update(8_000_000, high),
            vec![QualityEvent::BitrateTrim {
                from_bps: 5_600_000,
                to_bps: 4_900_000,
                percent: 70,
            }]
        );
        assert!(changed_to(
            &policy.update(8_000_001, high),
            QualityTier::P720
        ));
    }
}
