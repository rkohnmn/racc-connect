#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! Local-only host harness for the viewer-less `racc-probe` transport path.

use racc_core::{HostPeerAuthorization, HostRuntime, HostRuntimeEvent};
use racc_net::{
    BindPolicy, ControlConn, ControlError, ControlListener, ControlSettings, VideoSender,
};
use racc_proto::{
    CaptureBackend, ControlMessage, Encoder, HelloAck, HelloStatus, OsType, StatsReport,
    StreamReset, StreamStatus, PROTOCOL_VERSION,
};
use racc_session::{
    CaptureAction, CaptureFailure, EncoderAction, EncoderFailure, HostAction, HostConfig,
    HostPhase, STREAM_FPS,
};
use racc_topology::{Display, DisplayFlags, DisplayId, Topology};
use std::collections::VecDeque;
use std::error::Error;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

const FRAME_INTERVAL_US: u64 = 1_000_000 / STREAM_FPS as u64;
const CONTROL_POLL: Duration = Duration::from_millis(20);
const REPORT_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Default)]
struct StreamState {
    epoch: Option<u16>,
    display_id: Option<u32>,
    paused: bool,
    force_keyframe: bool,
    frame_id: u32,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("host-loopback stopped: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let bind = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let listener = ControlListener::bind(
        bind,
        BindPolicy::TestOnlyLoopback,
        ControlSettings {
            read_timeout: Some(CONTROL_POLL),
            ..ControlSettings::default()
        },
    )?;
    let local_address = listener.local_addr()?;
    let mut runtime = HostRuntime::new_loopback_test(
        capabilities(),
        topology()?,
        local_address.port(),
        HostConfig::default(),
    )?;
    runtime.update_test_loopback_address(local_address)?;
    println!("host-loopback listening on {local_address}");
    println!(
        "Start racc-probe with: 127.0.0.1:{} 127.0.0.1",
        local_address.port()
    );
    println!("Synthetic Annex B transport only; this harness has no capture or real encoder.");

    let (mut control, peer) = listener.accept()?;
    let connection_id = 1;
    let hello = match control.recv()? {
        ControlMessage::Hello(hello) => hello,
        _ => return Err(invalid_data("first control message must be Hello").into()),
    };
    if !peer.ip().is_loopback() {
        return Err(invalid_data("accepted peer was not loopback").into());
    }
    let events = runtime.on_hello(
        connection_id,
        peer,
        &hello,
        HostPeerAuthorization::Approved {
            peer_key: "testkit-loopback-peer".to_owned(),
        },
    )?;
    let mut stream = StreamState::default();
    let start = Instant::now();
    drive_events(events, &mut runtime, &mut control, start, &mut stream)?;

    let local_video = SocketAddr::new(peer.ip(), 0);
    let video_socket = UdpSocket::bind(local_video)?;
    let sender = VideoSender::from_socket(
        video_socket,
        SocketAddr::new(peer.ip(), hello.video_udp_port),
        BindPolicy::TestOnlyLoopback,
        FRAME_INTERVAL_US,
    )?;
    println!(
        "viewer connected from {peer}; UDP target port {}",
        hello.video_udp_port
    );

    let mut connected = true;
    let mut next_frame = Instant::now();
    let mut next_report = Instant::now() + REPORT_INTERVAL;
    let mut report_bytes = 0_u64;
    let mut report_frames = 0_u64;
    loop {
        if connected {
            match control.recv() {
                Ok(message) => {
                    let goodbye = matches!(message, ControlMessage::Goodbye(_));
                    let events =
                        runtime.on_control(connection_id, peer, message, elapsed_us(start))?;
                    drive_events(events, &mut runtime, &mut control, start, &mut stream)?;
                    if goodbye {
                        break;
                    }
                }
                Err(ControlError::Timeout) => {}
                Err(ControlError::Closed) => {
                    connected = false;
                    let events = runtime.on_disconnect(connection_id, elapsed_us(start))?;
                    drive_events(events, &mut runtime, &mut control, start, &mut stream)?;
                    println!(
                        "control connection dropped; stream stops after the 5-second grace period"
                    );
                }
                Err(ControlError::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
                    ) =>
                {
                    connected = false;
                    let events = runtime.on_disconnect(connection_id, elapsed_us(start))?;
                    drive_events(events, &mut runtime, &mut control, start, &mut stream)?;
                    println!(
                        "control connection dropped; stream stops after the 5-second grace period"
                    );
                }
                Err(error) => return Err(Box::new(error)),
            }
        }

        let events = runtime.tick(elapsed_us(start))?;
        drive_events(events, &mut runtime, &mut control, start, &mut stream)?;
        if !connected && runtime.status().session_phase == HostPhase::Closed {
            break;
        }

        let now = Instant::now();
        if !stream.paused && stream.epoch.is_some() && now >= next_frame {
            let epoch = stream.epoch.unwrap_or_default();
            let keyframe = stream.force_keyframe || stream.frame_id == 0;
            let annex_b = synthetic_annex_b_frame(stream.frame_id, keyframe);
            let sent = sender.send_frame(racc_net::SenderFrame {
                epoch,
                frame_id: stream.frame_id,
                keyframe,
                config: keyframe,
                capture_ts_us: elapsed_us(start) as u32,
                bytes: annex_b,
            })?;
            report_bytes = report_bytes.saturating_add(sent.bytes_sent as u64);
            report_frames = report_frames.saturating_add(1);
            stream.frame_id = stream.frame_id.wrapping_add(1);
            stream.force_keyframe = false;
            next_frame = now + Duration::from_micros(FRAME_INTERVAL_US);
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }

        if connected && Instant::now() >= next_report {
            let (width, height) = if stream.display_id.is_some() {
                (1280, 720)
            } else {
                (0, 0)
            };
            let actual_bitrate_kbps =
                u32::try_from(report_bytes.saturating_mul(8) / 1000 / REPORT_INTERVAL.as_secs())
                    .unwrap_or(u32::MAX);
            control.send(&ControlMessage::StatsReport(StatsReport {
                host_cpu_pct_x10: 0,
                capture_backend: CaptureBackend::Unknown,
                encoder: Encoder::Unknown,
                width,
                height,
                display_refresh_mhz: 60_000,
                target_bitrate_kbps: actual_bitrate_kbps,
                actual_bitrate_kbps,
                process_cpu_pct_x10: None,
            }))?;
            println!("synthetic frames/sec={report_frames} bytes/sec={report_bytes}");
            report_bytes = 0;
            report_frames = 0;
            next_report = Instant::now() + REPORT_INTERVAL;
        }
    }
    println!("host-loopback finished");
    Ok(())
}

