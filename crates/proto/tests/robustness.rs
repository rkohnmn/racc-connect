use proptest::prelude::*;
use racc_proto::{
    decode_control_frame, parse_cursor_datagram, parse_video_datagram, CaptureBackend,
    ClipboardOrigin, ClipboardSyncControl, ClipboardUpdate, ControlMessage, ControlPayload,
    CursorShape, CursorUpdate, DisplayInfo, Encoder, FrameDecoder, Goodbye, GoodbyeReason, Hello,
    HelloAck, HelloStatus, HostEventKind, HostEventReport, InputEvent, InputEventKind,
    LogicalClock, OsType, PauseVideo, Ping, Pong, ProtoError, QualityAdjustment,
    QualityAdjustmentReason, RequestKeyframe, ResumeVideo, SetQuality, StatsReport, StreamCodec,
    StreamReset, StreamStatus, SwitchMonitor, TopologyAnnounce, VideoDatagram, ViewerReport,
    CLIPBOARD_LOGICAL_CLOCK_VERSION, MAX_CLIPBOARD_BYTES, MAX_CLIPBOARD_LOGICAL_CLOCK,
    MAX_CONTROL_FRAME_BYTES, MAX_DATAGRAM, MAX_DISPLAYS, MAX_FRAGMENTS_PER_FRAME,
    MAX_VIEWER_REPORT_DROPPED_FRAMES, MAX_VIEWER_REPORT_DURATION_MS, PROTOCOL_VERSION,
    VIDEO_FLAG_LAST_FRAGMENT,
};

fn ascii_string() -> impl Strategy<Value = String> {
    prop::collection::vec(0u8..26, 0..24).prop_map(|bytes| {
        bytes
            .into_iter()
            .map(|byte| char::from(b'a' + byte))
            .collect()
    })
}

fn arb_event() -> impl Strategy<Value = InputEventKind> {
    prop_oneof![
        (any::<u16>(), any::<u16>()).prop_map(|(u, v)| InputEventKind::MouseMoveAbs { u, v }),
        (any::<i16>(), any::<i16>()).prop_map(|(dx, dy)| InputEventKind::MouseMoveRel { dx, dy }),
        (1u8..=5, any::<bool>())
            .prop_map(|(button, pressed)| InputEventKind::MouseButton { button, pressed }),
        (any::<i16>(), any::<i16>()).prop_map(|(dx, dy)| InputEventKind::Wheel { dx, dy }),
        (any::<u16>(), any::<bool>(), 0u8..16).prop_map(|(hid_usage, pressed, modifiers)| {
            InputEventKind::Key {
                hid_usage,
                pressed,
                modifiers,
            }
        }),
    ]
}

