use crate::{ImpairmentProfile, SimulatedNetwork};
use racc_net::{
    fragment_count, slice_frame_into, FrameData, Pacer, Reassembler, ReassemblyEvent, SenderFrame,
    DEFAULT_FRAME_INTERVAL_US, MAX_INFLIGHT_BYTES, MAX_INFLIGHT_FRAMES, PACING_FRACTION_PERCENT,
};
use racc_proto::{
    parse_video_datagram, VideoDatagram, VideoHeader, VIDEO_FLAG_CONFIG, VIDEO_FLAG_KEY,
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
    /// Median duration of a stale-picture freeze interval in milliseconds.
    pub median_freeze_ms: f64,
    /// 95th percentile stale-picture freeze duration in milliseconds.
    pub p95_freeze_ms: f64,
    /// Mean stale-picture freeze duration in milliseconds.
    pub mean_freeze_ms: f64,
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
    /// Shortest interval between retries while awaiting recovery, in milliseconds.
    pub minimum_retry_interval_ms: f64,
    /// Non-key frames delivered across a gap since the prior decoded frame.
    pub gap_safety_violations: u64,
    /// Virtual-clock timestamps of emitted keyframe requests.
    pub keyframe_request_times_us: Vec<u64>,
    /// Delivery-independent simulator statistics.
    pub network_submitted: u64,
    /// Datagrams lost by the configured independent or burst profile.
    pub network_profile_lost: u64,
    /// Datagrams discarded by the simulated byte queue.
    pub network_queue_drops: u64,
    /// Combined profile and queue loss count for compatibility with M2.
    pub network_lost: u64,
    /// Peak queued bytes in the simulated link.
    pub peak_simulated_queue_bytes: usize,
    /// Copies duplicated by the impairment profile.
    pub network_duplicates: u64,
    /// Mean serialization queue delay for scheduled datagrams, in milliseconds.
    pub mean_network_queue_delay_ms: f64,
    /// Maximum serialization queue delay observed, in milliseconds.
    pub max_network_queue_delay_ms: f64,
    /// Incomplete frames discarded by the reassembler.
    pub reassembly_dropped_incomplete: u64,
    /// Frames discarded while waiting for a keyframe.
    pub reassembly_dropped_waiting_key: u64,
    /// Frames evicted by a reassembler bound.
    pub reassembly_dropped_evicted: u64,
    /// Whole-frame gaps declared by the reassembler.
    pub reassembly_dropped_gap: u64,
    /// Frames discarded for inconsistent fragment metadata.
    pub reassembly_dropped_inconsistent: u64,
    /// Fully reassembled keyframes.
    pub keyframes_completed: u64,
    /// Keyframes delivered to the decoder.
    pub keyframes_delivered: u64,
    /// Partial keyframes discarded as incomplete.
    pub keyframes_dropped_incomplete: u64,
    /// Partial keyframes which reached the 100 ms idle timeout.
    pub keyframes_timed_out: u64,
    /// Older partial keyframes discarded after the reorder hold elapsed.
    pub keyframes_dropped_reorder: u64,
    /// Older partial keyframes discarded after a newer keyframe was accepted.
    pub keyframes_superseded: u64,
    /// Keyframes discarded due to a reassembly bound.
    pub keyframes_dropped_evicted: u64,
    /// Keyframes discarded for inconsistent metadata.
    pub keyframes_dropped_inconsistent: u64,
}

/// Optional parameters for deterministic stream audit scenarios.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamSoakOptions {
    /// Keyframe size multiplier relative to an average P-frame.
    pub keyframe_multiplier: usize,
    /// Override request one-way path delay for historical trace reproduction.
    /// `None` uses the profile's one-way delay.
    pub request_path_override_us: Option<u64>,
}

impl Default for StreamSoakOptions {
    fn default() -> Self {
        Self {
            keyframe_multiplier: 8,
            request_path_override_us: None,
        }
    }
}

