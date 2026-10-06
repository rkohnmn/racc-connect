//! Data-only recovery mechanism models. These types do not alter `racc-net` or the wire format.

use crate::StreamTier;
use std::fmt;

const FRAME_INTERVAL_US: u64 = 33_333;
const PACED_FRAME_US: u64 = 20_000;
const REORDER_WINDOW_US: u64 = 8_000;
const ENCODE_DELAY_US: u64 = 10_000;
const MAX_PAYLOAD: usize = 1182;
const MAX_DATAGRAM: usize = 1200;
const NACK_RING_RETENTION_US: u64 = 250_000;

/// Recovery strategy modeled in the testkit only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryMechanism {
    /// Existing keyframe request policy.
    Baseline,
    /// Retransmit only missing keyframe fragments; predictive losses use baseline recovery.
    NackKey,
    /// Retransmit missing fragments on all frames, at most twice.
    NackAll,
    /// One XOR parity datagram per group of ten data datagrams.
    Fec10,
    /// One XOR parity datagram per group of twenty data datagrams.
    Fec20,
    /// NACK-all plus XOR-20.
    NackAllFec20,
}

impl RecoveryMechanism {
    /// Mechanisms in the audit comparison order.
    pub const ALL: [Self; 6] = [
        Self::Baseline,
        Self::NackKey,
        Self::NackAll,
        Self::Fec10,
        Self::Fec20,
        Self::NackAllFec20,
    ];

    const fn fec_group(self) -> Option<usize> {
        match self {
            Self::Fec10 => Some(10),
            Self::Fec20 | Self::NackAllFec20 => Some(20),
            Self::Baseline | Self::NackKey | Self::NackAll => None,
        }
    }

    const fn nack(self, keyframe: bool) -> bool {
        matches!(self, Self::NackAll | Self::NackAllFec20)
            || (keyframe && matches!(self, Self::NackKey))
    }
}

impl fmt::Display for RecoveryMechanism {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Baseline => "baseline",
            Self::NackKey => "nack-key",
            Self::NackAll => "nack-all",
            Self::Fec10 => "fec-10",
            Self::Fec20 => "fec-20",
            Self::NackAllFec20 => "nack-all+fec-20",
        })
    }
}

/// Inputs for one reproducible 600-second or shorter recovery comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryScenario {
    /// Stream resolution/bitrate tier.
    pub tier: StreamTier,
    /// Independent datagram loss, in millionths.
    pub loss_ppm: u32,
    /// Use the established Gilbert-Elliott burst profile instead of iid loss.
    pub burst_loss: bool,
    /// One-way path delay in microseconds.
    pub one_way_delay_us: u64,
    /// Virtual duration in seconds.
    pub duration_seconds: u64,
    /// Reproducible random seed.
    pub seed: u64,
    /// Keyframe size relative to an average P-frame.
    pub keyframe_multiplier: usize,
}

/// Measured output from one simulator-only recovery model.
#[derive(Clone, Debug, PartialEq)]
pub struct RecoveryMetrics {
    /// Scenario recovery strategy.
    pub mechanism: RecoveryMechanism,
    /// Stream tier.
    pub tier: StreamTier,
    /// Configured iid loss percent (zero for the separately labeled burst case).
    pub loss_percent: f64,
    /// Observed lost datagrams as a percentage of all initial, parity and retry datagrams.
    pub packet_loss_observed_percent: f64,
    /// Whether the burst profile was used.
    pub burst_loss: bool,
    /// One-way delay in milliseconds.
    pub one_way_delay_ms: f64,
    /// Frames delivered as a percentage of source frames.
    pub delivered_percent: f64,
    /// Time stale beyond one frame interval, as a percentage of run duration.
    pub stale_percent: f64,
    /// Median stale freeze duration in milliseconds.
    pub median_freeze_ms: f64,
    /// 95th percentile stale freeze duration in milliseconds.
    pub p95_freeze_ms: f64,
    /// Keyframe requests per simulated minute.
    pub keyframe_requests_per_minute: f64,
    /// Additional packet bytes relative to the 30 fps source payload, in percent.
    pub bandwidth_overhead_percent: f64,
    /// Median extra delivery latency beyond the paced one-way base, in milliseconds.
    pub added_median_latency_ms: f64,
    /// Peak queued frame bytes in the unconstrained model (in-flight access unit only).
    pub peak_frame_bytes: usize,
}