fn arb_control_message() -> impl Strategy<Value = ControlMessage> {
    let os = prop_oneof![
        Just(OsType::Unknown),
        Just(OsType::Windows),
        Just(OsType::MacOs),
        Just(OsType::Linux),
    ];
    let hello_status = prop_oneof![
        Just(HelloStatus::Ok),
        Just(HelloStatus::UnsupportedVersion),
        Just(HelloStatus::NotAuthorized),
        Just(HelloStatus::Busy),
    ];
    let stream_status = prop_oneof![
        Just(StreamStatus::Ok),
        Just(StreamStatus::DisplayNotFound),
        Just(StreamStatus::CaptureFailed),
        Just(StreamStatus::EncoderFailed),
        Just(StreamStatus::Paused),
        Just(StreamStatus::Busy),
    ];
    let capture_backend = prop_oneof![
        Just(CaptureBackend::Unknown),
        Just(CaptureBackend::Dxgi),
        Just(CaptureBackend::Wgc),
        Just(CaptureBackend::ScreenCaptureKit),
        Just(CaptureBackend::CgDisplayStream),
    ];
    let encoder = prop_oneof![
        Just(Encoder::Unknown),
        Just(Encoder::MediaFoundationHw),
        Just(Encoder::OpenH264),
        Just(Encoder::VideoToolbox),
        Just(Encoder::Nvenc),
        Just(Encoder::Amf),
        Just(Encoder::Qsv),
    ];
    let goodbye = prop_oneof![
        Just(GoodbyeReason::Normal),
        Just(GoodbyeReason::Error),
        Just(GoodbyeReason::Superseded),
        Just(GoodbyeReason::NotAuthorized),
        Just(GoodbyeReason::Shutdown),
    ];
    let event = arb_event();

    prop_oneof![
        (
            any::<u8>(),
            ascii_string(),
            os.clone(),
            ascii_string(),
            any::<u16>(),
            any::<bool>(),
            any::<u16>(),
            any::<bool>()
        )
            .prop_map(
                |(
                    protocol_version,
                    device_name,
                    os,
                    app_version,
                    video_udp_port,
                    codec,
                    max_height,
                    clipboard,
                )| {
                    ControlMessage::Hello(Hello {
                        protocol_version,
                        device_name,
                        os,
                        app_version,
                        video_udp_port,
                        codecs: u32::from(codec),
                        max_height,
                        features: u32::from(clipboard),
                    })
                }
            ),
        (
            any::<u8>(),
            hello_status,
            ascii_string(),
            os,
            ascii_string(),
            any::<bool>(),
            any::<u16>(),
            any::<bool>(),
            any::<u8>()
        )
            .prop_map(
                |(
                    protocol_version,
                    status,
                    device_name,
                    os,
                    app_version,
                    codec,
                    max_height,
                    clipboard,
                    host_cpu_cores,
                )| {
                    ControlMessage::HelloAck(HelloAck {
                        protocol_version,
                        status,
                        device_name,
                        os,
                        app_version,
                        codecs: u32::from(codec),
                        max_height,
                        features: u32::from(clipboard),
                        host_cpu_cores,
                    })
                }
            ),
        (
            any::<u32>(),
            any::<u32>(),
            any::<u32>(),
            ascii_string(),
            any::<i32>(),
            any::<i32>(),
            1u32..=u32::MAX,
            1u32..=u32::MAX,
            any::<u16>(),
            any::<u32>(),
            0u8..16
        )
            .prop_map(
                |(
                    topology_rev,
                    active_display_id,
                    display_id,
                    name,
                    x,
                    y,
                    width_px,
                    height_px,
                    scale_milli,
                    refresh_mhz,
                    flags,
                )| {
                    ControlMessage::TopologyAnnounce(TopologyAnnounce {
                        topology_rev,
                        active_display_id,
                        displays: vec![DisplayInfo {
                            display_id,
                            name,
                            x,
                            y,
                            width_px,
                            height_px,
                            scale_milli,
                            refresh_mhz,
                            flags,
                        }],
                    })
                }
            ),
        (any::<u32>(), any::<u32>()).prop_map(
            |(req_id, display_id)| ControlMessage::SwitchMonitor(SwitchMonitor {
                req_id,
                display_id
            })
        ),
        (
            any::<u32>(),
            any::<u16>(),
            1u16..=u16::MAX,
            1u16..=u16::MAX,
            1u8..=u8::MAX,
            any::<u32>(),
            any::<u32>(),
            stream_status
        )
            .prop_map(
                |(req_id, epoch, width, height, fps, topology_rev, display_id, status)| {
                    ControlMessage::StreamReset(StreamReset {
                        req_id,
                        epoch,
                        codec: StreamCodec::H264,
                        width,
                        height,
                        fps,
                        topology_rev,
                        display_id,
                        status,
                    })
                }
            ),
        (
            prop_oneof![Just(0), Just(480), Just(720), Just(1080)],
            any::<u32>()
        )
            .prop_map(
                |(max_height, bitrate_hint_kbps)| ControlMessage::SetQuality(SetQuality {
                    max_height,
                    bitrate_hint_kbps
                })
            ),
        any::<u16>().prop_map(|epoch| ControlMessage::RequestKeyframe(RequestKeyframe { epoch })),
        Just(ControlMessage::PauseVideo(PauseVideo)),
        Just(ControlMessage::ResumeVideo(ResumeVideo)),
        (any::<u16>(), any::<u32>(), event).prop_map(|(epoch, display_id, event)| {
            ControlMessage::InputEvent(InputEvent {
                epoch,
                display_id,
                event,
            })
        }),
        (
            any::<u32>(),
            any::<bool>(),
            0u64..=MAX_CLIPBOARD_LOGICAL_CLOCK,
            ascii_string()
        )
            .prop_map(|(seq, host_origin, counter, text)| {
                ControlMessage::ClipboardUpdate(ClipboardUpdate {
                    seq,
                    origin: if host_origin {
                        ClipboardOrigin::Host
                    } else {
                        ClipboardOrigin::Viewer
                    },
                    logical_clock: LogicalClock {
                        version: CLIPBOARD_LOGICAL_CLOCK_VERSION,
                        counter,
                    },
                    text,
                })
            }),
        (
            0u16..=1000,
            capture_backend,
            encoder,
            any::<u16>(),
            any::<u16>(),
            any::<u32>(),
            any::<u32>(),
            any::<u32>()
        )
            .prop_map(
                |(
                    host_cpu_pct_x10,
                    capture_backend,
                    encoder,
                    width,
                    height,
                    display_refresh_mhz,
                    target_bitrate_kbps,
                    actual_bitrate_kbps,
                )| {
                    ControlMessage::StatsReport(StatsReport {
                        host_cpu_pct_x10,
                        capture_backend,
                        encoder,
                        width,
                        height,
                        display_refresh_mhz,
                        target_bitrate_kbps,
                        actual_bitrate_kbps,
                        process_cpu_pct_x10: Some(500),
                    })
                }
            ),
        (any::<u64>(), any::<u64>()).prop_map(|(nonce, sender_ts_us)| ControlMessage::Ping(Ping {
            nonce,
            sender_ts_us
        })),
        (any::<u64>(), any::<u64>())
            .prop_map(|(nonce, echo_ts_us)| ControlMessage::Pong(Pong { nonce, echo_ts_us })),
        (any::<u32>(), 0u16..=1, 0u16..=1, any::<[u8; 16]>()).prop_map(
            |(shape_id, hotspot_x, hotspot_y, bgra)| ControlMessage::CursorShape(CursorShape {
                shape_id,
                width: 2,
                height: 2,
                hotspot_x,
                hotspot_y,
                blend_mode: racc_proto::CursorBlendMode::PremultipliedAlpha,
                bgra: bgra.to_vec(),
            })
        ),
        goodbye.prop_map(|reason| ControlMessage::Goodbye(Goodbye { reason })),
        any::<bool>().prop_map(|enabled| ControlMessage::ClipboardSyncControl(
            ClipboardSyncControl { enabled }
        )),
        (any::<u16>(), 0u8..=4, any::<bool>()).prop_map(|(epoch, reason, downshift)| {
            let (from_height, to_height, from_bitrate_bps, to_bitrate_bps) = if downshift {
                (720, 480, 3_500_000, 1_500_000)
            } else {
                (720, 720, 3_500_000, 3_150_000)
            };
            let reason = if downshift {
                match reason {
                    0 => QualityAdjustmentReason::Loss,
                    1 => QualityAdjustmentReason::RttInflation,
                    2 => QualityAdjustmentReason::QueueOverflow,
                    3 => QualityAdjustmentReason::Stable,
                    _ => QualityAdjustmentReason::Preference,
                }
            } else {
                QualityAdjustmentReason::BitrateTrim
            };
            ControlMessage::QualityAdjustment(QualityAdjustment {
                epoch,
                reason,
                from_height,
                to_height,
                from_bitrate_bps,
                to_bitrate_bps,
            })
        }),
        (
            any::<u16>(),
            0u16..=1000,
            0u16..=1000,
            0u32..=MAX_VIEWER_REPORT_DURATION_MS,
            0u32..=MAX_VIEWER_REPORT_DURATION_MS,
            0u32..=MAX_VIEWER_REPORT_DROPPED_FRAMES
        )
            .prop_map(
                |(
                    epoch,
                    loss_permille,
                    frame_loss_permille,
                    rtt_ms,
                    decode_ms_p95,
                    dropped_frames,
                )| ControlMessage::ViewerReport(ViewerReport {
                    epoch,
                    loss_permille,
                    frame_loss_permille,
                    rtt_ms,
                    decode_ms_p95,
                    dropped_frames
                })
            ),
    ]
}