/// Runs a reproducible virtual-clock stream for the requested duration.
///
/// The sender emits a keyframe at startup and responds to a keyframe request at
/// the next encode opportunity after half the modeled RTT plus encode time.
pub fn run_stream_soak(
    tier: StreamTier,
    profile: ImpairmentProfile,
    seed: u64,
    duration_seconds: u64,
    packet_loss_percent: f64,
) -> StreamSoakMetrics {
    run_stream_soak_internal(
        tier,
        profile,
        seed,
        duration_seconds,
        packet_loss_percent,
        None,
        StreamSoakOptions::default(),
    )
    .0
}

/// Runs a deterministic stream soak and records a bounded event timeline.
///
/// Tracing is opt-in. It records up to max_loss_events lost datagrams and continues
/// through the following delivered keyframe so recovery can be audited.
pub fn run_stream_soak_with_trace(
    tier: StreamTier,
    profile: ImpairmentProfile,
    seed: u64,
    duration_seconds: u64,
    packet_loss_percent: f64,
    max_loss_events: usize,
) -> (StreamSoakMetrics, Vec<String>) {
    run_stream_soak_with_trace_and_options(
        tier,
        profile,
        seed,
        duration_seconds,
        packet_loss_percent,
        max_loss_events,
        StreamSoakOptions::default(),
    )
}

/// Runs a traced soak with explicit keyframe and feedback-path simulation options.
pub fn run_stream_soak_with_trace_and_options(
    tier: StreamTier,
    profile: ImpairmentProfile,
    seed: u64,
    duration_seconds: u64,
    packet_loss_percent: f64,
    max_loss_events: usize,
    options: StreamSoakOptions,
) -> (StreamSoakMetrics, Vec<String>) {
    let (metrics, trace) = run_stream_soak_internal(
        tier,
        profile,
        seed,
        duration_seconds,
        packet_loss_percent,
        Some(max_loss_events),
        options,
    );
    (metrics, trace)
}

/// Runs a soak while selecting the keyframe multiplier. The link remains the
/// corrected unconstrained default unless the profile sets an explicit rate.
pub fn run_stream_soak_with_options(
    tier: StreamTier,
    profile: ImpairmentProfile,
    seed: u64,
    duration_seconds: u64,
    packet_loss_percent: f64,
    options: StreamSoakOptions,
) -> StreamSoakMetrics {
    run_stream_soak_internal(
        tier,
        profile,
        seed,
        duration_seconds,
        packet_loss_percent,
        None,
        options,
    )
    .0
}

