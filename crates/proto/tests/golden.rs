use racc_proto::{
    decode_control_frame, encode_cursor_datagram, encode_video_datagram, parse_cursor_datagram,
    parse_video_datagram, CaptureBackend, ClipboardOrigin, ClipboardSyncControl, ClipboardUpdate,
    ControlMessage, ControlPayload, CursorShape, CursorUpdate, DisplayInfo, Encoder, Goodbye,
    GoodbyeReason, Hello, HelloAck, HelloStatus, HostEventKind, HostEventReport, InputEvent,
    InputEventKind, LogicalClock, OsType, PauseVideo, Ping, Pong, QualityAdjustment,
    QualityAdjustmentReason, RequestKeyframe, ResumeVideo, SetQuality, StatsReport, StreamCodec,
    StreamReset, StreamStatus, SwitchMonitor, TopologyAnnounce, VideoDatagram, VideoHeader,
    ViewerReport, CLIPBOARD_LOGICAL_CLOCK_VERSION, MAX_CLIPBOARD_BYTES,
    MAX_CLIPBOARD_LOGICAL_CLOCK, MAX_CONTROL_FRAME_BYTES, MAX_CURSOR_BYTES, MAX_CURSOR_DIM,
    MAX_DATAGRAM, MAX_DISPLAYS, MAX_FRAGMENTS_PER_FRAME, MAX_NAME_BYTES, MAX_VIDEO_PAYLOAD,
    MAX_VIEWER_REPORT_DROPPED_FRAMES, MAX_VIEWER_REPORT_DURATION_MS, PROTOCOL_VERSION,
    VIDEO_FLAG_CONFIG, VIDEO_FLAG_KEY, VIDEO_FLAG_LAST_FRAGMENT, VIDEO_HEADER_LEN,
};
use std::fmt::Debug;

fn hex(value: &str) -> Vec<u8> {
    value
        .split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).expect("golden vector uses valid hex"))
        .collect()
}