/// Runs a seeded packet-level model with the requested simulator-only strategy.
pub fn run_recovery_model(
    mechanism: RecoveryMechanism,
    scenario: RecoveryScenario,
) -> RecoveryMetrics {
    let frame_count = scenario.duration_seconds.saturating_mul(1_000_000) / FRAME_INTERVAL_US;
    let p_bytes = scenario.tier.average_p_frame_bytes();
    let key_bytes = p_bytes.saturating_mul(scenario.keyframe_multiplier.max(1));
    let mut channel = Channel::new(scenario.seed, scenario.loss_ppm, scenario.burst_loss);
    let mut waiting_for_keyframe = false;
    let mut next_keyframe_at = None;
    let mut unanswered_backoff_ms = 200_u64;
    let mut requests = 0_u64;
    let mut delivered = 0_u64;
    let mut last_delivery_us: Option<u64> = None;
    let mut stale_us = 0_u64;
    let mut freezes_us = Vec::new();
    let mut added_latency_us = Vec::new();
    let mut transmitted_extra_bytes = 0_u128;
    let mut baseline_source_bytes = u128::from(frame_count).saturating_mul(p_bytes as u128);
    if frame_count > 0 {
        baseline_source_bytes =
            baseline_source_bytes.saturating_add(key_bytes.saturating_sub(p_bytes) as u128);
    }
    let mut peak_frame_bytes = 0usize;

    for frame_index in 0..frame_count {
        let frame_start = frame_index.saturating_mul(FRAME_INTERVAL_US);
        let response_keyframe =
            waiting_for_keyframe && next_keyframe_at.is_some_and(|due| frame_start >= due);
        let keyframe = frame_index == 0 || response_keyframe;
        let frame_bytes = if keyframe { key_bytes } else { p_bytes };
        peak_frame_bytes = peak_frame_bytes.max(frame_bytes);
        if response_keyframe {
            transmitted_extra_bytes =
                transmitted_extra_bytes.saturating_add(key_bytes.saturating_sub(p_bytes) as u128);
            next_keyframe_at = None;
        }
        let data_fragments = fragment_lengths(frame_bytes);
        let fec_group = mechanism.fec_group();
        let parity_lengths =
            fec_group.map_or_else(Vec::new, |group| parity_lengths(&data_fragments, group));
        let mut missing = Vec::with_capacity(data_fragments.len());
        for _ in &data_fragments {
            missing.push(channel.lost());
        }
        let mut fec_recovered = 0usize;
        for (group_index, parity_len) in parity_lengths.iter().enumerate() {
            let parity_lost = channel.lost();
            transmitted_extra_bytes =
                transmitted_extra_bytes.saturating_add((*parity_len + 18) as u128);
            let start = group_index * fec_group.unwrap_or(usize::MAX);
            let end = (start + fec_group.unwrap_or(0)).min(missing.len());
            let lost_indexes = (start..end)
                .filter(|index| missing[*index])
                .collect::<Vec<_>>();
            if !parity_lost && lost_indexes.len() == 1 {
                missing[lost_indexes[0]] = false;
                fec_recovered += 1;
            }
        }
        let initial_missing = missing.iter().filter(|value| **value).count();
        let mut retry_latency = 0_u64;
        if initial_missing > 0 && mechanism.nack(keyframe) {
            for _round in 0..2 {
                let outstanding = missing.iter().filter(|value| **value).count();
                if outstanding == 0 {
                    break;
                }
                transmitted_extra_bytes = transmitted_extra_bytes.saturating_add(64);
                let pacing = PACED_FRAME_US
                    .saturating_mul(outstanding as u64)
                    .saturating_div(data_fragments.len().max(1) as u64);
                // Wait one RTT plus the reorder window, then include the
                // request and retransmit one-way trips.
                let round_latency = scenario
                    .one_way_delay_us
                    .saturating_mul(4)
                    .saturating_add(REORDER_WINDOW_US)
                    .saturating_add(pacing);
                let fragment_age = PACED_FRAME_US
                    .saturating_add(scenario.one_way_delay_us)
                    .saturating_add(retry_latency)
                    .saturating_add(round_latency);
                if fragment_age > NACK_RING_RETENTION_US {
                    break;
                }
                retry_latency = retry_latency.saturating_add(round_latency);
                for fragment_missing in &mut missing {
                    if *fragment_missing {
                        transmitted_extra_bytes =
                            transmitted_extra_bytes.saturating_add(MAX_DATAGRAM as u128);
                        if !channel.lost() {
                            *fragment_missing = false;
                        }
                    }
                }
            }
        }
        let frame_ok = missing.iter().all(|value| !*value);
        let parity_pacing = PACED_FRAME_US
            .saturating_mul(parity_lengths.len() as u64)
            .saturating_div(data_fragments.len().max(1) as u64);
        let base_arrival = frame_start
            .saturating_add(PACED_FRAME_US)
            .saturating_add(parity_pacing)
            .saturating_add(scenario.one_way_delay_us);
        if frame_ok {
            if !waiting_for_keyframe || keyframe {
                let arrival = base_arrival.saturating_add(retry_latency);
                if keyframe && waiting_for_keyframe {
                    waiting_for_keyframe = false;
                    unanswered_backoff_ms = 200;
                    next_keyframe_at = None;
                }
                delivered = delivered.saturating_add(1);
                if let Some(previous) = last_delivery_us {
                    let freeze = arrival.saturating_sub(previous.saturating_add(FRAME_INTERVAL_US));
                    if freeze > 0 {
                        stale_us = stale_us.saturating_add(freeze);
                        freezes_us.push(freeze);
                    }
                }
                last_delivery_us = Some(arrival);
                added_latency_us.push(parity_pacing.saturating_add(retry_latency));
            }
        } else if keyframe {
            if waiting_for_keyframe {
                let due = frame_start.saturating_add(unanswered_backoff_ms.saturating_mul(1000));
                next_keyframe_at = Some(round_up(due, FRAME_INTERVAL_US));
                unanswered_backoff_ms = unanswered_backoff_ms.saturating_mul(2).min(1000);
                requests = requests.saturating_add(1);
                transmitted_extra_bytes = transmitted_extra_bytes.saturating_add(64);
            } else {
                waiting_for_keyframe = true;
                requests = requests.saturating_add(1);
                transmitted_extra_bytes = transmitted_extra_bytes.saturating_add(64);
                let request_arrival = frame_start
                    .saturating_add(PACED_FRAME_US)
                    .saturating_add(scenario.one_way_delay_us)
                    .saturating_add(REORDER_WINDOW_US)
                    .saturating_add(scenario.one_way_delay_us)
                    .saturating_add(ENCODE_DELAY_US);
                next_keyframe_at = Some(round_up(request_arrival, FRAME_INTERVAL_US));
            }
        } else if initial_missing > fec_recovered {
            let can_retry = mechanism.nack(false) && missing.iter().all(|value| !*value);
            if !can_retry && !waiting_for_keyframe {
                waiting_for_keyframe = true;
                requests = requests.saturating_add(1);
                transmitted_extra_bytes = transmitted_extra_bytes.saturating_add(64);
                let loss_detected = frame_start
                    .saturating_add(PACED_FRAME_US)
                    .saturating_add(scenario.one_way_delay_us)
                    .saturating_add(REORDER_WINDOW_US);
                let request_arrival = loss_detected
                    .saturating_add(scenario.one_way_delay_us)
                    .saturating_add(ENCODE_DELAY_US);
                next_keyframe_at = Some(round_up(request_arrival, FRAME_INTERVAL_US));
            }
        }
    }
    let run_us = frame_count.saturating_mul(FRAME_INTERVAL_US).max(1);
    if let Some(last) = last_delivery_us {
        let tail = run_us.saturating_sub(last.saturating_add(FRAME_INTERVAL_US));
        if tail > 0 {
            stale_us = stale_us.saturating_add(tail);
            freezes_us.push(tail);
        }
    } else {
        stale_us = run_us;
        freezes_us.push(run_us);
    }
    freezes_us.sort_unstable();
    added_latency_us.sort_unstable();
    RecoveryMetrics {
        mechanism,
        tier: scenario.tier,
        loss_percent: f64::from(scenario.loss_ppm) / 10_000.0,
        packet_loss_observed_percent: channel.observed_loss_percent(),
        burst_loss: scenario.burst_loss,
        one_way_delay_ms: scenario.one_way_delay_us as f64 / 1000.0,
        delivered_percent: if frame_count == 0 {
            0.0
        } else {
            delivered as f64 * 100.0 / frame_count as f64
        },
        stale_percent: stale_us as f64 * 100.0 / run_us as f64,
        median_freeze_ms: percentile_ms(&freezes_us, 0.50),
        p95_freeze_ms: percentile_ms(&freezes_us, 0.95),
        keyframe_requests_per_minute: requests as f64 * 60.0
            / scenario.duration_seconds.max(1) as f64,
        bandwidth_overhead_percent: if baseline_source_bytes == 0 {
            0.0
        } else {
            transmitted_extra_bytes as f64 * 100.0 / baseline_source_bytes as f64
        },
        added_median_latency_ms: percentile_ms(&added_latency_us, 0.50),
        peak_frame_bytes,
    }
}

