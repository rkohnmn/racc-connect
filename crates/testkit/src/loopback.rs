use crate::{ImpairmentProfile, LoopbackUdpProxy};
use racc_net::{
    BindPolicy, NetError, SenderFrame, VideoReceiver, VideoSendMetrics, VideoSender,
    VideoTransportEvent, DEFAULT_FRAME_INTERVAL_US, THREAD_JOIN_TIMEOUT_MS,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

fn test_frame(frame_id: u32) -> SenderFrame {
    let keyframe = frame_id.is_multiple_of(50);
    let bytes_len = if keyframe && frame_id == 0 {
        233_328
    } else {
        match frame_id % 7 {
            0 => 1,
            1 => 1182,
            2 => 1183,
            3 => 2364,
            4 => 2365,
            5 => 8192,
            _ => 32_000,
        }
    };
    SenderFrame {
        epoch: 3,
        frame_id,
        keyframe,
        config: keyframe,
        capture_ts_us: frame_id.wrapping_mul(33_333),
        bytes: vec![u8::try_from(frame_id % 251).unwrap_or(0); bytes_len],
    }
}

fn drain(receiver: &VideoReceiver, frames: &mut Vec<racc_net::FrameData>, requests: &mut u64) {
    loop {
        match receiver.recv_event(Duration::from_millis(1)) {
            Ok(VideoTransportEvent::FrameReady(frame)) => frames.push(frame),
            Ok(VideoTransportEvent::NeedKeyframe(_)) => *requests = requests.saturating_add(1),
            Ok(VideoTransportEvent::Cursor(_)) => {}
            Err(NetError::Timeout) => break,
            Err(error) => panic!("loopback receiver failed: {error}"),
        }
    }
}

fn run_profile(
    loss_ppm: u32,
    seed: u64,
) -> (
    Vec<racc_net::FrameData>,
    racc_net::ReassemblyStats,
    u64,
    VideoSendMetrics,
) {
    let mut receiver = match VideoReceiver::bind(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        LOOPBACK,
        3,
        BindPolicy::TestOnlyLoopback,
    ) {
        Ok(value) => value,
        Err(error) => panic!("receiver bind failed: {error}"),
    };
    let profile = ImpairmentProfile {
        independent_loss_ppm: loss_ppm,
        ..ImpairmentProfile::default()
    };
    let mut proxy = match LoopbackUdpProxy::bind(
        receiver
            .local_addr()
            .unwrap_or_else(|error| panic!("receiver address failed: {error}")),
        profile,
        seed,
    ) {
        Ok(value) => value,
        Err(error) => panic!("proxy bind failed: {error}"),
    };
    let socket = match UdpSocket::bind("127.0.0.1:0") {
        Ok(value) => value,
        Err(error) => panic!("sender socket bind failed: {error}"),
    };
    let sender = match VideoSender::from_socket(
        socket,
        proxy.local_addr(),
        BindPolicy::TestOnlyLoopback,
        DEFAULT_FRAME_INTERVAL_US,
    ) {
        Ok(value) => value,
        Err(error) => panic!("video sender setup failed: {error}"),
    };

    let mut sender = sender;
    let mut frames = Vec::new();
    let mut request_events = 0u64;
    let mut keyframe_metrics = VideoSendMetrics::default();
    for frame_id in 0..300u32 {
        let frame = test_frame(frame_id);
        let expected_keyframe = frame.keyframe;
        let metrics = sender.send_frame(frame);
        assert!(
            metrics.is_ok(),
            "video send failed for frame {frame_id}: {metrics:?}"
        );
        if let Ok(metrics) = metrics {
            if expected_keyframe && metrics.bytes_sent > 200_000 {
                keyframe_metrics = metrics;
            }
        }
        drain(&receiver, &mut frames, &mut request_events);
    }

    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline {
        match receiver.recv_event(Duration::from_millis(10)) {
            Ok(VideoTransportEvent::FrameReady(frame)) => frames.push(frame),
            Ok(VideoTransportEvent::NeedKeyframe(_)) => {
                request_events = request_events.saturating_add(1)
            }
            Ok(VideoTransportEvent::Cursor(_)) => {}
            Err(NetError::Timeout) => {}
            Err(error) => panic!("loopback receiver failed while draining: {error}"),
        }
    }
    let stats = receiver.reassembly_stats();
    assert!(stats.is_ok());
    let stats = stats.unwrap_or_default();
    println!("proxy={:?} pacing={:?}", proxy.stats(), keyframe_metrics);
    println!(
        "UDP loopback loss={:.1}% delivered={} received={} dropped_incomplete={} dropped_gap={} requests={} proxy_lost={}",
        f64::from(loss_ppm) / 10_000.0,
        stats.counters.frames_delivered,
        stats.counters.datagrams_received,
        stats.counters.dropped_incomplete,
        stats.counters.dropped_gap,
        request_events,
        proxy.stats().lost
    );

    let close_start = Instant::now();
    assert!(sender.close().is_ok());
    assert!(close_start.elapsed() < Duration::from_millis(THREAD_JOIN_TIMEOUT_MS));
    let close_start = Instant::now();
    assert!(receiver.close().is_ok());
    assert!(close_start.elapsed() < Duration::from_millis(THREAD_JOIN_TIMEOUT_MS));
    let close_start = Instant::now();
    assert!(proxy.close().is_ok());
    assert!(close_start.elapsed() < Duration::from_millis(THREAD_JOIN_TIMEOUT_MS));
    assert!(proxy.stats().received > 0);
    (frames, stats, request_events, keyframe_metrics)
}

#[test]
fn real_udp_loopback_proxy_preserves_frames_and_recovers_under_seeded_loss() {
    let _serial = crate::TIMING_SENSITIVE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (delivered, stats, requests, pacing) = run_profile(0, 0x1234);

    assert_eq!(delivered.len(), 300);
    assert_eq!(stats.counters.frames_delivered, 300);
    assert!(delivered
        .windows(2)
        .all(|pair| pair[0].frame_id < pair[1].frame_id));
    for frame in delivered {
        assert_eq!(frame.bytes, test_frame(frame.frame_id).bytes);
    }
    assert_eq!(requests, 0);
    println!(
        "PACING 233KB frame={} datagrams={} duration_us={} max_gap_us={} max_sleep_us={} bytes={}",
        pacing.frame_id,
        pacing.datagrams_sent,
        pacing.send_duration_us,
        pacing.max_inter_datagram_gap_us,
        pacing.observed_sleep_granularity_us,
        pacing.bytes_sent
    );

    let (delivered, stats, requests, _) = run_profile(20_000, 0x5eed);
    assert!(delivered
        .windows(2)
        .all(|pair| pair[0].frame_id < pair[1].frame_id));
    for frame in delivered {
        assert_eq!(frame.bytes, test_frame(frame.frame_id).bytes);
    }
    assert!(stats.counters.dropped_incomplete > 0 || stats.counters.dropped_gap > 0);
    assert!(requests > 0);
}

#[test]
fn receiver_counts_datagrams_from_an_unexpected_peer_ip() {
    let receiver = VideoReceiver::bind(
        SocketAddr::from(([127, 0, 0, 1], 0)),
        "100.64.0.1".parse().unwrap_or(LOOPBACK),
        1,
        BindPolicy::TestOnlyLoopback,
    );
    assert!(receiver.is_ok());
    let mut receiver = match receiver {
        Ok(value) => value,
        Err(_) => return,
    };
    let sender = UdpSocket::bind("127.0.0.1:0");
    assert!(sender.is_ok());
    if let Ok(sender) = sender {
        let _ = sender.send_to(
            &[1, 2, 3],
            receiver
                .local_addr()
                .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], 0))),
        );
    }
    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline && receiver.counters().unexpected_peer_datagrams == 0 {
        thread::yield_now();
    }
    assert_eq!(receiver.counters().unexpected_peer_datagrams, 1);
    assert!(receiver.close().is_ok());
}