fn samples() -> Vec<ControlMessage> {
    vec![
        ControlMessage::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            device_name: "VIEW".into(),
            os: OsType::Windows,
            app_version: "1.2".into(),
            video_udp_port: 41000,
            codecs: 1,
            max_height: 1080,
            features: 1,
        }),
        ControlMessage::HelloAck(HelloAck {
            protocol_version: PROTOCOL_VERSION,
            status: HelloStatus::Ok,
            device_name: "HOST".into(),
            os: OsType::MacOs,
            app_version: "1.2".into(),
            codecs: 1,
            max_height: 1080,
            features: 1,
            host_cpu_cores: 8,
        }),
        ControlMessage::TopologyAnnounce(TopologyAnnounce {
            topology_rev: 1,
            active_display_id: 1,
            displays: vec![
                DisplayInfo {
                    display_id: 1,
                    name: "D1".into(),
                    x: -1920,
                    y: 0,
                    width_px: 1920,
                    height_px: 1080,
                    scale_milli: 1500,
                    refresh_mhz: 60000,
                    flags: 7,
                },
                DisplayInfo {
                    display_id: 2,
                    name: "D2".into(),
                    x: 0,
                    y: 0,
                    width_px: 1280,
                    height_px: 1024,
                    scale_milli: 1000,
                    refresh_mhz: 60000,
                    flags: 4,
                },
            ],
        }),
        ControlMessage::SwitchMonitor(SwitchMonitor {
            req_id: 3,
            display_id: 2,
        }),
        ControlMessage::StreamReset(StreamReset {
            req_id: 0,
            epoch: 2,
            codec: StreamCodec::H264,
            width: 1280,
            height: 720,
            fps: 30,
            topology_rev: 3,
            display_id: 1,
            status: StreamStatus::Ok,
        }),
        ControlMessage::SetQuality(SetQuality {
            max_height: 720,
            bitrate_hint_kbps: 4000,
        }),
        ControlMessage::RequestKeyframe(RequestKeyframe { epoch: 2 }),
        ControlMessage::PauseVideo(PauseVideo),
        ControlMessage::ResumeVideo(ResumeVideo),
        ControlMessage::InputEvent(InputEvent {
            epoch: 0x1234,
            display_id: 1,
            event: InputEventKind::MouseMoveAbs {
                u: 0x8000,
                v: u16::MAX,
            },
        }),
        ControlMessage::ClipboardUpdate(ClipboardUpdate {
            seq: 7,
            origin: ClipboardOrigin::Viewer,
            logical_clock: LogicalClock::new(9).expect("clock within bound"),
            text: "hi".into(),
        }),
        ControlMessage::StatsReport(StatsReport {
            host_cpu_pct_x10: 123,
            capture_backend: CaptureBackend::Dxgi,
            encoder: Encoder::MediaFoundationHw,
            width: 1280,
            height: 720,
            display_refresh_mhz: 60000,
            target_bitrate_kbps: 4000,
            actual_bitrate_kbps: 3900,
            process_cpu_pct_x10: Some(321),
        }),
        ControlMessage::HostEventReport(HostEventReport {
            kind: HostEventKind::CaptureLost,
        }),
        ControlMessage::Ping(Ping {
            nonce: 0x0102_0304_0506_0708,
            sender_ts_us: 9,
        }),
        ControlMessage::Pong(Pong {
            nonce: 7,
            echo_ts_us: 8,
        }),
        ControlMessage::CursorShape(CursorShape {
            shape_id: 5,
            width: 2,
            height: 1,
            hotspot_x: 1,
            hotspot_y: 0,
            blend_mode: racc_proto::CursorBlendMode::PremultipliedAlpha,
            bgra: vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
        }),
        ControlMessage::Goodbye(Goodbye {
            reason: GoodbyeReason::Normal,
        }),
        ControlMessage::ClipboardSyncControl(ClipboardSyncControl { enabled: true }),
        ControlMessage::QualityAdjustment(QualityAdjustment {
            epoch: 0x1234,
            reason: QualityAdjustmentReason::QueueOverflow,
            from_height: 720,
            to_height: 480,
            from_bitrate_bps: 3_500_000,
            to_bitrate_bps: 1_500_000,
        }),
    ]
}

fn frame(message: &ControlMessage) -> Vec<u8> {
    let mut bytes = Vec::new();
    message
        .encode_frame(&mut bytes)
        .expect("sample message is valid");
    bytes
}
fn assert_payload_round_trip<T: ControlPayload + PartialEq + Debug>(expected: &T, payload: &[u8]) {
    let (decoded, consumed) = T::decode_payload(payload).expect("valid payload");
    assert_eq!(&decoded, expected);
    assert_eq!(consumed, payload.len());
}

#[test]
fn constants_match_v5_wire_limits() {
    assert_eq!(PROTOCOL_VERSION, 5);
    assert_eq!(MAX_DATAGRAM, 1200);
    assert_eq!(VIDEO_HEADER_LEN, 18);
    assert_eq!(MAX_VIDEO_PAYLOAD, 1182);
    assert_eq!(MAX_FRAGMENTS_PER_FRAME, 1024);
    assert_eq!(MAX_CONTROL_FRAME_BYTES, 1_048_576);
    assert_eq!(MAX_CLIPBOARD_BYTES, 524_288);
    assert_eq!(CLIPBOARD_LOGICAL_CLOCK_VERSION, 1);
    assert_eq!(MAX_CLIPBOARD_LOGICAL_CLOCK, i64::MAX as u64);
    assert_eq!(MAX_VIEWER_REPORT_DURATION_MS, 60_000);
    assert_eq!(MAX_VIEWER_REPORT_DROPPED_FRAMES, 1_000_000);
    assert_eq!(MAX_DISPLAYS, 16);
    assert_eq!(MAX_NAME_BYTES, 128);
    assert_eq!(MAX_CURSOR_DIM, 128);
    assert_eq!(MAX_CURSOR_BYTES, 65_536);
    assert_eq!(racc_proto::CursorBlendMode::PremultipliedAlpha as u8, 0);
    assert_eq!(racc_proto::CursorBlendMode::WindowsMaskedColor as u8, 1);
    assert_eq!(racc_proto::CursorBlendMode::WindowsAndXor as u8, 2);
}

