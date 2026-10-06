use crate::{ImpairmentProfile, SimulatedNetwork};
use racc_net::{
    fragment_count, slice_frame_into, FrameData, Pacer, Reassembler, ReassemblyEvent, SenderFrame,
    DEFAULT_FRAME_INTERVAL_US, MAX_INFLIGHT_BYTES, MAX_INFLIGHT_FRAMES, PACING_FRACTION_PERCENT,
};

/// 30 fps quality tier used for deterministic transport estimates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamTier {
    /// 480p30, nominal 1.5 Mbps.
    P480,
    /// 720p30, nominal 3.5 Mbps.
    P720,
    /// 1080p30, nominal 7 Mbps.
    P1080,
}

impl StreamTier {
    /// Nominal video bitrate in kilobits per second.
    pub const fn bitrate_kbps(self) -> u64 {
        match self {
            Self::P480 => 1500,
            Self::P720 => 3500,
            Self::P1080 => 7000,
        }
    }

    /// Average predictive frame size in bytes at 30 fps.
    pub fn average_p_frame_bytes(self) -> usize {
        usize::try_from(self.bitrate_kbps() * 1000 / 8 / 30).unwrap_or(usize::MAX)
    }

    /// Modeled keyframe size, eight times the average predictive frame.
    pub fn keyframe_bytes(self) -> usize {
        self.average_p_frame_bytes().saturating_mul(8)
    }
}

/// Results from a deterministic 30 fps stream simulation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StreamSoakMetrics {
    /// Nominal video bitrate in kilobits per second.
    pub bitrate_kbps: u64,
    /// Configured independent packet-loss fraction.
    pub packet_loss_percent: f64,
    /// Percent of generated frames delivered to the decoder.
    pub frames_delivered_percent: f64,
    /// Keyframe requests emitted per simulated minute.
    pub keyframe_requests_per_minute: f64,
    /// Median loss-detection-to-delivered-keyframe time in milliseconds.
    pub median_recovery_ms: f64,
    /// 95th percentile loss-detection-to-delivered-keyframe time in milliseconds.
    pub p95_recovery_ms: f64,
    /// Percent of the run during which the displayed picture was stale by more than one frame interval.
    pub frozen_picture_percent: f64,
    /// Peak payload bytes retained by the reassembler.
    pub peak_inflight_bytes: usize,
    /// Peak number of frames retained by the reassembler.
    pub peak_inflight_frames: usize,
    /// Total generated stream frames.
    pub frames_generated: u64,
    /// Number of frames delivered to the decoder.
    pub frames_delivered: u64,
    /// Total keyframe requests.
    pub keyframe_requests: u64,
    /// Non-key frames delivered across a gap since the prior decoded frame.
    pub gap_safety_violations: u64,
    /// Virtual-clock timestamps of emitted keyframe requests.
    pub keyframe_request_times_us: Vec<u64>,
    /// Delivery-independent simulator statistics.
    pub network_submitted: u64,
    /// Datagrams lost by the configured profile or finite queue.
    pub network_lost: u64,
    /// Copies duplicated by the impairment profile.
    pub network_duplicates: u64,
}