fn invoke_all_decoders(bytes: &[u8]) {
    let _ = parse_video_datagram(bytes);
    let _ = parse_cursor_datagram(bytes);
    let _ = decode_control_frame(bytes);
    let _ = ControlMessage::decode_body(bytes);
    let _ = Hello::decode_payload(bytes);
    let _ = HelloAck::decode_payload(bytes);
    let _ = TopologyAnnounce::decode_payload(bytes);
    let _ = SwitchMonitor::decode_payload(bytes);
    let _ = StreamReset::decode_payload(bytes);
    let _ = SetQuality::decode_payload(bytes);
    let _ = RequestKeyframe::decode_payload(bytes);
    let _ = PauseVideo::decode_payload(bytes);
    let _ = ResumeVideo::decode_payload(bytes);
    let _ = InputEvent::decode_payload(bytes);
    let _ = ClipboardUpdate::decode_payload(bytes);
    let _ = StatsReport::decode_payload(bytes);
    let _ = Ping::decode_payload(bytes);
    let _ = Pong::decode_payload(bytes);
    let _ = CursorShape::decode_payload(bytes);
    let _ = Goodbye::decode_payload(bytes);
    let _ = ViewerReport::decode_payload(bytes);
    let _ = ClipboardSyncControl::decode_payload(bytes);
    let _ = QualityAdjustment::decode_payload(bytes);
    let mut decoder = FrameDecoder::new();
    let _ = decoder.feed(bytes, |_| {});
    assert!(decoder.buffer_capacity() <= MAX_CONTROL_FRAME_BYTES + 4);
}