fn run_stream_soak_internal(
    tier: StreamTier,
    mut profile: ImpairmentProfile,
    seed: u64,
    duration_seconds: u64,
    packet_loss_percent: f64,
    trace_loss_events: Option<usize>,
    options: StreamSoakOptions,
) -> (StreamSoakMetrics, Vec<String>) {
    let duration_us = duration_seconds.saturating_mul(1_000_000);
    let frame_count = duration_us / DEFAULT_FRAME_INTERVAL_US;
    let bitrate_bps = tier.bitrate_kbps().saturating_mul(1000);
    apply_default_link(&mut profile, bitrate_bps);
    profile.max_queue_bytes = profile.max_queue_bytes.max(1);
    let mut network = SimulatedNetwork::new(seed, profile);
    let mut reassembler = Reassembler::new(1);
    let mut pacer = Pacer::new();
    let mut feedback = SoakFeedback {
        request_path_us: options
            .request_path_override_us
            .unwrap_or(profile.one_way_delay_us),
        keyframe_multiplier: options.keyframe_multiplier.max(1),
        ..SoakFeedback::default()
    };
    let mut trace = trace_loss_events
        .filter(|max_loss_events| *max_loss_events > 0)
        .map(TraceRecorder::new);
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
            tier.average_p_frame_bytes()
                .saturating_mul(feedback.keyframe_multiplier)
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
        if keyframe {
            if let Some(trace) = trace.as_mut() {
                trace.record(
                    frame_start_us,
                    format!(
                        "keyframe_sent frame_id={frame_id} bytes={} fragments={count} KEY=1 CONFIG=1",
                        sender_frame.bytes.len()
                    ),
                );
            }
        }
        for (index, datagram) in datagrams.iter().enumerate() {
            if let Some(send_at) = schedule.send_at_us.get(index).copied() {
                let network_before = network.stats();
                let accepted = network.send(send_at, datagram);
                if !accepted {
                    if let Some(trace) = trace.as_mut() {
                        let network_after = network.stats();
                        let cause = if network_after.lost > network_before.lost {
                            "iid_or_burst_loss"
                        } else {
                            "queue_drop"
                        };
                        if let Ok(VideoDatagram::Video { header, .. }) =
                            parse_video_datagram(datagram)
                        {
                            trace.packet_lost(send_at, header, cause);
                        }
                    }
                }
            }
        }

        let frame_end = frame_start_us.saturating_add(DEFAULT_FRAME_INTERVAL_US);
        let startup_wait = if frame_index == 0 { 5_000 } else { 0 };
        let mut poll_at = now_us.max(frame_start_us).saturating_add(startup_wait);
        while poll_at <= frame_end {
            let arrivals = network.advance_to(poll_at);
            for arrival in arrivals {
                let before = reassembler.stats(arrival.at_us);
                let incoming = parse_video_datagram(&arrival.bytes).ok();
                if let Some(VideoDatagram::Video { header, .. }) = incoming.as_ref() {
                    if header.flags & VIDEO_FLAG_KEY != 0 {
                        if let Some(trace) = trace.as_mut() {
                            trace.record(
                                arrival.at_us,
                                format!(
                                    "keyframe_fragment_received frame_id={} fragment={}/{}",
                                    header.frame_id,
                                    header.frag_idx + 1,
                                    header.frag_cnt
                                ),
                            );
                        }
                    }
                }
                let events = reassembler.push_datagram(&arrival.bytes, arrival.at_us);
                let after = reassembler.stats(arrival.at_us);
                peak_inflight_bytes =
                    peak_inflight_bytes.max(after.counters.current_inflight_bytes);
                peak_inflight_frames =
                    peak_inflight_frames.max(after.counters.current_inflight_frames);
                trace_reassembly_changes(
                    trace.as_mut(),
                    arrival.at_us,
                    incoming.as_ref(),
                    before,
                    after,
                );
                process_events(events, arrival.at_us, &mut feedback, trace.as_mut());
            }
            let before_poll = reassembler.stats(poll_at);
            let events = reassembler.poll(poll_at);
            let after_poll = reassembler.stats(poll_at);
            trace_reassembly_changes(trace.as_mut(), poll_at, None, before_poll, after_poll);
            process_events(events, poll_at, &mut feedback, trace.as_mut());
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
        let before = reassembler.stats(arrival.at_us);
        let incoming = parse_video_datagram(&arrival.bytes).ok();
        if let Some(VideoDatagram::Video { header, .. }) = incoming.as_ref() {
            if header.flags & VIDEO_FLAG_KEY != 0 {
                if let Some(trace) = trace.as_mut() {
                    trace.record(
                        arrival.at_us,
                        format!(
                            "keyframe_fragment_received frame_id={} fragment={}/{}",
                            header.frame_id,
                            header.frag_idx + 1,
                            header.frag_cnt
                        ),
                    );
                }
            }
        }
        let events = reassembler.push_datagram(&arrival.bytes, arrival.at_us);
        let after = reassembler.stats(arrival.at_us);
        trace_reassembly_changes(
            trace.as_mut(),
            arrival.at_us,
            incoming.as_ref(),
            before,
            after,
        );
        process_events(events, arrival.at_us, &mut feedback, trace.as_mut());
    }
    let stale_after_last = stale_tail_us(feedback.last_delivered_us, end_us);
    feedback.frozen_us = finalize_stale_us(feedback.frozen_us, feedback.last_delivered_us, end_us);
    if stale_after_last > 0 {
        feedback.freeze_durations_us.push(stale_after_last);
    }
    let duration_us = duration_us.max(1);

    feedback.recovery_times.sort_unstable();
    let median_recovery_ms = percentile_ms(&feedback.recovery_times, 0.50);
    let p95_recovery_ms = percentile_ms(&feedback.recovery_times, 0.95);
    feedback.freeze_durations_us.sort_unstable();
    let median_freeze_ms = percentile_ms(&feedback.freeze_durations_us, 0.50);
    let p95_freeze_ms = percentile_ms(&feedback.freeze_durations_us, 0.95);
    let mean_freeze_ms = if feedback.freeze_durations_us.is_empty() {
        0.0
    } else {
        feedback.freeze_durations_us.iter().copied().sum::<u64>() as f64
            / feedback.freeze_durations_us.len() as f64
            / 1000.0
    };
    let stats = network.stats();
    let reassembly_counters = reassembler.stats(end_us).counters;
    let metrics = StreamSoakMetrics {
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
        median_freeze_ms,
        p95_freeze_ms,
        mean_freeze_ms,
        frozen_picture_percent: feedback.frozen_us as f64 * 100.0 / duration_us as f64,
        peak_inflight_bytes: peak_inflight_bytes.min(MAX_INFLIGHT_BYTES),
        peak_inflight_frames: peak_inflight_frames.min(MAX_INFLIGHT_FRAMES),
        frames_generated: frame_count,
        frames_delivered: feedback.delivered_count,
        keyframe_requests: feedback.keyframe_request_times.len() as u64,
        minimum_retry_interval_ms: feedback
            .retry_intervals_us
            .iter()
            .copied()
            .min()
            .map_or(0.0, |interval| interval as f64 / 1000.0),
        gap_safety_violations: feedback.gap_safety_violations,
        keyframe_request_times_us: feedback.keyframe_request_times,
        network_submitted: stats.submitted,
        network_profile_lost: stats.lost,
        network_queue_drops: stats.queue_drops,
        network_lost: stats.lost.saturating_add(stats.queue_drops),
        peak_simulated_queue_bytes: stats.peak_queue_bytes,
        network_duplicates: stats.duplicates,
        mean_network_queue_delay_ms: if stats.scheduled == 0 {
            0.0
        } else {
            stats.total_queue_delay_us as f64 / stats.scheduled as f64 / 1000.0
        },
        max_network_queue_delay_ms: stats.max_queue_delay_us as f64 / 1000.0,
        reassembly_dropped_incomplete: reassembly_counters.dropped_incomplete,
        reassembly_dropped_waiting_key: reassembly_counters.dropped_waiting_key,
        reassembly_dropped_evicted: reassembly_counters.dropped_evicted,
        reassembly_dropped_gap: reassembly_counters.dropped_gap,
        reassembly_dropped_inconsistent: reassembly_counters.dropped_inconsistent,
        keyframes_completed: reassembly_counters.keyframes_completed,
        keyframes_delivered: reassembly_counters.keyframes_delivered,
        keyframes_dropped_incomplete: reassembly_counters.keyframes_dropped_incomplete,
        keyframes_timed_out: reassembly_counters.keyframes_timed_out,
        keyframes_dropped_reorder: reassembly_counters.keyframes_dropped_reorder,
        keyframes_superseded: reassembly_counters.keyframes_superseded,
        keyframes_dropped_evicted: reassembly_counters.keyframes_dropped_evicted,
        keyframes_dropped_inconsistent: reassembly_counters.keyframes_dropped_inconsistent,
    };
    (
        metrics,
        trace.map_or_else(Vec::new, TraceRecorder::into_lines),
    )
}