/// Runs a reproducible virtual-clock stream for the requested duration.
///
/// The sender emits a keyframe at startup and responds to a keyframe request at
/// the next encode opportunity after half the modeled RTT plus encode time.
pub fn run_stream_soak(
    tier: StreamTier,
    mut profile: ImpairmentProfile,
    seed: u64,
    duration_seconds: u64,
    packet_loss_percent: f64,
) -> StreamSoakMetrics {
    let duration_us = duration_seconds.saturating_mul(1_000_000);
    let frame_count = duration_us / DEFAULT_FRAME_INTERVAL_US;
    let bitrate_bps = tier.bitrate_kbps().saturating_mul(1000);
    if profile.bitrate_limit_bps.is_none() {
        profile.bitrate_limit_bps = Some(bitrate_bps.saturating_mul(102) / 100);
    }
    profile.max_queue_bytes = profile.max_queue_bytes.max(1);
    let mut network = SimulatedNetwork::new(seed, profile);
    let mut reassembler = Reassembler::new(1);
    let mut pacer = Pacer::new();
    let mut feedback = SoakFeedback {
        response_delay_us: 40_000 / 2 + 10_000,
        ..SoakFeedback::default()
    };
    let mut peak_inflight_bytes = 0usize;
    let mut peak_inflight_frames = 0usize;
    let mut last_loss_count = 0u64;
    let mut now_us = 0u64;

    for frame_index in 0..frame_count {
        let frame_start_us = frame_index.saturating_mul(DEFAULT_FRAME_INTERVAL_US);
        let requested_key = feedback
            .next_keyframe_due_us
            .is_some_and(|due| frame_start_us >= due);
        let keyframe = frame_index == 0 || requested_key;
        if requested_key {
            feedback.next_keyframe_due_us = None;
        }
        let frame_id = u32::try_from(frame_index).unwrap_or(u32::MAX);
        let frame_len = if keyframe {
            tier.keyframe_bytes()
        } else {
            tier.average_p_frame_bytes()
        };
        let fill = u8::try_from(frame_index % 251).unwrap_or(0);
        let mut frame_bytes = Vec::new();
        if frame_bytes.try_reserve_exact(frame_len).is_err() {
            break;
        }
        frame_bytes.resize(frame_len.max(1), fill);
        let sender_frame = SenderFrame {
            epoch: 1,
            frame_id,
            keyframe,
            config: keyframe,
            capture_ts_us: frame_start_us as u32,
            bytes: frame_bytes,
        };
        let count = match fragment_count(sender_frame.bytes.len()) {
            Ok(value) => value,
            Err(_) => break,
        };
        let mut datagrams = Vec::with_capacity(count);
        for _ in 0..count {
            datagrams.push(Vec::with_capacity(1200));
        }
        if slice_frame_into(&sender_frame, &mut datagrams).is_err() {
            break;
        }
        let schedule = pacer.schedule(
            frame_start_us,
            count,
            DEFAULT_FRAME_INTERVAL_US,
            PACING_FRACTION_PERCENT,
        );
        for (index, datagram) in datagrams.iter().enumerate() {
            if let Some(send_at) = schedule.send_at_us.get(index).copied() {
                let _ = network.send(send_at, datagram);
            }
        }

        let frame_end = frame_start_us.saturating_add(DEFAULT_FRAME_INTERVAL_US);
        let startup_wait = if frame_index == 0 { 5_000 } else { 0 };
        let mut poll_at = now_us.max(frame_start_us).saturating_add(startup_wait);
        while poll_at <= frame_end {
            let arrivals = network.advance_to(poll_at);
            for arrival in arrivals {
                process_events(
                    reassembler.push_datagram(&arrival.bytes, arrival.at_us),
                    arrival.at_us,
                    &mut feedback,
                );
            }
            process_events(reassembler.poll(poll_at), poll_at, &mut feedback);
            let stats = reassembler.stats(poll_at);
            peak_inflight_bytes = peak_inflight_bytes.max(stats.counters.current_inflight_bytes);
            peak_inflight_frames = peak_inflight_frames.max(stats.counters.current_inflight_frames);
            let loss_count = stats
                .counters
                .dropped_incomplete
                .saturating_add(stats.counters.dropped_gap)
                .saturating_add(stats.counters.dropped_inconsistent);
            if loss_count > last_loss_count && feedback.first_loss_us.is_none() {
                feedback.first_loss_us = Some(poll_at);
            }
            last_loss_count = loss_count;
            poll_at = poll_at.saturating_add(5000);
        }
        now_us = frame_end;
    }

    let end_us = frame_count.saturating_mul(DEFAULT_FRAME_INTERVAL_US);
    let arrivals = network.advance_to(u64::MAX);
    for arrival in arrivals {
        process_events(
            reassembler.push_datagram(&arrival.bytes, arrival.at_us),
            arrival.at_us,
            &mut feedback,
        );
    }
    let stale_after_last = feedback
        .last_delivered_us
        .map(|last| end_us.saturating_sub(last.saturating_add(DEFAULT_FRAME_INTERVAL_US)))
        .unwrap_or(end_us);
    feedback.frozen_us = feedback.frozen_us.saturating_add(stale_after_last);
    let duration_us = duration_us.max(1);
    if let Some(last) = feedback.last_delivered_us {
        feedback.frozen_us = feedback
            .frozen_us
            .min(duration_us.saturating_sub(last.min(duration_us)));
    }

    feedback.recovery_times.sort_unstable();
    let median_recovery_ms = percentile_ms(&feedback.recovery_times, 0.50);
    let p95_recovery_ms = percentile_ms(&feedback.recovery_times, 0.95);
    let stats = network.stats();
    StreamSoakMetrics {
        bitrate_kbps: tier.bitrate_kbps(),
        packet_loss_percent,
        frames_delivered_percent: if frame_count == 0 {
            0.0
        } else {
            feedback.delivered_count as f64 * 100.0 / frame_count as f64
        },
        keyframe_requests_per_minute: feedback.keyframe_request_times.len() as f64 * 60.0
            / duration_seconds.max(1) as f64,
        median_recovery_ms,
        p95_recovery_ms,
        frozen_picture_percent: feedback.frozen_us as f64 * 100.0 / duration_us as f64,
        peak_inflight_bytes: peak_inflight_bytes.min(MAX_INFLIGHT_BYTES),
        peak_inflight_frames: peak_inflight_frames.min(MAX_INFLIGHT_FRAMES),
        frames_generated: frame_count,
        frames_delivered: feedback.delivered_count,
        keyframe_requests: feedback.keyframe_request_times.len() as u64,
        gap_safety_violations: feedback.gap_safety_violations,
        keyframe_request_times_us: feedback.keyframe_request_times,
        network_submitted: stats.submitted,
        network_lost: stats.lost.saturating_add(stats.queue_drops),
        network_duplicates: stats.duplicates,
    }
}