proptest! {
    #[test]
    fn generated_valid_control_messages_round_trip(message in arb_control_message()) {
        let mut encoded = Vec::new();
        message.encode_frame(&mut encoded).unwrap();
        let (decoded, consumed) = decode_control_frame(&encoded).unwrap();
        prop_assert_eq!(decoded, message);
        prop_assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn arbitrary_bytes_never_panic_any_decoder(bytes in prop::collection::vec(any::<u8>(), 0..4097)) {
        invoke_all_decoders(&bytes);
    }

    #[test]
    fn mutations_of_valid_frames_never_panic(
        message in arb_control_message(),
        flip_index in any::<usize>(),
        flip_mask in any::<u8>(),
        truncate_to in 0usize..4096,
        insertion in prop::collection::vec(any::<u8>(), 0..8),
    ) {
        let mut mutated = Vec::new();
        message.encode_frame(&mut mutated).unwrap();
        let index = flip_index % mutated.len().max(1);
        if let Some(byte) = mutated.get_mut(index) {
            *byte ^= flip_mask;
        }
        let current_len = mutated.len();
        mutated.truncate(truncate_to.min(current_len));
        let insert_at = flip_index % (mutated.len() + 1);
        mutated.splice(insert_at..insert_at, insertion);
        invoke_all_decoders(&mutated);
    }
    #[test]
    fn incremental_decoder_handles_random_chunks(
        message in arb_control_message(),
        chunk_sizes in prop::collection::vec(1usize..64, 1..64),
    ) {
        let mut encoded = Vec::new();
        message.encode_frame(&mut encoded).unwrap();
        let mut decoder = FrameDecoder::new();
        let mut decoded = Vec::new();
        let mut offset = 0usize;
        let mut size_index = 0usize;
        while offset < encoded.len() {
            let size = chunk_sizes[size_index % chunk_sizes.len()];
            let end = offset.saturating_add(size).min(encoded.len());
            decoder.feed(&encoded[offset..end], |item| decoded.push(item)).unwrap();
            assert!(decoder.buffer_capacity() <= MAX_CONTROL_FRAME_BYTES + 4);
            offset = end;
            size_index += 1;
        }
        prop_assert_eq!(decoded, vec![message]);
    }
}
fn raw_video(
    flags: u8,
    reserved: u8,
    version: u8,
    kind: u8,
    index: u16,
    count: u16,
    payload: &[u8],
) -> Vec<u8> {
    let mut bytes = vec![version, kind, flags, reserved];
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&index.to_le_bytes());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn frame_body(message: &ControlMessage) -> Vec<u8> {
    let mut frame = Vec::new();
    message.encode_frame(&mut frame).unwrap();
    frame[4..].to_vec()
}

#[test]
fn video_validation_rejects_malformed_datagrams() {
    assert_eq!(
        parse_video_datagram(&raw_video(0, 0, PROTOCOL_VERSION, 1, 0, 0, &[1])),
        Err(ProtoError::InvalidValue)
    );
    assert_eq!(
        parse_video_datagram(&raw_video(
            VIDEO_FLAG_LAST_FRAGMENT,
            0,
            PROTOCOL_VERSION,
            1,
            1,
            1,
            &[1]
        )),
        Err(ProtoError::InvalidValue)
    );
    assert_eq!(
        parse_video_datagram(&raw_video(0, 0, PROTOCOL_VERSION, 1, 0, 1, &[1])),
        Err(ProtoError::InvalidValue)
    );
    assert_eq!(
        parse_video_datagram(&raw_video(0, 1, PROTOCOL_VERSION, 1, 0, 1, &[1])),
        Err(ProtoError::InvalidValue)
    );
    assert_eq!(
        parse_video_datagram(&raw_video(0x80, 0, PROTOCOL_VERSION, 1, 0, 1, &[1])),
        Err(ProtoError::InvalidValue)
    );
    assert_eq!(
        parse_video_datagram(&raw_video(0, 0, 0, 1, 0, 1, &[1])),
        Err(ProtoError::UnsupportedVersion)
    );
    assert_eq!(
        parse_video_datagram(&raw_video(0, 0, PROTOCOL_VERSION, 9, 0, 1, &[1])),
        Err(ProtoError::UnknownKind)
    );
    assert_eq!(
        parse_video_datagram(&raw_video(
            VIDEO_FLAG_LAST_FRAGMENT,
            0,
            PROTOCOL_VERSION,
            1,
            0,
            1,
            &[]
        )),
        Err(ProtoError::InvalidValue)
    );
    assert_eq!(
        parse_video_datagram(&vec![0; MAX_DATAGRAM + 1]),
        Err(ProtoError::TooLarge)
    );

    let max_fragments = raw_video(
        VIDEO_FLAG_LAST_FRAGMENT,
        0,
        PROTOCOL_VERSION,
        1,
        1023,
        1024,
        &[1],
    );
    assert!(matches!(
        parse_video_datagram(&max_fragments),
        Ok(VideoDatagram::Video { .. })
    ));
    assert_eq!(MAX_FRAGMENTS_PER_FRAME, 1024);
    assert_eq!(
        parse_video_datagram(&raw_video(
            VIDEO_FLAG_LAST_FRAGMENT,
            0,
            PROTOCOL_VERSION,
            1,
            1024,
            1025,
            &[1]
        )),
        Err(ProtoError::InvalidValue)
    );
    let oversized_payload = vec![1; 1183];
    assert_eq!(
        parse_video_datagram(&raw_video(
            VIDEO_FLAG_LAST_FRAGMENT,
            0,
            PROTOCOL_VERSION,
            1,
            0,
            1,
            &oversized_payload
        )),
        Err(ProtoError::TooLarge)
    );
}

#[test]
fn cursor_validation_rejects_flags_booleans_and_wrong_lengths() {
    let mut valid = Vec::new();
    racc_proto::encode_cursor_datagram(
        CursorUpdate {
            epoch: 1,
            shape_id: 2,
            x: -1,
            y: 3,
            visible: false,
        },
        &mut valid,
    );
    let mut bad_flags = valid.clone();
    bad_flags[2] = 1;
    assert_eq!(
        parse_cursor_datagram(&bad_flags),
        Err(ProtoError::InvalidValue)
    );
    let mut bad_reserved = valid.clone();
    bad_reserved[3] = 1;
    assert_eq!(
        parse_cursor_datagram(&bad_reserved),
        Err(ProtoError::InvalidValue)
    );
    let mut bad_bool = valid.clone();
    bad_bool[18] = 2;
    assert_eq!(
        parse_cursor_datagram(&bad_bool),
        Err(ProtoError::InvalidValue)
    );
    assert_eq!(
        parse_cursor_datagram(&valid[..18]),
        Err(ProtoError::Truncated)
    );
    let mut extra = valid;
    extra.push(0);
    assert_eq!(
        parse_cursor_datagram(&extra),
        Err(ProtoError::TrailingBytes)
    );
}

#[test]
fn control_length_utf8_enums_bits_and_boolean_validation() {
    assert_eq!(
        decode_control_frame(&[0, 0, 0, 0]),
        Err(ProtoError::InvalidValue)
    );
    let too_long = u32::try_from(MAX_CONTROL_FRAME_BYTES + 1)
        .unwrap()
        .to_le_bytes();
    assert_eq!(decode_control_frame(&too_long), Err(ProtoError::TooLarge));

    let mut clipboard = vec![11, 0, 0, 0, 0, 0, CLIPBOARD_LOGICAL_CLOCK_VERSION];
    clipboard.extend_from_slice(&0u64.to_le_bytes());
    clipboard.push(1);
    clipboard.extend_from_slice(
        &u32::try_from(MAX_CLIPBOARD_BYTES + 1)
            .unwrap()
            .to_le_bytes(),
    );
    assert_eq!(
        ControlMessage::decode_body(&clipboard),
        Err(ProtoError::TooLarge)
    );
    assert_eq!(
        ControlMessage::decode_body(&[1, 0, 129]),
        Err(ProtoError::TooLarge)
    );
    let mut invalid_text = vec![11, 0, 0, 0, 0, 0, CLIPBOARD_LOGICAL_CLOCK_VERSION];
    invalid_text.extend_from_slice(&0u64.to_le_bytes());
    invalid_text.push(1);
    invalid_text.extend_from_slice(&1u32.to_le_bytes());
    invalid_text.push(0xff);
    assert_eq!(
        ControlMessage::decode_body(&invalid_text),
        Err(ProtoError::InvalidUtf8)
    );

    let mut unsupported_clock = vec![11, 0, 0, 0, 0, 0, 2];
    unsupported_clock.extend_from_slice(&0u64.to_le_bytes());
    unsupported_clock.push(1);
    unsupported_clock.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        ControlMessage::decode_body(&unsupported_clock),
        Err(ProtoError::InvalidValue)
    );

    let mut oversized_clock = vec![11, 0, 0, 0, 0, 0, CLIPBOARD_LOGICAL_CLOCK_VERSION];
    oversized_clock.extend_from_slice(&(MAX_CLIPBOARD_LOGICAL_CLOCK + 1).to_le_bytes());
    oversized_clock.push(1);
    oversized_clock.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        ControlMessage::decode_body(&oversized_clock),
        Err(ProtoError::InvalidValue)
    );

    let mut bad_os = frame_body(&ControlMessage::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
        device_name: String::new(),
        os: OsType::Unknown,
        app_version: String::new(),
        video_udp_port: 1,
        codecs: 0,
        max_height: 0,
        features: 0,
    }));
    bad_os[3] = 99;
    assert_eq!(
        ControlMessage::decode_body(&bad_os),
        Err(ProtoError::InvalidValue)
    );

    let mut bad_codec_bits = frame_body(&ControlMessage::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
        device_name: String::new(),
        os: OsType::Unknown,
        app_version: String::new(),
        video_udp_port: 1,
        codecs: 0,
        max_height: 0,
        features: 0,
    }));
    bad_codec_bits[9] = 2;
    assert_eq!(
        ControlMessage::decode_body(&bad_codec_bits),
        Err(ProtoError::InvalidValue)
    );

    let mut bad_quality = frame_body(&ControlMessage::SetQuality(SetQuality {
        max_height: 480,
        bitrate_hint_kbps: 0,
    }));
    bad_quality[1..3].copy_from_slice(&481u16.to_le_bytes());
    assert_eq!(
        ControlMessage::decode_body(&bad_quality),
        Err(ProtoError::InvalidValue)
    );

    let mut bad_input_tag = frame_body(&ControlMessage::InputEvent(InputEvent {
        epoch: 1,
        display_id: 2,
        event: InputEventKind::MouseMoveAbs { u: 0, v: 0 },
    }));
    bad_input_tag[7] = 99;
    assert_eq!(
        ControlMessage::decode_body(&bad_input_tag),
        Err(ProtoError::InvalidValue)
    );
    let mut bad_button = frame_body(&ControlMessage::InputEvent(InputEvent {
        epoch: 1,
        display_id: 2,
        event: InputEventKind::MouseButton {
            button: 1,
            pressed: false,
        },
    }));
    bad_button[8] = 6;
    assert_eq!(
        ControlMessage::decode_body(&bad_button),
        Err(ProtoError::InvalidValue)
    );
    bad_button[8] = 1;
    bad_button[9] = 2;
    assert_eq!(
        ControlMessage::decode_body(&bad_button),
        Err(ProtoError::InvalidValue)
    );

    let mut bad_key = frame_body(&ControlMessage::InputEvent(InputEvent {
        epoch: 1,
        display_id: 2,
        event: InputEventKind::Key {
            hid_usage: 4,
            pressed: false,
            modifiers: 0,
        },
    }));
    bad_key[10] = 2;
    assert_eq!(
        ControlMessage::decode_body(&bad_key),
        Err(ProtoError::InvalidValue)
    );
    bad_key[10] = 0;
    bad_key[11] = 0x10;
    assert_eq!(
        ControlMessage::decode_body(&bad_key),
        Err(ProtoError::InvalidValue)
    );

    let mut bad_clip_origin = vec![11, 0, 0, 0, 0, 2, 1];
    bad_clip_origin.extend_from_slice(&0u64.to_le_bytes());
    bad_clip_origin.push(1);
    bad_clip_origin.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        ControlMessage::decode_body(&bad_clip_origin),
        Err(ProtoError::InvalidValue)
    );
    bad_clip_origin[5] = 0;
    bad_clip_origin[6] = 2;
    assert_eq!(
        ControlMessage::decode_body(&bad_clip_origin),
        Err(ProtoError::InvalidValue)
    );

    let mut bad_ack = frame_body(&ControlMessage::HelloAck(HelloAck {
        protocol_version: PROTOCOL_VERSION,
        status: HelloStatus::Ok,
        device_name: String::new(),
        os: OsType::Unknown,
        app_version: String::new(),
        codecs: 0,
        max_height: 0,
        features: 0,
        host_cpu_cores: 1,
    }));
    bad_ack[2] = 99;
    assert_eq!(
        ControlMessage::decode_body(&bad_ack),
        Err(ProtoError::InvalidValue)
    );

    let mut bad_features = frame_body(&ControlMessage::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
        device_name: String::new(),
        os: OsType::Unknown,
        app_version: String::new(),
        video_udp_port: 1,
        codecs: 0,
        max_height: 0,
        features: 0,
    }));
    bad_features[13] = 2;
    assert_eq!(
        ControlMessage::decode_body(&bad_features),
        Err(ProtoError::InvalidValue)
    );
    let mut bad_stream = frame_body(&ControlMessage::StreamReset(StreamReset {
        req_id: 1,
        epoch: 1,
        codec: StreamCodec::H264,
        width: 1,
        height: 1,
        fps: 1,
        topology_rev: 1,
        display_id: 1,
        status: StreamStatus::Ok,
    }));
    bad_stream[7] = 9;
    assert_eq!(
        ControlMessage::decode_body(&bad_stream),
        Err(ProtoError::InvalidValue)
    );

    let mut bad_stats = frame_body(&ControlMessage::StatsReport(StatsReport {
        host_cpu_pct_x10: 0,
        capture_backend: CaptureBackend::Unknown,
        encoder: Encoder::Unknown,
        width: 0,
        height: 0,
        display_refresh_mhz: 0,
        target_bitrate_kbps: 0,
        actual_bitrate_kbps: 0,
        process_cpu_pct_x10: None,
    }));
    bad_stats[3] = 9;
    assert_eq!(
        ControlMessage::decode_body(&bad_stats),
        Err(ProtoError::InvalidValue)
    );

    bad_stream[7] = 1;
    bad_stream[21] = 99;
    assert_eq!(
        ControlMessage::decode_body(&bad_stream),
        Err(ProtoError::InvalidValue)
    );

    let mut bad_encoder = frame_body(&ControlMessage::StatsReport(StatsReport {
        host_cpu_pct_x10: 0,
        capture_backend: CaptureBackend::Unknown,
        encoder: Encoder::Unknown,
        width: 0,
        height: 0,
        display_refresh_mhz: 0,
        target_bitrate_kbps: 0,
        actual_bitrate_kbps: 0,
        process_cpu_pct_x10: None,
    }));
    bad_encoder[4] = 99;
    assert_eq!(
        ControlMessage::decode_body(&bad_encoder),
        Err(ProtoError::InvalidValue)
    );

    let mut bad_cpu = frame_body(&ControlMessage::StatsReport(StatsReport {
        host_cpu_pct_x10: 0,
        capture_backend: CaptureBackend::Unknown,
        encoder: Encoder::Unknown,
        width: 0,
        height: 0,
        display_refresh_mhz: 0,
        target_bitrate_kbps: 0,
        actual_bitrate_kbps: 0,
        process_cpu_pct_x10: None,
    }));
    bad_cpu[1..3].copy_from_slice(&1001u16.to_le_bytes());
    assert_eq!(
        ControlMessage::decode_body(&bad_cpu),
        Err(ProtoError::InvalidValue)
    );
    let mut bad_process_cpu = frame_body(&ControlMessage::StatsReport(StatsReport {
        host_cpu_pct_x10: 0,
        capture_backend: CaptureBackend::Unknown,
        encoder: Encoder::Unknown,
        width: 0,
        height: 0,
        display_refresh_mhz: 0,
        target_bitrate_kbps: 0,
        actual_bitrate_kbps: 0,
        process_cpu_pct_x10: None,
    }));
    bad_process_cpu[21..23].copy_from_slice(&1001u16.to_le_bytes());
    assert_eq!(
        ControlMessage::decode_body(&bad_process_cpu),
        Err(ProtoError::InvalidValue)
    );
    let mut bad_host_event = frame_body(&ControlMessage::HostEventReport(HostEventReport {
        kind: HostEventKind::CaptureLost,
    }));
    bad_host_event[1] = 5;
    assert_eq!(
        ControlMessage::decode_body(&bad_host_event),
        Err(ProtoError::InvalidValue)
    );
    assert_eq!(
        ControlMessage::decode_body(&[18]),
        Err(ProtoError::Truncated)
    );
    assert_eq!(
        ControlMessage::decode_body(&[18, 2]),
        Err(ProtoError::InvalidValue)
    );

    let mut bad_goodbye = frame_body(&ControlMessage::Goodbye(Goodbye {
        reason: GoodbyeReason::Normal,
    }));
    bad_goodbye[1] = 9;
    assert_eq!(
        ControlMessage::decode_body(&bad_goodbye),
        Err(ProtoError::InvalidValue)
    );
    assert_eq!(
        ControlMessage::decode_body(&[255]),
        Err(ProtoError::UnknownMessageType)
    );
}