#[test]
fn video_header_golden_vectors_round_trip() {
    let cases = [
        (
            VideoHeader {
                version: PROTOCOL_VERSION,
                flags: VIDEO_FLAG_KEY | VIDEO_FLAG_LAST_FRAGMENT | VIDEO_FLAG_CONFIG,
                epoch: 0x1234,
                frame_id: 0x0102_0304,
                frag_idx: 2,
                frag_cnt: 3,
                capture_ts_us: 0x0a0b_0c0d,
            },
            vec![0x00, 0x00, 0x00, 0x01, 0x65],
            "05 01 07 00 34 12 04 03 02 01 02 00 03 00 0d 0c 0b 0a 00 00 00 01 65",
        ),
        (
            VideoHeader {
                version: PROTOCOL_VERSION,
                flags: VIDEO_FLAG_KEY | VIDEO_FLAG_CONFIG,
                epoch: 0x1234,
                frame_id: 0x0102_0304,
                frag_idx: 1,
                frag_cnt: 3,
                capture_ts_us: 0x0a0b_0c0d,
            },
            vec![0x00, 0x00, 0x01, 0x41],
            "05 01 05 00 34 12 04 03 02 01 01 00 03 00 0d 0c 0b 0a 00 00 01 41",
        ),
    ];
    for (header, payload, expected_hex) in cases {
        let expected = hex(expected_hex);
        let mut encoded = Vec::new();
        encode_video_datagram(header, &payload, &mut encoded).expect("valid header");
        assert_eq!(encoded, expected);
        for prefix_len in 0..=VIDEO_HEADER_LEN {
            assert!(parse_video_datagram(&expected[..prefix_len]).is_err());
        }
        assert_eq!(
            parse_video_datagram(&expected).expect("valid datagram"),
            VideoDatagram::Video {
                header,
                payload: &payload
            }
        );
    }
}

#[test]
fn cursor_datagram_golden_vector_round_trips() {
    let cursor = CursorUpdate {
        epoch: 0x1234,
        shape_id: 0x1122_3344,
        x: -2,
        y: 0x0102_0304,
        visible: true,
    };
    let expected = hex("05 02 00 00 34 12 44 33 22 11 fe ff ff ff 04 03 02 01 01");
    let mut encoded = Vec::new();
    encode_cursor_datagram(cursor, &mut encoded);
    assert_eq!(encoded, expected);
    assert_eq!(parse_cursor_datagram(&expected), Ok(cursor));
    assert_eq!(
        parse_video_datagram(&expected),
        Ok(VideoDatagram::Cursor(cursor))
    );
}

#[test]
fn hello_control_prefix_and_topology_golden_vectors_round_trip() {
    let hello = ControlMessage::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
        device_name: "A".into(),
        os: OsType::MacOs,
        app_version: "1".into(),
        video_udp_port: 0x1234,
        codecs: 1,
        max_height: 1080,
        features: 1,
    });
    let hello_vector = hex("13 00 00 00 01 05 01 41 02 01 31 34 12 01 00 00 00 38 04 01 00 00 00");
    assert_eq!(&frame(&hello)[..4], &hex("13 00 00 00"));
    assert_eq!(frame(&hello), hello_vector);
    assert_eq!(ControlMessage::decode_body(&hello_vector[4..]), Ok(hello));

    let topology = samples()
        .into_iter()
        .find(|m| matches!(m, ControlMessage::TopologyAnnounce(_)))
        .unwrap();
    let topology_vector = hex(
        "46 00 00 00 03 01 00 00 00 01 00 00 00 02          01 00 00 00 02 44 31 80 f8 ff ff 00 00 00 00 80 07 00 00 38 04 00 00 dc 05 60 ea 00 00 07          02 00 00 00 02 44 32 00 00 00 00 00 00 00 00 00 05 00 00 00 04 00 00 e8 03 60 ea 00 00 04",
    );
    assert_eq!(frame(&topology), topology_vector);
    assert_eq!(
        ControlMessage::decode_body(&topology_vector[4..]),
        Ok(topology)
    );
}

