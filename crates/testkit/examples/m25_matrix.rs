use racc_testkit::sim::recovery_models::{run_recovery_model, RecoveryMechanism, RecoveryScenario};
use racc_testkit::{
    run_stream_soak, run_stream_soak_with_options, ImpairmentProfile, StreamSoakOptions, StreamTier,
};

fn main() {
    let tiers = [StreamTier::P480, StreamTier::P720, StreamTier::P1080];
    let losses = [0, 1_000, 2_500, 5_000, 10_000, 20_000, 50_000];
    println!("P4|tier|case|loss_ppm|key_multiplier|seed|delivered_pct|requests_min|freeze_med_ms|freeze_p95_ms|freeze_mean_ms|stale_pct|peak_reassembly_bytes|queue_mean_ms|queue_max_ms|queue_drops|profile_lost");
    for tier in tiers {
        for loss in losses {
            let seed = 0x2505_u64 ^ tier.bitrate_kbps() ^ u64::from(loss);
            let profile = ImpairmentProfile {
                independent_loss_ppm: loss,
                one_way_delay_us: 1_000,
                jitter_us: 100,
                reorder_probability_ppm: 30_000,
                reorder_max_displacement_packets: 1,
                duplicate_probability_ppm: 1_000,
                ..ImpairmentProfile::default()
            };
            let m = run_stream_soak(tier, profile, seed, 600, f64::from(loss) / 10_000.0);
            print_soak(tier, "unconstrained", loss, 8, seed, &m);
        }
    }
    let burst_seed = 0xfeed_u64;
    let burst = ImpairmentProfile {
        good_to_bad_ppm: 1_000,
        bad_to_good_ppm: 100_000,
        bad_state_loss_ppm: 250_000,
        one_way_delay_us: 1_000,
        jitter_us: 100,
        reorder_probability_ppm: 30_000,
        reorder_max_displacement_packets: 1,
        duplicate_probability_ppm: 1_000,
        ..ImpairmentProfile::default()
    };
    let m = run_stream_soak(StreamTier::P720, burst, burst_seed, 600, 0.0);
    print_soak(StreamTier::P720, "burst_GE", 0, 8, burst_seed, &m);

    for tier in [StreamTier::P720, StreamTier::P1080] {
        for loss in [5_000, 10_000, 20_000] {
            let seed = 0x2505_u64 ^ tier.bitrate_kbps() ^ u64::from(loss) ^ 4;
            let profile = ImpairmentProfile {
                independent_loss_ppm: loss,
                one_way_delay_us: 1_000,
                jitter_us: 100,
                reorder_probability_ppm: 30_000,
                reorder_max_displacement_packets: 1,
                duplicate_probability_ppm: 1_000,
                ..ImpairmentProfile::default()
            };
            let m = run_stream_soak_with_options(
                tier,
                profile,
                seed,
                600,
                f64::from(loss) / 10_000.0,
                StreamSoakOptions {
                    keyframe_multiplier: 4,
                    request_path_override_us: None,
                },
            );
            print_soak(tier, "unconstrained", loss, 4, seed, &m);
        }
    }

    for (label, rate, queue) in [
        ("constrained_2x_100KB", 7_000_000_u64, 100_000_usize),
        ("constrained_5Mbps_150KB", 5_000_000_u64, 150_000_usize),
    ] {
        for loss in [5_000, 20_000] {
            let seed = 0x2505_u64 ^ 3_500 ^ u64::from(loss) ^ rate;
            let profile = ImpairmentProfile {
                independent_loss_ppm: loss,
                one_way_delay_us: 1_000,
                jitter_us: 100,
                reorder_probability_ppm: 30_000,
                reorder_max_displacement_packets: 1,
                duplicate_probability_ppm: 1_000,
                bitrate_limit_bps: Some(rate),
                max_queue_bytes: queue,
                ..ImpairmentProfile::default()
            };
            let m = run_stream_soak(
                StreamTier::P720,
                profile,
                seed,
                600,
                f64::from(loss) / 10_000.0,
            );
            print_soak(StreamTier::P720, label, loss, 8, seed, &m);
        }
    }

    println!("P5|tier|loss_label|delay_ms|mechanism|seed|packet_loss_observed_pct|delivered_pct|stale_pct|freeze_med_ms|freeze_p95_ms|requests_min|bandwidth_overhead_pct|added_median_latency_ms");
    for (tier, losses) in [
        (StreamTier::P720, vec![5_000, 10_000, 20_000, 50_000]),
        (StreamTier::P1080, vec![10_000, 20_000]),
    ] {
        for loss in losses {
            for delay_ms in [1_u64, 20] {
                for mechanism in RecoveryMechanism::ALL {
                    let seed = 0x25f5_u64 ^ tier.bitrate_kbps() ^ u64::from(loss) ^ delay_ms;
                    let m = run_recovery_model(
                        mechanism,
                        RecoveryScenario {
                            tier,
                            loss_ppm: loss,
                            burst_loss: false,
                            one_way_delay_us: delay_ms * 1000,
                            duration_seconds: 600,
                            seed,
                            keyframe_multiplier: 8,
                        },
                    );
                    println!("P5|{tier:?}|{:.2}%|{delay_ms}|{mechanism}|{seed}|{:.4}|{:.3}|{:.3}|{:.3}|{:.3}|{:.2}|{:.3}|{:.3}",
                        f64::from(loss) / 10_000.0, m.packet_loss_observed_percent, m.delivered_percent,
                        m.stale_percent, m.median_freeze_ms, m.p95_freeze_ms,
                        m.keyframe_requests_per_minute, m.bandwidth_overhead_percent, m.added_median_latency_ms);
                }
            }
        }
    }
    for delay_ms in [1_u64, 20] {
        for mechanism in RecoveryMechanism::ALL {
            let seed = 0x25f5_u64 ^ 7_200 ^ delay_ms ^ 0xfeed;
            let m = run_recovery_model(
                mechanism,
                RecoveryScenario {
                    tier: StreamTier::P720,
                    loss_ppm: 0,
                    burst_loss: true,
                    one_way_delay_us: delay_ms * 1000,
                    duration_seconds: 600,
                    seed,
                    keyframe_multiplier: 8,
                },
            );
            println!("P5|P720|burst_GE({:.3}% actual)|{delay_ms}|{mechanism}|{seed}|{:.4}|{:.3}|{:.3}|{:.3}|{:.3}|{:.2}|{:.3}|{:.3}",
                m.packet_loss_observed_percent, m.packet_loss_observed_percent, m.delivered_percent,
                m.stale_percent, m.median_freeze_ms, m.p95_freeze_ms,
                m.keyframe_requests_per_minute, m.bandwidth_overhead_percent, m.added_median_latency_ms);
        }
    }
}

fn print_soak(
    tier: StreamTier,
    case: &str,
    loss: u32,
    multiplier: usize,
    seed: u64,
    m: &racc_testkit::StreamSoakMetrics,
) {
    println!("P4|{tier:?}|{case}|{loss}|{multiplier}|{seed}|{:.3}|{:.2}|{:.3}|{:.3}|{:.3}|{:.4}|{}|{:.4}|{:.4}|{}|{}",
        m.frames_delivered_percent, m.keyframe_requests_per_minute, m.median_freeze_ms,
        m.p95_freeze_ms, m.mean_freeze_ms, m.frozen_picture_percent, m.peak_inflight_bytes,
        m.mean_network_queue_delay_ms, m.max_network_queue_delay_ms, m.network_queue_drops,
        m.network_profile_lost);
}