#[test]
fn topology_and_cursor_shape_bounds_are_checked() {
    let mut topology = frame_body(&ControlMessage::TopologyAnnounce(TopologyAnnounce {
        topology_rev: 1,
        active_display_id: 1,
        displays: vec![DisplayInfo {
            display_id: 1,
            name: String::new(),
            x: 0,
            y: 0,
            width_px: 1,
            height_px: 1,
            scale_milli: 1000,
            refresh_mhz: 60000,
            flags: 0,
        }],
    }));
    let display_start = 1 + 4 + 4 + 1;
    let zero_width_start = display_start + 4 + 1 + 4 + 4;
    topology[zero_width_start..zero_width_start + 4].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        ControlMessage::decode_body(&topology),
        Err(ProtoError::InvalidValue)
    );

    let two_displays = ControlMessage::TopologyAnnounce(TopologyAnnounce {
        topology_rev: 1,
        active_display_id: 1,
        displays: vec![
            DisplayInfo {
                display_id: 1,
                name: String::new(),
                x: 0,
                y: 0,
                width_px: 1,
                height_px: 1,
                scale_milli: 1,
                refresh_mhz: 1,
                flags: 0,
            },
            DisplayInfo {
                display_id: 2,
                name: String::new(),
                x: 0,
                y: 0,
                width_px: 1,
                height_px: 1,
                scale_milli: 1,
                refresh_mhz: 1,
                flags: 0,
            },
        ],
    });
    let mut duplicate_ids = frame_body(&two_displays);
    duplicate_ids[display_start + 28..display_start + 32].copy_from_slice(&1u32.to_le_bytes());
    assert_eq!(
        ControlMessage::decode_body(&duplicate_ids),
        Err(ProtoError::InvalidValue)
    );

    let mut too_many = vec![3];
    too_many.extend_from_slice(&0u32.to_le_bytes());
    too_many.extend_from_slice(&0u32.to_le_bytes());
    too_many.push((MAX_DISPLAYS + 1) as u8);
    assert_eq!(
        ControlMessage::decode_body(&too_many),
        Err(ProtoError::TooLarge)
    );

    let bad_flags = ControlMessage::TopologyAnnounce(TopologyAnnounce {
        topology_rev: 1,
        active_display_id: 1,
        displays: vec![DisplayInfo {
            display_id: 1,
            name: String::new(),
            x: 0,
            y: 0,
            width_px: 1,
            height_px: 1,
            scale_milli: 1,
            refresh_mhz: 1,
            flags: 0x10,
        }],
    });
    let mut encoded = Vec::new();
    assert_eq!(
        bad_flags.encode_frame(&mut encoded),
        Err(ProtoError::InvalidValue)
    );

    let bad_dim = ControlMessage::CursorShape(CursorShape {
        shape_id: 1,
        width: 129,
        height: 1,
        hotspot_x: 0,
        hotspot_y: 0,
        blend_mode: racc_proto::CursorBlendMode::PremultipliedAlpha,
        bgra: vec![0; 4],
    });
    assert_eq!(
        bad_dim.encode_frame(&mut Vec::new()),
        Err(ProtoError::InvalidValue)
    );
    let bad_size = ControlMessage::CursorShape(CursorShape {
        shape_id: 1,
        width: 2,
        height: 2,
        hotspot_x: 0,
        hotspot_y: 0,
        blend_mode: racc_proto::CursorBlendMode::PremultipliedAlpha,
        bgra: vec![0; 15],
    });
    assert_eq!(
        bad_size.encode_frame(&mut Vec::new()),
        Err(ProtoError::InvalidValue)
    );

    let mut oversized_dims = vec![15];
    oversized_dims.extend_from_slice(&1u32.to_le_bytes());
    oversized_dims.extend_from_slice(&u16::MAX.to_le_bytes());
    oversized_dims.extend_from_slice(&u16::MAX.to_le_bytes());
    oversized_dims.extend_from_slice(&0u16.to_le_bytes());
    oversized_dims.extend_from_slice(&0u16.to_le_bytes());
    oversized_dims.push(0);
    assert_eq!(
        ControlMessage::decode_body(&oversized_dims),
        Err(ProtoError::InvalidValue)
    );

    let mut short_bitmap = vec![15];
    short_bitmap.extend_from_slice(&1u32.to_le_bytes());
    short_bitmap.extend_from_slice(&2u16.to_le_bytes());
    short_bitmap.extend_from_slice(&2u16.to_le_bytes());
    short_bitmap.extend_from_slice(&0u16.to_le_bytes());
    short_bitmap.extend_from_slice(&0u16.to_le_bytes());
    short_bitmap.push(0);
    short_bitmap.extend_from_slice(&[0; 15]);
    assert_eq!(
        ControlMessage::decode_body(&short_bitmap),
        Err(ProtoError::Truncated)
    );

    let mut long_bitmap = short_bitmap.clone();
    long_bitmap.extend_from_slice(&[0; 2]);
    assert_eq!(
        ControlMessage::decode_body(&long_bitmap),
        Err(ProtoError::TrailingBytes)
    );

    let mut bad_blend_mode = vec![15];
    bad_blend_mode.extend_from_slice(&1u32.to_le_bytes());
    bad_blend_mode.extend_from_slice(&1u16.to_le_bytes());
    bad_blend_mode.extend_from_slice(&1u16.to_le_bytes());
    bad_blend_mode.extend_from_slice(&0u16.to_le_bytes());
    bad_blend_mode.extend_from_slice(&0u16.to_le_bytes());
    bad_blend_mode.push(3);
    bad_blend_mode.extend_from_slice(&[0; 4]);
    assert_eq!(
        ControlMessage::decode_body(&bad_blend_mode),
        Err(ProtoError::InvalidValue)
    );

    let bad_hotspot = ControlMessage::CursorShape(CursorShape {
        shape_id: 1,
        width: 1,
        height: 1,
        hotspot_x: 1,
        hotspot_y: 0,
        blend_mode: racc_proto::CursorBlendMode::PremultipliedAlpha,
        bgra: vec![0; 4],
    });
    assert_eq!(
        bad_hotspot.encode_frame(&mut Vec::new()),
        Err(ProtoError::InvalidValue)
    );
}