#[test]
fn stream_reset_golden_vector_round_trips() {
    let message = ControlMessage::StreamReset(StreamReset {
        req_id: 0,
        epoch: 2,
        codec: StreamCodec::H264,
        width: 1280,
        height: 720,
        fps: 30,
        topology_rev: 3,
        display_id: 1,
        status: StreamStatus::Ok,
    });
    let expected =
        hex("16 00 00 00 05 00 00 00 00 02 00 01 00 05 d0 02 1e 03 00 00 00 01 00 00 00 00");
    assert_eq!(frame(&message), expected);
    assert_eq!(ControlMessage::decode_body(&expected[4..]), Ok(message));
}

#[test]
fn every_input_event_tag_has_a_golden_vector() {
    let values = [
        (
            InputEventKind::MouseMoveAbs {
                u: 0x8000,
                v: u16::MAX,
            },
            "0c 00 00 00 0a 34 12 01 00 00 00 01 00 80 ff ff",
        ),
        (
            InputEventKind::MouseMoveRel { dx: -2, dy: 300 },
            "0c 00 00 00 0a 34 12 01 00 00 00 02 fe ff 2c 01",
        ),
        (
            InputEventKind::MouseButton {
                button: 4,
                pressed: true,
            },
            "0a 00 00 00 0a 34 12 01 00 00 00 03 04 01",
        ),
        (
            InputEventKind::Wheel { dx: -120, dy: 240 },
            "0c 00 00 00 0a 34 12 01 00 00 00 04 88 ff f0 00",
        ),
        (
            InputEventKind::Key {
                hid_usage: 4,
                pressed: true,
                modifiers: 3,
            },
            "0c 00 00 00 0a 34 12 01 00 00 00 05 04 00 01 03",
        ),
    ];
    for (event, expected_hex) in values {
        let message = ControlMessage::InputEvent(InputEvent {
            epoch: 0x1234,
            display_id: 1,
            event,
        });
        let expected = hex(expected_hex);
        assert_eq!(frame(&message), expected);
        assert_eq!(ControlMessage::decode_body(&expected[4..]), Ok(message));
    }
}