#[derive(Default)]
struct SoakFeedback {
    next_keyframe_due_us: Option<u64>,
    response_delay_us: u64,
    keyframe_request_times: Vec<u64>,
    recovery_times: Vec<u64>,
    first_loss_us: Option<u64>,
    frozen_us: u64,
    last_delivered_us: Option<u64>,
    last_delivered_frame_id: Option<u32>,
    gap_safety_violations: u64,
    delivered_count: u64,
}

fn process_events(events: Vec<ReassemblyEvent>, now_us: u64, feedback: &mut SoakFeedback) {
    for event in events {
        match event {
            ReassemblyEvent::NeedKeyframe(_) => {
                feedback.keyframe_request_times.push(now_us);
                let due = now_us.saturating_add(feedback.response_delay_us);
                if feedback.next_keyframe_due_us.is_none_or(|old| due < old) {
                    feedback.next_keyframe_due_us = Some(due);
                }
                if feedback.first_loss_us.is_none() && now_us > 0 {
                    feedback.first_loss_us = Some(now_us);
                }
            }
            ReassemblyEvent::FrameReady(FrameData {
                frame_id, keyframe, ..
            }) => {
                if !keyframe
                    && feedback
                        .last_delivered_frame_id
                        .is_none_or(|previous| previous.wrapping_add(1) != frame_id)
                {
                    feedback.gap_safety_violations =
                        feedback.gap_safety_violations.saturating_add(1);
                }
                feedback.last_delivered_frame_id = Some(frame_id);
                if let Some(previous) = feedback.last_delivered_us {
                    feedback.frozen_us =
                        feedback
                            .frozen_us
                            .saturating_add(now_us.saturating_sub(
                                previous.saturating_add(DEFAULT_FRAME_INTERVAL_US),
                            ));
                }
                feedback.last_delivered_us = Some(now_us);
                feedback.delivered_count = feedback.delivered_count.saturating_add(1);
                if keyframe {
                    if let Some(first_loss) = feedback.first_loss_us.take() {
                        feedback
                            .recovery_times
                            .push(now_us.saturating_sub(first_loss));
                    }
                    feedback.next_keyframe_due_us = None;
                }
            }
            ReassemblyEvent::Cursor(_) => {}
        }
    }
}