#[derive(Default)]
struct SoakFeedback {
    next_keyframe_due_us: Option<u64>,
    request_path_us: u64,
    keyframe_multiplier: usize,
    freeze_durations_us: Vec<u64>,
    retry_intervals_us: Vec<u64>,
    last_request_since_keyframe_us: Option<u64>,
    request_step_since_keyframe: u8,
    keyframe_request_times: Vec<u64>,
    recovery_times: Vec<u64>,
    first_loss_us: Option<u64>,
    frozen_us: u64,
    last_delivered_us: Option<u64>,
    last_delivered_frame_id: Option<u32>,
    gap_safety_violations: u64,
    delivered_count: u64,
}

fn process_events(
    events: Vec<ReassemblyEvent>,
    now_us: u64,
    feedback: &mut SoakFeedback,
    mut trace: Option<&mut TraceRecorder>,
) {
    for event in events {
        match event {
            ReassemblyEvent::NeedKeyframe(_) => {
                let request_step = feedback.request_step_since_keyframe.saturating_add(1);
                let backoff_ms = feedback
                    .last_request_since_keyframe_us
                    .map(|last| now_us.saturating_sub(last) / 1000)
                    .unwrap_or(0);
                if let Some(trace) = trace.as_deref_mut() {
                    trace.record(
                        now_us,
                        format!(
                            "need_keyframe=true request_emitted backoff_step={request_step} elapsed_since_prior_request_ms={backoff_ms}"
                        ),
                    );
                    trace.record(
                        now_us.saturating_add(feedback.request_path_us),
                        "request_arrived_at_simulated_sender".to_string(),
                    );
                }
                if let Some(previous) = feedback.last_request_since_keyframe_us {
                    feedback
                        .retry_intervals_us
                        .push(now_us.saturating_sub(previous));
                }
                feedback.last_request_since_keyframe_us = Some(now_us);
                feedback.request_step_since_keyframe = request_step;
                feedback.keyframe_request_times.push(now_us);
                let encode_ready = now_us
                    .saturating_add(feedback.request_path_us)
                    .saturating_add(10_000);
                let due = encode_ready.saturating_add(DEFAULT_FRAME_INTERVAL_US - 1)
                    / DEFAULT_FRAME_INTERVAL_US
                    * DEFAULT_FRAME_INTERVAL_US;
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
                if keyframe {
                    if let Some(trace) = trace.as_deref_mut() {
                        trace.record(now_us, format!("keyframe_delivered frame_id={frame_id}"));
                        trace.finish_after_recovery();
                    }
                }
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
                    let stale_gap =
                        now_us.saturating_sub(previous.saturating_add(DEFAULT_FRAME_INTERVAL_US));
                    feedback.frozen_us = feedback.frozen_us.saturating_add(stale_gap);
                    if stale_gap > 0 {
                        feedback.freeze_durations_us.push(stale_gap);
                    }
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
                    feedback.last_request_since_keyframe_us = None;
                    feedback.request_step_since_keyframe = 0;
                }
            }
            ReassemblyEvent::Cursor(_) => {}
        }
    }
}