#[test]
fn clipboard_and_cursor_shape_golden_vectors_round_trip() {
    for (enabled, expected) in [(true, "02 00 00 00 12 01"), (false, "02 00 00 00 12 00")] {
        let control = ControlMessage::ClipboardSyncControl(ClipboardSyncControl { enabled });
        let expected = hex(expected);
        assert_eq!(frame(&control), expected);
        assert_eq!(ControlMessage::decode_body(&expected[4..]), Ok(control));
    }

    let clipboard = ControlMessage::ClipboardUpdate(ClipboardUpdate {
        seq: 7,
        origin: ClipboardOrigin::Viewer,
        logical_clock: LogicalClock::new(9).expect("clock within bound"),
        text: "hi".into(),
    });
    let clipboard_vector =
        hex("16 00 00 00 0b 07 00 00 00 00 01 09 00 00 00 00 00 00 00 01 02 00 00 00 68 69");
    assert_eq!(frame(&clipboard), clipboard_vector);
    assert_eq!(
        ControlMessage::decode_body(&clipboard_vector[4..]),
        Ok(clipboard)
    );

    let report = ControlMessage::ViewerReport(ViewerReport {
        epoch: 0x1234,
        loss_permille: 12,
        frame_loss_permille: 34,
        rtt_ms: 55,
        decode_ms_p95: 6,
        dropped_frames: 7,
    });
    let report_vector = hex("13 00 00 00 11 34 12 0c 00 22 00 37 00 00 00 06 00 00 00 07 00 00 00");
    assert_eq!(frame(&report), report_vector);
    assert_eq!(ControlMessage::decode_body(&report_vector[4..]), Ok(report));

    let stats = ControlMessage::StatsReport(StatsReport {
        host_cpu_pct_x10: 123,
        capture_backend: CaptureBackend::Dxgi,
        encoder: Encoder::MediaFoundationHw,
        width: 1280,
        height: 720,
        display_refresh_mhz: 60000,
        target_bitrate_kbps: 4000,
        actual_bitrate_kbps: 3900,
        process_cpu_pct_x10: Some(321),
    });
    let stats_vector =
        hex("17 00 00 00 0c 7b 00 01 01 00 05 d0 02 60 ea 00 00 a0 0f 00 00 3c 0f 00 00 41 01");
    assert_eq!(frame(&stats), stats_vector);
    assert_eq!(ControlMessage::decode_body(&stats_vector[4..]), Ok(stats));

    let host_event = ControlMessage::HostEventReport(HostEventReport {
        kind: HostEventKind::CaptureLost,
    });
    let host_event_vector = hex("02 00 00 00 14 00");
    assert_eq!(frame(&host_event), host_event_vector);
    assert_eq!(
        ControlMessage::decode_body(&host_event_vector[4..]),
        Ok(host_event)
    );

    let adjustment = ControlMessage::QualityAdjustment(QualityAdjustment {
        epoch: 0x1234,
        reason: QualityAdjustmentReason::QueueOverflow,
        from_height: 720,
        to_height: 480,
        from_bitrate_bps: 3_500_000,
        to_bitrate_bps: 1_500_000,
    });
    let adjustment_vector = hex("10 00 00 00 13 34 12 02 d0 02 e0 01 e0 67 35 00 60 e3 16 00");
    assert_eq!(frame(&adjustment), adjustment_vector);
    assert_eq!(
        ControlMessage::decode_body(&adjustment_vector[4..]),
        Ok(adjustment)
    );

    let cursor = ControlMessage::CursorShape(CursorShape {
        shape_id: 5,
        width: 2,
        height: 1,
        hotspot_x: 1,
        hotspot_y: 0,
        blend_mode: racc_proto::CursorBlendMode::PremultipliedAlpha,
        bgra: vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
    });
    let cursor_vector =
        hex("16 00 00 00 0f 05 00 00 00 02 00 01 00 01 00 00 00 00 11 22 33 44 55 66 77 88");
    assert_eq!(frame(&cursor), cursor_vector);
    assert_eq!(ControlMessage::decode_body(&cursor_vector[4..]), Ok(cursor));
}

#[test]
fn cursor_blend_modes_round_trip_with_stable_wire_values() {
    for (mode, wire_value) in [
        (racc_proto::CursorBlendMode::PremultipliedAlpha, 0),
        (racc_proto::CursorBlendMode::WindowsMaskedColor, 1),
        (racc_proto::CursorBlendMode::WindowsAndXor, 2),
    ] {
        let message = ControlMessage::CursorShape(CursorShape {
            shape_id: 0x0102_0304,
            width: 1,
            height: 1,
            hotspot_x: 0,
            hotspot_y: 0,
            blend_mode: mode,
            bgra: vec![0, 0, 0, 0],
        });
        let encoded = frame(&message);
        assert_eq!(encoded[17], wire_value);
        assert_eq!(ControlMessage::decode_body(&encoded[4..]), Ok(message));
    }
}