#[test]
fn incremental_decoder_handles_arbitrary_chunk_boundaries_and_bounds_memory() {
    let messages = [
        ControlMessage::Ping(Ping {
            nonce: 1,
            sender_ts_us: 2,
        }),
        ControlMessage::PauseVideo(PauseVideo),
        ControlMessage::Pong(Pong {
            nonce: 3,
            echo_ts_us: 4,
        }),
    ];
    let mut stream = Vec::new();
    for message in &messages {
        message.encode_frame(&mut stream).unwrap();
    }

    let mut bytewise = FrameDecoder::new();
    let mut bytewise_messages = Vec::new();
    for byte in &stream {
        bytewise
            .feed(core::slice::from_ref(byte), |message| {
                bytewise_messages.push(message)
            })
            .unwrap();
        assert!(bytewise.buffer_capacity() <= MAX_CONTROL_FRAME_BYTES + 4);
    }
    assert_eq!(bytewise_messages, messages);

    let mut chunked = FrameDecoder::new();
    let mut chunked_messages = Vec::new();
    let sizes = [3usize, 1, 8, 2, 5, 11];
    let mut offset = 0usize;
    let mut size_index = 0usize;
    while offset < stream.len() {
        let end = (offset + sizes[size_index % sizes.len()]).min(stream.len());
        chunked
            .feed(&stream[offset..end], |message| {
                chunked_messages.push(message)
            })
            .unwrap();
        offset = end;
        size_index += 1;
    }
    let mut whole = FrameDecoder::new();
    let mut whole_messages = Vec::new();
    whole
        .feed(&stream, |message| whole_messages.push(message))
        .unwrap();
    assert_eq!(whole_messages, messages);
    assert_eq!(chunked_messages, whole_messages);
    assert_eq!(chunked_messages, messages);

    let too_long = u32::try_from(MAX_CONTROL_FRAME_BYTES + 1)
        .unwrap()
        .to_le_bytes();
    let mut decoder = FrameDecoder::new();
    let mut zero_length_decoder = FrameDecoder::new();
    assert_eq!(
        zero_length_decoder.feed(&[0; 4], |_| {}),
        Err(ProtoError::InvalidValue)
    );

    let mut partway_decoder = FrameDecoder::new();
    let mut prior_messages = Vec::new();
    let mut first_frame = Vec::new();
    messages[0].encode_frame(&mut first_frame).unwrap();
    partway_decoder
        .feed(&first_frame, |message| prior_messages.push(message))
        .unwrap();
    assert_eq!(prior_messages, vec![messages[0].clone()]);
    assert_eq!(decoder.feed(&too_long, |_| {}), Err(ProtoError::TooLarge));
    assert_eq!(decoder.buffered_len(), 0);
    assert_eq!(
        partway_decoder.feed(&too_long, |_| {}),
        Err(ProtoError::TooLarge)
    );

    let max_length = u32::try_from(MAX_CONTROL_FRAME_BYTES)
        .unwrap()
        .to_le_bytes();
    decoder.feed(&max_length, |_| {}).unwrap();
    assert_eq!(decoder.buffered_len(), 4);
    assert!(decoder.buffer_capacity() <= MAX_CONTROL_FRAME_BYTES + 4);
}