fn drive_events(
    events: Vec<HostRuntimeEvent>,
    runtime: &mut HostRuntime,
    control: &mut ControlConn,
    start: Instant,
    stream: &mut StreamState,
) -> Result<(), Box<dyn Error>> {
    let mut pending: VecDeque<HostRuntimeEvent> = events.into();
    while let Some(event) = pending.pop_front() {
        match event {
            HostRuntimeEvent::SendControl { message, .. } => control.send(&message)?,
            HostRuntimeEvent::SessionAction { action, .. } => match action {
                HostAction::SendControl(message) => {
                    if let ControlMessage::StreamReset(reset) = &message {
                        observe_reset(reset, stream);
                    }
                    control.send(&message)?;
                }
                HostAction::Capture(CaptureAction::SwitchDisplay { operation_id, .. })
                | HostAction::Capture(CaptureAction::Recreate { operation_id, .. }) => {
                    pending.extend(runtime.on_capture_result(
                        operation_id,
                        Ok::<(), CaptureFailure>(()),
                        elapsed_us(start),
                    )?);
                }
                HostAction::Capture(CaptureAction::Stop) => stream.paused = true,
                HostAction::Encoder(EncoderAction::Configure { operation_id, .. }) => {
                    pending.extend(runtime.on_encoder_configured(
                        operation_id,
                        Ok::<(), EncoderFailure>(()),
                        elapsed_us(start),
                    )?);
                }
                HostAction::Encoder(EncoderAction::SetPaused(paused)) => stream.paused = paused,
                HostAction::Encoder(EncoderAction::ForceKeyframe { epoch }) => {
                    stream.epoch = Some(epoch);
                    stream.force_keyframe = true;
                }
                HostAction::Encoder(EncoderAction::Rebuild { operation_id, .. }) => {
                    pending.extend(runtime.on_encoder_rebuild_result(
                        operation_id,
                        true,
                        elapsed_us(start),
                    )?);
                }
                HostAction::Encoder(EncoderAction::SetBitrate(_))
                | HostAction::Quality(_)
                | HostAction::Event(_) => {}
            },
            HostRuntimeEvent::CloseConnection(_) => {
                return Err(invalid_data("HostRuntime rejected the local test peer").into());
            }
            HostRuntimeEvent::Stopped
            | HostRuntimeEvent::BindAddressChanged(_)
            | HostRuntimeEvent::PendingAuthorization(_)
            | HostRuntimeEvent::QualityPreference(_)
            | HostRuntimeEvent::ViewerFeedback(_)
            | HostRuntimeEvent::QualityDecision(_) => {}
        }
    }
    Ok(())
}

fn observe_reset(reset: &StreamReset, stream: &mut StreamState) {
    stream.epoch = Some(reset.epoch);
    stream.display_id = Some(reset.display_id);
    stream.paused = reset.status != StreamStatus::Ok;
    stream.force_keyframe = reset.status == StreamStatus::Ok;
    stream.frame_id = 0;
}

fn synthetic_annex_b_frame(frame_id: u32, keyframe: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(9);
    bytes.extend_from_slice(&[0, 0, 0, 1, if keyframe { 0x65 } else { 0x41 }]);
    bytes.extend_from_slice(&frame_id.to_le_bytes());
    bytes
}

fn capabilities() -> HelloAck {
    HelloAck {
        protocol_version: PROTOCOL_VERSION,
        status: HelloStatus::Ok,
        device_name: "Racc Loopback Host".to_owned(),
        os: OsType::Windows,
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        codecs: 1,
        max_height: 720,
        features: 0,
        host_cpu_cores: 1,
    }
}

fn topology() -> Result<Topology, Box<dyn Error>> {
    let display_id = DisplayId::try_from(1)?;
    let display = Display::new(
        display_id,
        "Synthetic 1280x720",
        0,
        0,
        1280,
        720,
        1000,
        60_000,
        DisplayFlags::new(true, true, true, false),
    );
    Ok(Topology::new(1, vec![display], Some(display_id))?)
}

fn elapsed_us(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