fn fragment_lengths(frame_bytes: usize) -> Vec<usize> {
    let count = frame_bytes.div_ceil(MAX_PAYLOAD).max(1);
    (0..count)
        .map(|index| {
            if index + 1 == count {
                frame_bytes - index * MAX_PAYLOAD
            } else {
                MAX_PAYLOAD
            }
        })
        .collect()
}

fn parity_lengths(data: &[usize], group_size: usize) -> Vec<usize> {
    data.chunks(group_size)
        .map(|group| group.iter().copied().max().unwrap_or(0))
        .collect()
}

fn round_up(value: u64, quantum: u64) -> u64 {
    value.saturating_add(quantum - 1) / quantum * quantum
}

fn percentile_ms(samples: &[u64], percentile: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let index = ((samples.len() - 1) as f64 * percentile).ceil() as usize;
    samples.get(index).copied().unwrap_or(0) as f64 / 1000.0
}

#[derive(Clone, Debug)]
struct Channel {
    rng: Rng,
    loss_ppm: u32,
    burst: bool,
    bad: bool,
    attempted: u64,
    lost_count: u64,
}

impl Channel {
    fn new(seed: u64, loss_ppm: u32, burst: bool) -> Self {
        Self {
            rng: Rng::new(seed),
            loss_ppm,
            burst,
            bad: false,
            attempted: 0,
            lost_count: 0,
        }
    }

