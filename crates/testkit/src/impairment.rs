use std::collections::BTreeMap;

/// Probability denominator used by impairment profiles.
pub const PROBABILITY_SCALE: u32 = 1_000_000;
/// Default finite virtual-network packet queue budget.
pub const DEFAULT_SIMULATION_QUEUE_BYTES: usize = 64 * 1024 * 1024;

/// Configurable deterministic packet impairment profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImpairmentProfile {
    /// Independent packet drop probability, in millionths.
    pub independent_loss_ppm: u32,
    /// Probability that a good burst-loss state switches to bad, in millionths per packet.
    pub good_to_bad_ppm: u32,
    /// Probability that a bad burst-loss state switches to good, in millionths per packet.
    pub bad_to_good_ppm: u32,
    /// Packet loss probability while in the bad burst state, in millionths.
    pub bad_state_loss_ppm: u32,
    /// Probability of applying a packet reordering delay, in millionths.
    pub reorder_probability_ppm: u32,
    /// Maximum packet displacement introduced by reordering.
    pub reorder_max_displacement_packets: u16,
    /// Probability that a packet is duplicated, in millionths.
    pub duplicate_probability_ppm: u32,
    /// Base one-way network delay.
    pub one_way_delay_us: u64,
    /// Uniform jitter amplitude added or subtracted from the base delay.
    pub jitter_us: u64,
    /// Optional bitrate limit in bits per second.
    pub bitrate_limit_bps: Option<u64>,
    /// Maximum bytes waiting in the virtual network queue.
    pub max_queue_bytes: usize,
}

impl Default for ImpairmentProfile {
    fn default() -> Self {
        Self {
            independent_loss_ppm: 0,
            good_to_bad_ppm: 0,
            bad_to_good_ppm: PROBABILITY_SCALE,
            bad_state_loss_ppm: 0,
            reorder_probability_ppm: 0,
            reorder_max_displacement_packets: 0,
            duplicate_probability_ppm: 0,
            one_way_delay_us: 0,
            jitter_us: 0,
            bitrate_limit_bps: None,
            max_queue_bytes: DEFAULT_SIMULATION_QUEUE_BYTES,
        }
    }
}

/// One packet delivered by the virtual network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveredDatagram {
    /// Virtual arrival time in microseconds.
    pub at_us: u64,
    /// Datagram contents, preserved byte-for-byte.
    pub bytes: Vec<u8>,
}

/// Cumulative statistics for a virtual impairment run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImpairmentStats {
    /// Datagrams submitted to the network.
    pub submitted: u64,
    /// Datagrams discarded by configured loss.
    pub lost: u64,
    /// Datagrams duplicated by the configured profile.
    pub duplicates: u64,
    /// Datagrams discarded when the bounded queue was full.
    pub queue_drops: u64,
    /// Datagrams delivered after the caller advanced the virtual clock.
    pub delivered: u64,
    /// Copies accepted into the virtual network for serialization and delivery.
    pub scheduled: u64,
    /// Peak scheduled bytes in the finite queue.
    pub peak_queue_bytes: usize,
    /// Sum of serialization queue wait across scheduled datagrams, in microseconds.
    pub total_queue_delay_us: u64,
    /// Maximum serialization queue wait observed, in microseconds.
    pub max_queue_delay_us: u64,
}

/// Virtual-clock, seeded packet network with loss, reordering, duplication,
/// delay, jitter, and rate limiting.
#[derive(Clone, Debug)]
pub struct SimulatedNetwork {
    profile: ImpairmentProfile,
    rng: SeededRng,
    queue: BTreeMap<(u64, u64), Vec<u8>>,
    queue_bytes: usize,
    next_sequence: u64,
    next_rate_available_us: u64,
    in_bad_state: bool,
    stats: ImpairmentStats,
}