#[test]
fn viewer_report_rejects_out_of_range_feedback() {
    let valid = ControlMessage::ViewerReport(ViewerReport {
        epoch: 1,
        loss_permille: 1,
        frame_loss_permille: 2,
        rtt_ms: 3,
        decode_ms_p95: 4,
        dropped_frames: 5,
    });
    let mut loss = frame_body(&valid);
    loss[3..5].copy_from_slice(&1001u16.to_le_bytes());
    assert_eq!(
        ControlMessage::decode_body(&loss),
        Err(ProtoError::InvalidValue)
    );
    let mut frame_loss = frame_body(&valid);
    frame_loss[5..7].copy_from_slice(&1001u16.to_le_bytes());
    assert_eq!(
        ControlMessage::decode_body(&frame_loss),
        Err(ProtoError::InvalidValue)
    );
    let mut rtt = frame_body(&valid);
    rtt[7..11].copy_from_slice(&(MAX_VIEWER_REPORT_DURATION_MS + 1).to_le_bytes());
    assert_eq!(
        ControlMessage::decode_body(&rtt),
        Err(ProtoError::InvalidValue)
    );
    let mut decode = frame_body(&valid);
    decode[11..15].copy_from_slice(&(MAX_VIEWER_REPORT_DURATION_MS + 1).to_le_bytes());
    assert_eq!(
        ControlMessage::decode_body(&decode),
        Err(ProtoError::InvalidValue)
    );
    let mut dropped = frame_body(&valid);
    dropped[15..19].copy_from_slice(&(MAX_VIEWER_REPORT_DROPPED_FRAMES + 1).to_le_bytes());
    assert_eq!(
        ControlMessage::decode_body(&dropped),
        Err(ProtoError::InvalidValue)
    );
}