#[test]
fn quality_adjustment_rejects_invalid_reason_tier_and_bitrate() {
    let valid = hex("10 00 00 00 13 34 12 02 d0 02 e0 01 e0 67 35 00 60 e3 16 00");
    for (index, value) in [
        (7, 6),
        (8, 0xf4),
        (9, 0x01),
        (12, 0xff),
        (13, 0xff),
        (14, 0xff),
        (15, 0xff),
    ] {
        let mut malformed = valid.clone();
        malformed[index] = value;
        assert!(ControlMessage::decode_body(&malformed[4..]).is_err());
    }
}

#[test]
fn every_control_message_round_trips_and_rejects_trailing_bytes() {
    for message in samples() {
        let encoded = frame(&message);
        let (decoded, consumed) = decode_control_frame(&encoded).expect("valid frame");
        assert_eq!(decoded, message);
        assert_eq!(consumed, encoded.len());
        let payload = &encoded[5..];
        match &message {
            ControlMessage::Hello(value) => assert_payload_round_trip(value, payload),
            ControlMessage::HelloAck(value) => assert_payload_round_trip(value, payload),
            ControlMessage::TopologyAnnounce(value) => assert_payload_round_trip(value, payload),
            ControlMessage::SwitchMonitor(value) => assert_payload_round_trip(value, payload),
            ControlMessage::StreamReset(value) => assert_payload_round_trip(value, payload),
            ControlMessage::SetQuality(value) => assert_payload_round_trip(value, payload),
            ControlMessage::RequestKeyframe(value) => assert_payload_round_trip(value, payload),
            ControlMessage::PauseVideo(value) => assert_payload_round_trip(value, payload),
            ControlMessage::ResumeVideo(value) => assert_payload_round_trip(value, payload),
            ControlMessage::InputEvent(value) => assert_payload_round_trip(value, payload),
            ControlMessage::ClipboardUpdate(value) => assert_payload_round_trip(value, payload),
            ControlMessage::ViewerReport(value) => assert_payload_round_trip(value, payload),
            ControlMessage::ClipboardSyncControl(value) => {
                assert_payload_round_trip(value, payload)
            }
            ControlMessage::QualityAdjustment(value) => assert_payload_round_trip(value, payload),
            ControlMessage::StatsReport(value) => assert_payload_round_trip(value, payload),
            ControlMessage::HostEventReport(value) => assert_payload_round_trip(value, payload),
            ControlMessage::Ping(value) => assert_payload_round_trip(value, payload),
            ControlMessage::Pong(value) => assert_payload_round_trip(value, payload),
            ControlMessage::CursorShape(value) => assert_payload_round_trip(value, payload),
            ControlMessage::Goodbye(value) => assert_payload_round_trip(value, payload),
        }
        assert_eq!(ControlMessage::decode_body(&encoded[4..]), Ok(message));

        let mut body_with_trailing = encoded[4..].to_vec();
        body_with_trailing.push(0);
        assert_eq!(
            ControlMessage::decode_body(&body_with_trailing),
            Err(racc_proto::ProtoError::TrailingBytes)
        );

        for prefix_len in 0..encoded.len() {
            assert!(decode_control_frame(&encoded[..prefix_len]).is_err());
        }
    }
}