fn trace_reassembly_changes(
    trace: Option<&mut TraceRecorder>,
    now_us: u64,
    incoming: Option<&VideoDatagram<'_>>,
    before: racc_net::ReassemblyStats,
    after: racc_net::ReassemblyStats,
) {
    let Some(trace) = trace else {
        return;
    };
    let counters_before = before.counters;
    let counters_after = after.counters;
    let incomplete = counters_after
        .dropped_incomplete
        .saturating_sub(counters_before.dropped_incomplete);
    let gaps = counters_after
        .dropped_gap
        .saturating_sub(counters_before.dropped_gap);
    let evicted = counters_after
        .dropped_evicted
        .saturating_sub(counters_before.dropped_evicted);
    let inconsistent = counters_after
        .dropped_inconsistent
        .saturating_sub(counters_before.dropped_inconsistent);
    let waiting = counters_after
        .dropped_waiting_key
        .saturating_sub(counters_before.dropped_waiting_key);
    let frame_id = match incoming {
        Some(VideoDatagram::Video { header, .. }) => Some(header.frame_id),
        _ => None,
    };
    if incomplete + gaps + evicted + inconsistent > 0 {
        trace.record(
            now_us,
            format!(
                "loss_declared triggering_frame_id={frame_id:?} incomplete={incomplete} gap={gaps} evicted={evicted} inconsistent={inconsistent}"
            ),
        );
    }
    if waiting > 0 {
        trace.record(
            now_us,
            format!(
                "frame_not_delivered frame_id={frame_id:?} reason=waiting_for_keyframe count={waiting}"
            ),
        );
    }
    if counters_after.frames_completed > counters_before.frames_completed {
        if let Some(VideoDatagram::Video { header, .. }) = incoming {
            if header.flags & VIDEO_FLAG_KEY != 0 {
                trace.record(
                    now_us,
                    format!(
                        "keyframe_completed frame_id={} fragments={}",
                        header.frame_id, header.frag_cnt
                    ),
                );
            }
        }
    }
    if before.need_keyframe && !after.need_keyframe {
        trace.record(
            now_us,
            "need_keyframe_cleared_after_keyframe_delivery".to_string(),
        );
    } else if !before.need_keyframe && after.need_keyframe {
        trace.record(now_us, "need_keyframe_set_after_loss".to_string());
    }
}