    fn lost(&mut self) -> bool {
        self.attempted = self.attempted.saturating_add(1);
        let lost = if self.burst {
            if self.bad {
                if self.rng.probability(100_000) {
                    self.bad = false;
                }
            } else if self.rng.probability(1_000) {
                self.bad = true;
            }
            let in_bad = self.bad && self.rng.probability(250_000);
            in_bad || self.rng.probability(self.loss_ppm)
        } else {
            self.rng.probability(self.loss_ppm)
        };
        if lost {
            self.lost_count = self.lost_count.saturating_add(1);
        }
        lost
    }

    fn observed_loss_percent(&self) -> f64 {
        if self.attempted == 0 {
            0.0
        } else {
            self.lost_count as f64 * 100.0 / self.attempted as f64
        }
    }
}

#[derive(Clone, Debug)]
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9e37_79b9_7f4a_7c15
            } else {
                seed
            },
        }
    }
    fn next(&mut self) -> u64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        self.state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn probability(&mut self, ppm: u32) -> bool {
        self.next() % 1_000_000 < u64::from(ppm.min(1_000_000))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_link_has_no_redundancy_overhead_or_freezes() {
        let scenario = RecoveryScenario {
            tier: StreamTier::P720,
            loss_ppm: 0,
            burst_loss: false,
            one_way_delay_us: 1_000,
            duration_seconds: 10,
            seed: 17,
            keyframe_multiplier: 8,
        };
        for mechanism in RecoveryMechanism::ALL {
            let result = run_recovery_model(mechanism, scenario);
            assert_eq!(result.delivered_percent, 100.0, "{mechanism}");
            assert_eq!(result.keyframe_requests_per_minute, 0.0, "{mechanism}");
            assert!(result.stale_percent <= 0.02, "{mechanism}: {result:?}");
            if matches!(
                mechanism,
                RecoveryMechanism::Fec10
                    | RecoveryMechanism::Fec20
                    | RecoveryMechanism::NackAllFec20
            ) {
                assert!(result.bandwidth_overhead_percent > 0.0, "{mechanism}");
            } else {
                assert_eq!(result.bandwidth_overhead_percent, 0.0, "{mechanism}");
            }
        }
    }

    #[test]
    fn parity_datagrams_stay_within_the_1200_byte_limit() {
        let data = fragment_lengths(StreamTier::P1080.keyframe_bytes());
        for group in [10, 20] {
            assert!(parity_lengths(&data, group)
                .iter()
                .all(|payload| payload + 18 <= MAX_DATAGRAM));
        }
        assert_eq!(MAX_PAYLOAD + 18, MAX_DATAGRAM);
    }

    #[test]
    fn fec_models_charge_parity_and_nack_models_charge_retransmissions() {
        let scenario = RecoveryScenario {
            tier: StreamTier::P720,
            loss_ppm: 20_000,
            burst_loss: false,
            one_way_delay_us: 1_000,
            duration_seconds: 20,
            seed: 913,
            keyframe_multiplier: 8,
        };
        let fec = run_recovery_model(RecoveryMechanism::Fec20, scenario);
        let nack = run_recovery_model(RecoveryMechanism::NackAll, scenario);
        assert!(fec.bandwidth_overhead_percent >= 4.0);
        assert!(nack.bandwidth_overhead_percent > 0.0);
        assert!(nack.keyframe_requests_per_minute <= fec.keyframe_requests_per_minute);
    }
}