#[test]
fn boundary_values_round_trip() {
    let max_name = "n".repeat(MAX_NAME_BYTES);
    let max_text = "x".repeat(MAX_CLIPBOARD_BYTES);
    let max_cursor = CursorShape {
        shape_id: u32::MAX,
        width: 128,
        height: 128,
        hotspot_x: 127,
        hotspot_y: 127,
        blend_mode: racc_proto::CursorBlendMode::PremultipliedAlpha,
        bgra: vec![0xa5; MAX_CURSOR_BYTES],
    };
    let messages = vec![
        ControlMessage::Hello(Hello {
            protocol_version: u8::MAX,
            device_name: max_name.clone(),
            os: OsType::Linux,
            app_version: max_name.clone(),
            video_udp_port: u16::MAX,
            codecs: 1,
            max_height: u16::MAX,
            features: 1,
        }),
        ControlMessage::HelloAck(HelloAck {
            protocol_version: u8::MAX,
            status: HelloStatus::Busy,
            device_name: max_name.clone(),
            os: OsType::Unknown,
            app_version: max_name,
            codecs: 1,
            max_height: u16::MAX,
            features: 1,
            host_cpu_cores: u8::MAX,
        }),
        ControlMessage::TopologyAnnounce(TopologyAnnounce {
            topology_rev: u32::MAX,
            active_display_id: u32::MAX,
            displays: (0..MAX_DISPLAYS)
                .map(|id| DisplayInfo {
                    display_id: id as u32,
                    name: String::new(),
                    x: i32::MIN,
                    y: i32::MAX,
                    width_px: u32::MAX,
                    height_px: u32::MAX,
                    scale_milli: u16::MAX,
                    refresh_mhz: u32::MAX,
                    flags: 0x0f,
                })
                .collect(),
        }),
        ControlMessage::SwitchMonitor(SwitchMonitor {
            req_id: u32::MAX,
            display_id: u32::MAX,
        }),
        ControlMessage::StreamReset(StreamReset {
            req_id: u32::MAX,
            epoch: u16::MAX,
            codec: StreamCodec::H264,
            width: u16::MAX,
            height: u16::MAX,
            fps: u8::MAX,
            topology_rev: u32::MAX,
            display_id: u32::MAX,
            status: StreamStatus::Busy,
        }),
        ControlMessage::SetQuality(SetQuality {
            max_height: 0,
            bitrate_hint_kbps: u32::MAX,
        }),
        ControlMessage::RequestKeyframe(RequestKeyframe { epoch: u16::MAX }),
        ControlMessage::ClipboardUpdate(ClipboardUpdate {
            seq: u32::MAX,
            origin: ClipboardOrigin::Host,
            logical_clock: LogicalClock {
                version: CLIPBOARD_LOGICAL_CLOCK_VERSION,
                counter: MAX_CLIPBOARD_LOGICAL_CLOCK,
            },
            text: max_text,
        }),
        ControlMessage::InputEvent(InputEvent {
            epoch: u16::MAX,
            display_id: u32::MAX,
            event: InputEventKind::MouseMoveRel {
                dx: i16::MIN,
                dy: i16::MAX,
            },
        }),
        ControlMessage::CursorShape(max_cursor),
        ControlMessage::Ping(Ping {
            nonce: u64::MAX,
            sender_ts_us: u64::MAX,
        }),
        ControlMessage::Pong(Pong {
            nonce: 0,
            echo_ts_us: 0,
        }),
        ControlMessage::ViewerReport(ViewerReport {
            epoch: u16::MAX,
            loss_permille: 1000,
            frame_loss_permille: 1000,
            rtt_ms: MAX_VIEWER_REPORT_DURATION_MS,
            decode_ms_p95: MAX_VIEWER_REPORT_DURATION_MS,
            dropped_frames: MAX_VIEWER_REPORT_DROPPED_FRAMES,
        }),
    ];
    for message in messages {
        let encoded = frame(&message);
        assert_eq!(decode_control_frame(&encoded).unwrap().0, message);
    }
}

#[test]
fn maximal_video_datagram_is_exactly_1200_bytes() {
    let header = VideoHeader {
        version: PROTOCOL_VERSION,
        flags: VIDEO_FLAG_LAST_FRAGMENT,
        epoch: 1,
        frame_id: u32::MAX,
        frag_idx: 0,
        frag_cnt: 1,
        capture_ts_us: u32::MAX,
    };
    let mut bytes = Vec::new();
    encode_video_datagram(header, &vec![0x5a; MAX_VIDEO_PAYLOAD], &mut bytes).unwrap();
    assert_eq!(bytes.len(), MAX_DATAGRAM);
    assert!(
        matches!(parse_video_datagram(&bytes), Ok(VideoDatagram::Video { payload, .. }) if payload.len() == MAX_VIDEO_PAYLOAD)
    );
}