#[derive(Debug)]
struct TraceRecorder {
    entries: Vec<(u64, u64, String)>,
    next_sequence: u64,
    max_loss_events: usize,
    loss_events: usize,
    stopped: bool,
    tail_deadline_us: Option<u64>,
}

impl TraceRecorder {
    fn new(max_loss_events: usize) -> Self {
        Self {
            entries: Vec::new(),
            next_sequence: 0,
            max_loss_events,
            loss_events: 0,
            stopped: false,
            tail_deadline_us: None,
        }
    }

    fn record(&mut self, at_us: u64, event: String) {
        if self.stopped {
            return;
        }
        if self
            .tail_deadline_us
            .is_some_and(|deadline| at_us > deadline)
        {
            self.push(
                at_us,
                "trace_stopped reason=no_recovery_within_5000ms_of_target_loss".to_string(),
            );
            self.stopped = true;
            return;
        }
        self.push(at_us, event);
    }

    fn push(&mut self, at_us: u64, event: String) {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.entries.push((at_us, sequence, event));
    }

    fn into_lines(mut self) -> Vec<String> {
        self.entries
            .sort_by_key(|(at_us, sequence, _)| (*at_us, *sequence));
        self.entries
            .into_iter()
            .map(|(at_us, _, event)| format!("t={:.3}ms {event}", at_us as f64 / 1000.0))
            .collect()
    }

    fn packet_lost(&mut self, at_us: u64, header: VideoHeader, cause: &str) {
        if self.loss_events >= self.max_loss_events {
            return;
        }
        self.loss_events += 1;
        if self.loss_events == self.max_loss_events {
            self.tail_deadline_us = Some(at_us.saturating_add(5_000_000));
        }
        let prefix = if self.loss_events == 1 {
            "first_lost_fragment"
        } else {
            "fragment_lost"
        };
        self.record(
            at_us,
            format!(
                "{prefix} loss_event={} frame_id={} fragment={}/{} KEY={} CONFIG={} cause={cause}",
                self.loss_events,
                header.frame_id,
                header.frag_idx + 1,
                header.frag_cnt,
                u8::from(header.flags & VIDEO_FLAG_KEY != 0),
                u8::from(header.flags & VIDEO_FLAG_CONFIG != 0)
            ),
        );
    }

    fn finish_after_recovery(&mut self) {
        if self.loss_events >= self.max_loss_events {
            self.stopped = true;
        }
    }
}

fn apply_default_link(profile: &mut ImpairmentProfile, bitrate_bps: u64) {
    if profile.bitrate_limit_bps.is_none() {
        profile.bitrate_limit_bps = Some(bitrate_bps.saturating_mul(10).max(50_000_000));
        profile.max_queue_bytes = profile.max_queue_bytes.max(64 * 1024 * 1024);
    }
}

fn finalize_stale_us(accumulated_us: u64, last_delivery_us: Option<u64>, end_us: u64) -> u64 {
    accumulated_us.saturating_add(stale_tail_us(last_delivery_us, end_us))
}