impl SimulatedNetwork {
    /// Creates a reproducible network impairment simulator.
    pub fn new(seed: u64, profile: ImpairmentProfile) -> Self {
        Self {
            profile,
            rng: SeededRng::new(seed),
            queue: BTreeMap::new(),
            queue_bytes: 0,
            next_sequence: 0,
            next_rate_available_us: 0,
            in_bad_state: false,
            stats: ImpairmentStats::default(),
        }
    }

    /// Submits one datagram at virtual time, applying configured impairments.
    ///
    /// Returns false when the original datagram was lost or the bounded queue
    /// rejected it. Duplicated copies may still be scheduled when true is returned.
    pub fn send(&mut self, now_us: u64, bytes: &[u8]) -> bool {
        self.stats.submitted = self.stats.submitted.saturating_add(1);
        let was_bad = self.in_bad_state;
        if self.in_bad_state {
            if self.rng.probability(self.profile.bad_to_good_ppm) {
                self.in_bad_state = false;
            }
        } else if self.rng.probability(self.profile.good_to_bad_ppm) {
            self.in_bad_state = true;
        }
        let burst_loss = was_bad && self.rng.probability(self.profile.bad_state_loss_ppm);
        if burst_loss || self.rng.probability(self.profile.independent_loss_ppm) {
            self.stats.lost = self.stats.lost.saturating_add(1);
            return false;
        }

        let duplicate = self.rng.probability(self.profile.duplicate_probability_ppm);
        let copy_count = if duplicate { 2usize } else { 1usize };
        let total_bytes = bytes.len().saturating_mul(copy_count);
        let available = self
            .profile
            .max_queue_bytes
            .saturating_sub(self.queue_bytes);
        if total_bytes > available {
            self.stats.queue_drops = self.stats.queue_drops.saturating_add(1);
            return false;
        }

        let jitter = self.rng.signed(self.profile.jitter_us);
        let delay = if jitter < 0 {
            self.profile
                .one_way_delay_us
                .saturating_sub(u64::try_from(jitter.unsigned_abs()).unwrap_or(u64::MAX))
        } else {
            self.profile.one_way_delay_us.saturating_add(jitter as u64)
        };
        let reorder_extra = if self.rng.probability(self.profile.reorder_probability_ppm) {
            let displacement = u64::from(self.profile.reorder_max_displacement_packets.max(1));
            displacement.saturating_mul(delay.max(1000))
        } else {
            0
        };

        let copies = if duplicate { 2usize } else { 1usize };
        for copy_index in 0..copies {
            let service_start = now_us.max(self.next_rate_available_us);
            let queue_wait = service_start.saturating_sub(now_us);
            self.stats.total_queue_delay_us =
                self.stats.total_queue_delay_us.saturating_add(queue_wait);
            self.stats.max_queue_delay_us = self.stats.max_queue_delay_us.max(queue_wait);
            let service_duration = if let Some(rate) =
                self.profile.bitrate_limit_bps.filter(|rate| *rate > 0)
            {
                let numerator = u128::try_from(bytes.len())
                    .unwrap_or(u128::MAX)
                    .saturating_mul(8_000_000);
                let duration = numerator.saturating_add(u128::from(rate - 1)) / u128::from(rate);
                u64::try_from(duration).unwrap_or(u64::MAX)
            } else {
                0
            };
            let service_complete = service_start.saturating_add(service_duration);
            self.next_rate_available_us = service_complete;
            let arrival = service_complete
                .saturating_add(delay)
                .saturating_add(reorder_extra)
                .saturating_add(u64::try_from(copy_index).unwrap_or(u64::MAX));
            self.schedule(arrival, bytes.to_vec());
        }
        if duplicate {
            self.stats.duplicates = self.stats.duplicates.saturating_add(1);
        }
        self.stats.peak_queue_bytes = self.stats.peak_queue_bytes.max(self.queue_bytes);
        true
    }