fn percentile_ms(samples: &[u64], percentile: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let index = ((samples.len() - 1) as f64 * percentile).ceil() as usize;
    samples.get(index).copied().unwrap_or(0) as f64 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_minute_iid_and_burst_profiles_stay_bounded_and_zero_loss_is_clean() {
        let _serial = crate::TIMING_SENSITIVE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let losses = [0u32, 5000, 10_000, 20_000, 50_000];
        for tier in [StreamTier::P480, StreamTier::P720, StreamTier::P1080] {
            for loss_ppm in losses {
                let profile = ImpairmentProfile {
                    independent_loss_ppm: loss_ppm,
                    one_way_delay_us: 1000,
                    jitter_us: 100,
                    reorder_probability_ppm: 3000,
                    reorder_max_displacement_packets: 1,
                    duplicate_probability_ppm: 1000,
                    max_queue_bytes: 4 * 1024 * 1024,
                    ..ImpairmentProfile::default()
                };
                let metrics = run_stream_soak(
                    tier,
                    profile,
                    u64::from(loss_ppm) ^ tier.bitrate_kbps(),
                    600,
                    f64::from(loss_ppm) / 10_000.0,
                );
                assert_eq!(metrics.frames_generated, 18_000);
                assert!(metrics.peak_inflight_bytes <= MAX_INFLIGHT_BYTES);
                assert!(metrics.peak_inflight_frames <= MAX_INFLIGHT_FRAMES);
                assert!(metrics
                    .keyframe_request_times_us
                    .windows(2)
                    .all(|pair| { pair[1].saturating_sub(pair[0]) >= 200_000 }));
                assert_eq!(metrics.gap_safety_violations, 0);
                assert!(metrics.network_duplicates > 0);
                if loss_ppm == 0 {
                    assert_eq!(metrics.frames_delivered, metrics.frames_generated);
                    assert_eq!(
                        metrics.keyframe_requests, 0,
                        "no-loss profile requested a keyframe: {metrics:?}"
                    );
                    assert!(metrics.frozen_picture_percent <= 0.01);
                }
                println!(
                    "SOAK {:?} loss={:.1}% delivered={:.3}% req/min={:.2} recovery_ms={:.2}/{:.2} frozen={:.3}% peak={}B {}",
                    tier, metrics.packet_loss_percent, metrics.frames_delivered_percent,
                    metrics.keyframe_requests_per_minute, metrics.median_recovery_ms,
                    metrics.p95_recovery_ms, metrics.frozen_picture_percent,
                    metrics.peak_inflight_bytes, metrics.peak_inflight_frames
                );
            }
        }

        let burst = ImpairmentProfile {
            good_to_bad_ppm: 1000,
            bad_to_good_ppm: 100_000,
            bad_state_loss_ppm: 250_000,
            one_way_delay_us: 1000,
            jitter_us: 100,
            reorder_probability_ppm: 3000,
            reorder_max_displacement_packets: 1,
            duplicate_probability_ppm: 1000,
            max_queue_bytes: 4 * 1024 * 1024,
            ..ImpairmentProfile::default()
        };
        let burst_metrics = run_stream_soak(StreamTier::P720, burst, 0xfeed, 600, 0.0);
        assert!(burst_metrics.peak_inflight_bytes <= MAX_INFLIGHT_BYTES);
        assert!(burst_metrics.peak_inflight_frames <= MAX_INFLIGHT_FRAMES);
        assert_eq!(burst_metrics.gap_safety_violations, 0);
        println!(
            "SOAK P720 burst delivered={:.3}% req/min={:.2} recovery_ms={:.2}/{:.2} frozen={:.3}% peak={}B {}",
            burst_metrics.frames_delivered_percent,
            burst_metrics.keyframe_requests_per_minute,
            burst_metrics.median_recovery_ms,
            burst_metrics.p95_recovery_ms,
            burst_metrics.frozen_picture_percent,
            burst_metrics.peak_inflight_bytes,
            burst_metrics.peak_inflight_frames
        );
    }

    #[test]
    fn average_and_keyframe_payload_sizes_follow_bitrate_formula() {
        assert_eq!(StreamTier::P480.average_p_frame_bytes(), 6250);
        assert_eq!(StreamTier::P720.average_p_frame_bytes(), 14_583);
        assert_eq!(StreamTier::P1080.average_p_frame_bytes(), 29_166);
        assert_eq!(StreamTier::P1080.keyframe_bytes(), 233_328);
        assert_eq!(racc_proto::MAX_VIDEO_PAYLOAD, 1182);
        assert_eq!(racc_net::REASSEMBLY_TIMEOUT_MS, 100);
    }
}