fn stale_tail_us(last_delivery_us: Option<u64>, end_us: u64) -> u64 {
    last_delivery_us
        .map(|last| end_us.saturating_sub(last.saturating_add(DEFAULT_FRAME_INTERVAL_US)))
        .unwrap_or(end_us)
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
                assert!(
                    metrics.minimum_retry_interval_ms == 0.0
                        || metrics.minimum_retry_interval_ms >= 200.0,
                    "unanswered keyframe retry was too early for {tier:?} loss={loss_ppm}: {} ms",
                    metrics.minimum_retry_interval_ms
                );
                assert_eq!(metrics.gap_safety_violations, 0);
                assert!(metrics.network_duplicates > 0);
                if loss_ppm == 0 {
                    assert_eq!(metrics.frames_delivered, metrics.frames_generated);
                    assert_eq!(
                        metrics.keyframe_requests, 0,
                        "no-loss profile requested a keyframe: {metrics:?}"
                    );
                    // +/-100 us packet jitter can contribute at most about
                    // 200 us per frame interval, or 0.6% over 600 virtual seconds.
                    assert!(metrics.frozen_picture_percent <= 0.61);
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
    fn trace_records_first_twenty_losses_through_keyframe_recovery() {
        let _serial = crate::TIMING_SENSITIVE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let profile = ImpairmentProfile {
            independent_loss_ppm: 5000,
            one_way_delay_us: 1000,
            jitter_us: 100,
            reorder_probability_ppm: 3000,
            reorder_max_displacement_packets: 1,
            duplicate_probability_ppm: 1000,
            bitrate_limit_bps: Some(1_530_000),
            max_queue_bytes: 4 * 1024 * 1024,
            ..ImpairmentProfile::default()
        };
        let seed = 5000_u64 ^ StreamTier::P480.bitrate_kbps();
        let (metrics, trace) = run_stream_soak_with_trace_and_options(
            StreamTier::P480,
            profile,
            seed,
            600,
            0.5,
            20,
            StreamSoakOptions {
                keyframe_multiplier: 8,
                request_path_override_us: Some(20_000),
            },
        );
        let joined = trace.join("\\n");

        assert!(joined.contains("first_lost_fragment loss_event=1"));
        assert!(joined.contains("loss_event=20"));
        assert!(joined.contains("request_emitted backoff_step=1"));
        assert!(joined.contains("request_arrived_at_simulated_sender"));
        assert!(joined.contains("keyframe_sent"));
        assert!(joined.contains("keyframe_fragment_received"));
        assert!(joined.contains("keyframe_completed") || joined.contains("keyframe_delivered"));
        assert!(
            joined.contains("keyframe_delivered")
                || joined.contains("trace_stopped reason=no_recovery_within_5000ms_of_target_loss")
        );
        assert!(trace.len() < 20_000);
        println!(
            "TRACE seed={seed} bitrate={} loss=0.5% losses=20 delivered={:.3}% requests={} stale={:.3}% reassembly_peak={}B profile_loss={} queue_drops={} link_queue_peak={}B incomplete={} waiting={} evicted={} gap={} inconsistent={} keyframes={}/{},keyframe_drop_inc/timeout/reorder/superseded/evict/incon={}/{}/{}/{}/{}/{}",
            StreamTier::P480.bitrate_kbps(),
            metrics.frames_delivered_percent,
            metrics.keyframe_requests,
            metrics.frozen_picture_percent,
            metrics.peak_inflight_bytes,
            metrics.network_profile_lost,
            metrics.network_queue_drops,
            metrics.peak_simulated_queue_bytes,
            metrics.reassembly_dropped_incomplete,
            metrics.reassembly_dropped_waiting_key,
            metrics.reassembly_dropped_evicted,
            metrics.reassembly_dropped_gap,
            metrics.reassembly_dropped_inconsistent,
            metrics.keyframes_delivered,
            metrics.keyframes_completed,
            metrics.keyframes_dropped_incomplete,
            metrics.keyframes_timed_out,
            metrics.keyframes_dropped_reorder,
            metrics.keyframes_superseded,
            metrics.keyframes_dropped_evicted,
            metrics.keyframes_dropped_inconsistent
        );
        for event in trace {
            println!("{event}");
        }
    }

    #[test]
    fn corrected_default_link_has_capacity_margin_and_no_sustained_tail_drops() {
        let _serial = crate::TIMING_SENSITIVE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tier = StreamTier::P480;
        let bitrate_bps = tier.bitrate_kbps() * 1000;
        let mut default_profile = ImpairmentProfile::default();
        apply_default_link(&mut default_profile, bitrate_bps);
        assert_eq!(
            default_profile.bitrate_limit_bps,
            Some((bitrate_bps * 10).max(50_000_000))
        );
        assert!(default_profile.max_queue_bytes >= 64 * 1024 * 1024);

        let profile = ImpairmentProfile {
            independent_loss_ppm: 5_000,
            one_way_delay_us: 1_000,
            ..ImpairmentProfile::default()
        };
        let metrics = run_stream_soak(tier, profile, 13_137, 600, 0.5);
        assert_eq!(metrics.network_queue_drops, 0);
        assert!(metrics.peak_simulated_queue_bytes < 1_000_000);
    }

    #[test]
    fn corrected_default_matches_low_loss_analytic_sanity_envelope() {
        let _serial = crate::TIMING_SENSITIVE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for tier in [StreamTier::P480, StreamTier::P720] {
            for loss_ppm in [1_000, 2_500, 5_000] {
                let profile = ImpairmentProfile {
                    independent_loss_ppm: loss_ppm,
                    one_way_delay_us: 1_000,
                    ..ImpairmentProfile::default()
                };
                let metrics = run_stream_soak(
                    tier,
                    profile,
                    0x2505 ^ tier.bitrate_kbps() ^ u64::from(loss_ppm),
                    600,
                    f64::from(loss_ppm) / 10_000.0,
                );
                let (predicted_delivery, predicted_stale) =
                    analytic_sanity_prediction(tier, loss_ppm);
                assert!(
                    within_factor_of_two(metrics.frames_delivered_percent, predicted_delivery),
                    "delivery envelope {tier:?} {loss_ppm}: measured={} predicted={predicted_delivery}",
                    metrics.frames_delivered_percent,
                );
                assert!(
                    within_factor_of_two(metrics.frozen_picture_percent, predicted_stale),
                    "stale envelope {tier:?} {loss_ppm}: measured={} predicted={predicted_stale}; requests/min={} freeze mean/p95={}/{}ms",
                    metrics.frozen_picture_percent,
                    metrics.keyframe_requests_per_minute,
                    metrics.mean_freeze_ms,
                    metrics.p95_freeze_ms,
                );
            }
        }
    }

    fn analytic_sanity_prediction(tier: StreamTier, loss_ppm: u32) -> (f64, f64) {
        let loss = f64::from(loss_ppm) / 1_000_000.0;
        let p_bytes = tier.average_p_frame_bytes();
        let key_bytes = tier.keyframe_bytes();
        let p_fragments = p_bytes.div_ceil(racc_proto::MAX_VIDEO_PAYLOAD);
        let key_fragments = key_bytes.div_ceil(racc_proto::MAX_VIDEO_PAYLOAD);
        let p_success = (1.0 - loss).powi(p_fragments as i32);
        let key_success = (1.0 - loss).powi(key_fragments as i32);
        let lambda = 30.0 * (1.0 - p_success);
        let q = 1.0 - key_success;
        let retry_ms =
            200.0 * q + 400.0 * q.powi(2) + 800.0 * q.powi(3) + 1_000.0 * q.powi(4) / key_success;
        let recovery_ms = 8.0 + 47.667 / key_success + retry_ms;
        // Include the current displayed frame's age at loss declaration; the
        // event-level recovery timer starts only after that frame was emitted.
        let total_freeze_ms = 33.333 + recovery_ms;
        let delivery = 1.0 / (1.0 + lambda * total_freeze_ms / 1000.0);
        let stale = delivery * lambda * recovery_ms / 1000.0;
        (delivery * 100.0, stale * 100.0)
    }

    fn within_factor_of_two(measured: f64, predicted: f64) -> bool {
        measured > 0.0
            && predicted > 0.0
            && measured.max(predicted) / measured.min(predicted) <= 2.0
    }

    #[test]
    fn final_tail_staleness_does_not_truncate_freezes_accumulated_earlier() {
        let earlier_freeze_us: u64 = 200_000;
        let end_us: u64 = 1_000_000;
        let last_delivery_us: u64 = 500_000;
        let tail =
            end_us.saturating_sub(last_delivery_us.saturating_add(DEFAULT_FRAME_INTERVAL_US));
        let total = finalize_stale_us(earlier_freeze_us, Some(last_delivery_us), end_us);
        assert_eq!(tail, 466_667);
        assert_eq!(total, 666_667);
        assert!(total > end_us.saturating_sub(last_delivery_us));
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