    /// Delivers every queued datagram due by the requested virtual time.
    pub fn advance_to(&mut self, now_us: u64) -> Vec<DeliveredDatagram> {
        let due = self
            .queue
            .keys()
            .take_while(|(at_us, _)| *at_us <= now_us)
            .copied()
            .collect::<Vec<_>>();
        let mut delivered = Vec::with_capacity(due.len());
        for key in due {
            if let Some(bytes) = self.queue.remove(&key) {
                self.queue_bytes = self.queue_bytes.saturating_sub(bytes.len());
                self.stats.delivered = self.stats.delivered.saturating_add(1);
                delivered.push(DeliveredDatagram {
                    at_us: key.0,
                    bytes,
                });
            }
        }
        delivered
    }

    /// Returns current impairment and bounded-queue statistics.
    pub const fn stats(&self) -> ImpairmentStats {
        self.stats
    }

    /// Current virtual bytes queued for delivery.
    pub const fn queued_bytes(&self) -> usize {
        self.queue_bytes
    }

    fn schedule(&mut self, at_us: u64, bytes: Vec<u8>) {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.stats.scheduled = self.stats.scheduled.saturating_add(1);
        self.queue_bytes = self.queue_bytes.saturating_add(bytes.len());
        self.queue.insert((at_us, sequence), bytes);
    }
}

#[derive(Clone, Debug)]
struct SeededRng {
    state: u64,
}

impl SeededRng {
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

    fn probability(&mut self, probability_ppm: u32) -> bool {
        let threshold = probability_ppm.min(PROBABILITY_SCALE);
        self.next() % u64::from(PROBABILITY_SCALE) < u64::from(threshold)
    }

    fn signed(&mut self, magnitude: u64) -> i128 {
        if magnitude == 0 {
            return 0;
        }
        let width = u128::from(magnitude).saturating_mul(2).saturating_add(1);
        let sample = u128::from(self.next()) % width;
        i128::try_from(sample).unwrap_or(i128::MAX) - i128::from(magnitude)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_replays_loss_duplicate_delay_and_order_decisions() {
        let profile = ImpairmentProfile {
            independent_loss_ppm: 100_000,
            reorder_probability_ppm: 400_000,
            reorder_max_displacement_packets: 2,
            duplicate_probability_ppm: 200_000,
            one_way_delay_us: 1000,
            jitter_us: 400,
            ..ImpairmentProfile::default()
        };
        let mut a = SimulatedNetwork::new(55, profile);
        let mut b = SimulatedNetwork::new(55, profile);
        for index in 0u32..100 {
            let bytes = index.to_le_bytes();
            assert_eq!(
                a.send(u64::from(index) * 100, &bytes),
                b.send(u64::from(index) * 100, &bytes)
            );
        }
        let a_due = a.advance_to(u64::MAX);
        let b_due = b.advance_to(u64::MAX);
        assert_eq!(a_due, b_due);
        assert_eq!(a.stats(), b.stats());
    }

    #[test]
    fn arrivals_include_serialization_time_and_duplicate_uses_link_capacity() {
        let profile = ImpairmentProfile {
            bitrate_limit_bps: Some(8_000),
            duplicate_probability_ppm: PROBABILITY_SCALE,
            one_way_delay_us: 1_000,
            max_queue_bytes: 1024,
            ..ImpairmentProfile::default()
        };
        let mut network = SimulatedNetwork::new(7, profile);
        assert!(network.send(0, &[0; 10])); // 10 ms per copy at 8 kbps.
        let arrivals = network.advance_to(u64::MAX);
        assert_eq!(arrivals.len(), 2);
        assert_eq!(arrivals[0].at_us, 11_000);
        assert_eq!(arrivals[1].at_us, 21_001);
        assert_eq!(network.stats().max_queue_delay_us, 10_000);
    }

    #[test]
    fn rate_queue_has_a_finite_byte_bound() {
        let profile = ImpairmentProfile {
            bitrate_limit_bps: Some(8),
            max_queue_bytes: 8,
            ..ImpairmentProfile::default()
        };
        let mut network = SimulatedNetwork::new(1, profile);
        assert!(network.send(0, &[1; 4]));
        assert!(!network.send(0, &[2; 8]));
        assert!(network.queued_bytes() <= 8);
        assert_eq!(network.stats().queue_drops, 1);
    }
}