#[test]
fn encoders_reject_out_of_range_clock_and_viewer_report_values() {
    let mut encoded = Vec::new();
    let invalid_clock_version = ControlMessage::ClipboardUpdate(ClipboardUpdate {
        seq: 1,
        origin: ClipboardOrigin::Viewer,
        logical_clock: LogicalClock {
            version: CLIPBOARD_LOGICAL_CLOCK_VERSION + 1,
            counter: 0,
        },
        text: String::new(),
    });
    assert_eq!(
        invalid_clock_version.encode_frame(&mut encoded),
        Err(ProtoError::InvalidValue)
    );

    let invalid_clock_counter = ControlMessage::ClipboardUpdate(ClipboardUpdate {
        seq: 1,
        origin: ClipboardOrigin::Viewer,
        logical_clock: LogicalClock {
            version: CLIPBOARD_LOGICAL_CLOCK_VERSION,
            counter: MAX_CLIPBOARD_LOGICAL_CLOCK + 1,
        },
        text: String::new(),
    });
    assert_eq!(
        invalid_clock_counter.encode_frame(&mut encoded),
        Err(ProtoError::InvalidValue)
    );
    assert!(LogicalClock::new(MAX_CLIPBOARD_LOGICAL_CLOCK + 1).is_err());

    let invalid_report = ControlMessage::ViewerReport(ViewerReport {
        epoch: 1,
        loss_permille: 1001,
        frame_loss_permille: 0,
        rtt_ms: 0,
        decode_ms_p95: 0,
        dropped_frames: 0,
    });
    assert_eq!(
        invalid_report.encode_frame(&mut encoded),
        Err(ProtoError::InvalidValue)
    );
}

#[test]
fn control_message_truncations_are_rejected() {
    for message in [
        ControlMessage::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            device_name: "x".into(),
            os: OsType::Unknown,
            app_version: "v".into(),
            video_udp_port: 1,
            codecs: 1,
            max_height: 480,
            features: 1,
        }),
        ControlMessage::Ping(Ping {
            nonce: 1,
            sender_ts_us: 2,
        }),
        ControlMessage::PauseVideo(PauseVideo),
        ControlMessage::CursorShape(CursorShape {
            shape_id: 1,
            width: 1,
            height: 1,
            hotspot_x: 0,
            hotspot_y: 0,
            blend_mode: racc_proto::CursorBlendMode::PremultipliedAlpha,
            bgra: vec![0; 4],
        }),
    ] {
        let encoded = {
            let mut value = Vec::new();
            message.encode_frame(&mut value).unwrap();
            value
        };
        for end in 0..encoded.len() {
            assert!(decode_control_frame(&encoded[..end]).is_err());
        }
    }
}
