#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! Viewer-less Tailscale probe that captures reassembled Annex B access units.

use racc_net::{
    connect_control, BindPolicy, ControlConn, ControlError, ControlSettings, NetError,
    VideoReceiver, VideoTransportEvent,
};
use racc_proto::{
    ControlMessage, DisplayInfo, Goodbye, GoodbyeReason, Hello, HelloStatus, OsType, Ping, Pong,
    RequestKeyframe, SetQuality, StatsReport, StreamStatus, PROTOCOL_VERSION,
};
use racc_session::{ViewerAction, ViewerSession, KEYFRAME_REQUEST_MIN_INTERVAL_US};
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_CAPTURE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_COMMAND_BYTES: usize = 256;
const COMMAND_CAPACITY: usize = 16;
const CONTROL_POLL: Duration = Duration::from_millis(20);
const REPORT_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, PartialEq, Eq)]
struct Config {
    host: SocketAddr,
    local_ip: IpAddr,
    output: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
enum ParseError {
    Help,
    Usage,
    HostAddress,
    LocalAddress,
    NonTailscale,
}

fn parse_args<I>(args: I) -> Result<Config, ParseError>
where
    I: IntoIterator<Item = String>,
{
    let args: Vec<String> = args.into_iter().collect();
    if args.as_slice() == ["--help"] || args.as_slice() == ["-h"] {
        return Err(ParseError::Help);
    }
    if !(2..=3).contains(&args.len()) {
        return Err(ParseError::Usage);
    }
    let host = args[0]
        .parse::<SocketAddr>()
        .map_err(|_| ParseError::HostAddress)?;
    let local_ip = args[1]
        .parse::<IpAddr>()
        .map_err(|_| ParseError::LocalAddress)?;
    if host.port() == 0 {
        return Err(ParseError::HostAddress);
    }
    if racc_net::validate_bind_addr(host.ip(), bind_policy()).is_err()
        || racc_net::validate_bind_addr(local_ip, bind_policy()).is_err()
    {
        return Err(ParseError::NonTailscale);
    }
    Ok(Config {
        host,
        local_ip,
        output: args.get(2).map(PathBuf::from),
    })
}

fn bind_policy() -> BindPolicy {
    #[cfg(feature = "probe-loopback")]
    {
        BindPolicy::TestOnlyLoopback
    }
    #[cfg(not(feature = "probe-loopback"))]
    {
        BindPolicy::Tailscale
    }
}

fn main() {
    match parse_args(env::args().skip(1)) {
        Ok(config) => {
            if let Err(error) = run(config) {
                eprintln!("probe stopped: {}", bounded_error(&error.to_string()));
                std::process::exit(1);
            }
        }
        Err(ParseError::Help) => print_usage(),
        Err(error) => {
            print_usage();
            eprintln!("invalid arguments: {error:?}");
            std::process::exit(2);
        }
    }
}

fn print_usage() {
    println!(
        "Usage: racc-probe <host-tailscale-ip:control-port> <local-tailscale-ip> [capture.h264]\n\
         Commands: help, list displays, stats, switch <display-id>, pause, resume, quality 480|720|1080|auto, keyframe, quit\n\
         Captures are capped at 512 MiB. The default output is target/probe-captures/."
    );
}

fn run(config: Config) -> Result<(), Box<dyn Error>> {
    let local_video_bind = SocketAddr::new(config.local_ip, 0);
    let receiver = VideoReceiver::bind(local_video_bind, config.host.ip(), 0, bind_policy())?;
    let video_port = receiver.local_addr()?.port();
    let settings = ControlSettings {
        read_timeout: Some(CONTROL_POLL),
        ..ControlSettings::default()
    };
    let mut control = connect_control(config.host, bind_policy(), settings)?;
    let mut session = ViewerSession::new(Hello {
        protocol_version: PROTOCOL_VERSION,
        device_name: local_device_name(),
        os: local_os(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        video_udp_port: video_port,
        codecs: 1,
        max_height: 1080,
        features: 0,
    });
    let start = Instant::now();
    apply_actions(session.on_connect(), &mut session, &mut control, &receiver)?;

    let output_path = config.output.unwrap_or_else(default_capture_path);
    if let Some(parent) = output_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(&output_path)?;
    let mut capture = BoundedCapture::new(BufWriter::new(file), MAX_CAPTURE_BYTES);
    let stop_input = Arc::new(AtomicBool::new(false));
    let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    spawn_stdin_reader(command_tx, Arc::clone(&stop_input));
    println!(
        "Connected to Tailscale peer {}. UDP video port {}; capture {}",
        config.host,
        video_port,
        output_path.display()
    );
    println!("Type help for commands. Capture limit: 512 MiB.");

    let mut stats = ProbeStats::default();
    let mut capture_gate = CaptureGate::default();
    let mut next_report = Instant::now() + REPORT_INTERVAL;
    let mut next_ping = Instant::now() + Duration::from_secs(1);
    let mut next_nonce = 1_u64;
    let mut pending_ping: Option<(u64, Instant)> = None;
    let mut last_keyframe_request_us: Option<u64> = None;
    let mut last_report_frames = 0_u64;
    let mut last_report_at = Instant::now();

    loop {
        match command_rx.try_recv() {
            Ok(command) => {
                if handle_command(
                    &command,
                    &mut session,
                    &mut control,
                    &receiver,
                    start,
                    &stats,
                    capture.bytes_written(),
                )? {
                    break;
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {}
        }

        let now = Instant::now();
        if now >= next_ping {
            let ping = Ping {
                nonce: next_nonce,
                sender_ts_us: elapsed_us(start),
            };
            control.send(&ControlMessage::Ping(ping))?;
            pending_ping = Some((next_nonce, now));
            next_nonce = next_nonce.wrapping_add(1);
            next_ping = now + Duration::from_secs(1);
        }

        match control.recv() {
            Ok(message) => handle_control(
                message,
                &mut session,
                &mut control,
                &receiver,
                &mut stats,
                &mut pending_ping,
                start,
                &mut capture_gate,
            )?,
            Err(ControlError::Timeout) => {}
            Err(error) => {
                let _ = session.on_disconnect(elapsed_us(start));
                return Err(Box::new(error));
            }
        }

        match receiver.recv_event(Duration::from_millis(1)) {
            Ok(VideoTransportEvent::FrameReady(frame)) => {
                let epoch = session.epoch().unwrap_or_default();
                if capture_gate.accepts_frame(epoch, frame.keyframe) {
                    stats.capture_ready = capture_gate.ready();
                    let frame_len = frame.bytes.len();
                    if capture.append(&frame.bytes)? {
                        stats.frames = stats.frames.saturating_add(1);
                        stats.bytes = stats
                            .bytes
                            .saturating_add(u64::try_from(frame_len).unwrap_or(u64::MAX));
                        stats.keyframes = stats.keyframes.saturating_add(u64::from(frame.keyframe));
                        stats.latest_frame_id = Some(frame.frame_id);
                        stats.epoch = Some(epoch);
                        stats.capture_ready = capture_gate.ready();
                    }
                }
            }
            Ok(VideoTransportEvent::NeedKeyframe(epoch)) => {
                capture_gate.on_loss(epoch);
                stats.capture_ready = capture_gate.ready();
                if session.epoch() == Some(epoch) {
                    let now_us = elapsed_us(start);
                    let allowed = last_keyframe_request_us.is_none_or(|previous| {
                        now_us
                            .checked_sub(previous)
                            .is_some_and(|elapsed| elapsed >= KEYFRAME_REQUEST_MIN_INTERVAL_US)
                    });
                    if allowed {
                        control
                            .send(&ControlMessage::RequestKeyframe(RequestKeyframe { epoch }))?;
                        last_keyframe_request_us = Some(now_us);
                    }
                }
            }
            Ok(VideoTransportEvent::Cursor(_)) => {
                stats.cursor_updates = stats.cursor_updates.saturating_add(1);
            }
            Err(NetError::Timeout) => {}
            Err(error) => return Err(Box::new(error)),
        }

        if Instant::now() >= next_report {
            let now = Instant::now();
            let elapsed = now.duration_since(last_report_at).as_secs_f64().max(0.001);
            let fps = (stats.frames.saturating_sub(last_report_frames)) as f64 / elapsed;
            let reassembly = receiver.reassembly_stats()?;
            let host_stats = stats.last_host;
            println!(
                "stats phase={:?} capture_ready={} epoch={} frames={} fps={:.1} keyframes={} bytes={} file={} loss_pkt_pct={:.1} loss_whole_pct={:.1} keyframe_requests={} dropped_incomplete={} rtt_ms={} host_cpu_pct={} host_bitrate_kbps={} cursor={} capped={}",
                session.phase(),
                capture_gate.ready(),
                stats.epoch.map_or_else(|| "-".to_owned(), |value| value.to_string()),
                stats.frames,
                fps,
                stats.keyframes,
                stats.bytes,
                capture.bytes_written(),
                reassembly.loss.packet_loss_fraction * 100.0,
                reassembly.loss.whole_frame_loss_fraction * 100.0,
                reassembly.counters.keyframe_requests,
                reassembly.counters.dropped_incomplete,
                stats.last_rtt_ms.map_or_else(|| "-".to_owned(), |value| value.to_string()),
                host_stats.map_or_else(|| "-".to_owned(), |value| (f32::from(value.host_cpu_pct_x10) / 10.0).to_string()),
                host_stats.map_or_else(|| "-".to_owned(), |value| value.actual_bitrate_kbps.to_string()),
                stats.cursor_updates,
                capture.is_capped(),
            );
            capture.flush()?;
            last_report_frames = stats.frames;
            last_report_at = now;
            next_report = now + REPORT_INTERVAL;
        }
    }

    let _ = control.send(&ControlMessage::Goodbye(Goodbye {
        reason: GoodbyeReason::Normal,
    }));
    stop_input.store(true, Ordering::Release);
    capture.flush()?;
    println!(
        "capture complete: frames={} bytes={} file_bytes={} capped={}",
        stats.frames,
        stats.bytes,
        capture.bytes_written(),
        capture.is_capped()
    );
    Ok(())
}

#[derive(Default)]
struct CaptureGate {
    epoch: Option<u16>,
    awaiting_keyframe: bool,
}

impl CaptureGate {
    fn on_reset(&mut self, epoch: u16, stream_active: bool) {
        self.epoch = stream_active.then_some(epoch);
        self.awaiting_keyframe = true;
    }

    fn on_loss(&mut self, epoch: u16) {
        if self.epoch == Some(epoch) {
            self.awaiting_keyframe = true;
        }
    }

    fn accepts_frame(&mut self, epoch: u16, keyframe: bool) -> bool {
        if self.epoch != Some(epoch) {
            return false;
        }
        if self.awaiting_keyframe {
            if !keyframe {
                return false;
            }
            self.awaiting_keyframe = false;
        }
        true
    }

    fn ready(&self) -> bool {
        self.epoch.is_some() && !self.awaiting_keyframe
    }
}

#[derive(Default)]
struct ProbeStats {
    capture_ready: bool,
    displays: Vec<DisplayInfo>,
    active_display_id: Option<u32>,
    frames: u64,
    bytes: u64,
    keyframes: u64,
    cursor_updates: u64,
    latest_frame_id: Option<u32>,
    epoch: Option<u16>,
    last_rtt_ms: Option<u64>,
    last_host: Option<StatsReport>,
}

struct BoundedCapture<W: Write> {
    output: W,
    byte_limit: u64,
    bytes_written: u64,
    capped: bool,
}

impl<W: Write> BoundedCapture<W> {
    fn new(output: W, byte_limit: u64) -> Self {
        Self {
            output,
            byte_limit,
            bytes_written: 0,
            capped: false,
        }
    }

    fn append(&mut self, annex_b: &[u8]) -> io::Result<bool> {
        let length = u64::try_from(annex_b.len()).unwrap_or(u64::MAX);
        if length > self.byte_limit.saturating_sub(self.bytes_written) {
            self.capped = true;
            return Ok(false);
        }
        self.output.write_all(annex_b)?;
        self.bytes_written = self.bytes_written.saturating_add(length);
        Ok(true)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }

    fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    fn is_capped(&self) -> bool {
        self.capped
    }
}

fn apply_actions(
    actions: Vec<ViewerAction>,
    session: &mut ViewerSession,
    control: &mut ControlConn,
    receiver: &VideoReceiver,
) -> Result<(), Box<dyn Error>> {
    for action in actions {
        match action {
            ViewerAction::SendControl(message) => control.send(&message)?,
            ViewerAction::ResetDecoder { epoch } => receiver.reset_epoch(epoch)?,
            ViewerAction::DisconnectTransport => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "host rejected or closed the viewer session",
                )
                .into());
            }
            ViewerAction::ReconnectTransport => {
                let _ = session.on_disconnect(0);
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "control connection was lost; restart racc-probe to reconnect",
                )
                .into());
            }
            ViewerAction::Event(_) => {}
        }
    }
    Ok(())
}

// This function applies one protocol message across the session, control, receiver, and bounded probe stats.
#[allow(clippy::too_many_arguments)]
fn handle_control(
    message: ControlMessage,
    session: &mut ViewerSession,
    control: &mut ControlConn,
    receiver: &VideoReceiver,
    stats: &mut ProbeStats,
    pending_ping: &mut Option<(u64, Instant)>,
    start: Instant,
    capture_gate: &mut CaptureGate,
) -> Result<(), Box<dyn Error>> {
    match message {
        ControlMessage::HelloAck(ack) => {
            if ack.status != HelloStatus::Ok {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "host did not accept the viewer handshake",
                )
                .into());
            }
            apply_actions(session.on_hello_ack(ack), session, control, receiver)?;
        }
        ControlMessage::TopologyAnnounce(topology) => {
            stats.active_display_id =
                (topology.active_display_id != 0).then_some(topology.active_display_id);
            stats.displays = topology.displays.clone();
            let actions = session.on_topology(topology.clone());
            apply_actions(actions, session, control, receiver)?;
            if topology.active_display_id == 0 {
                if let Some(display) = first_available_display(&topology.displays) {
                    apply_actions(
                        session.switch_display(display, elapsed_us(start)),
                        session,
                        control,
                        receiver,
                    )?;
                }
            }
        }
        ControlMessage::StreamReset(reset) => {
            let epoch = reset.epoch;
            let stream_active = reset.status == StreamStatus::Ok;
            let actions = session.on_stream_reset(reset);
            if actions
                .iter()
                .any(|action| matches!(action, ViewerAction::ResetDecoder { epoch: value } if *value == epoch))
            {
                capture_gate.on_reset(epoch, stream_active);
                stats.capture_ready = capture_gate.ready();
                apply_actions(actions, session, control, receiver)?;
            }
        }
        ControlMessage::StatsReport(report) => {
            stats.last_host = Some(report);
        }
        ControlMessage::Ping(ping) => {
            control.send(&ControlMessage::Pong(Pong {
                nonce: ping.nonce,
                echo_ts_us: ping.sender_ts_us,
            }))?;
        }
        ControlMessage::Pong(pong) => {
            if let Some((nonce, sent_at)) = pending_ping.take() {
                if nonce == pong.nonce {
                    stats.last_rtt_ms =
                        Some(u64::try_from(sent_at.elapsed().as_millis()).unwrap_or(u64::MAX));
                } else {
                    *pending_ping = Some((nonce, sent_at));
                }
            }
        }
        ControlMessage::Goodbye(_) => {
            return Err(
                io::Error::new(io::ErrorKind::ConnectionAborted, "host ended the session").into(),
            );
        }
        ControlMessage::CursorShape(_) => {}
        _ => {}
    }
    Ok(())
}

fn parse_quality(value: &str) -> Option<u16> {
    match value {
        "480" => Some(480),
        "720" => Some(720),
        "1080" => Some(1080),
        "auto" => Some(0),
        _ => None,
    }
}

fn sanitized_display_name(name: &str) -> String {
    name.chars()
        .filter(|character| !character.is_control())
        .take(64)
        .collect()
}

fn first_available_display(displays: &[DisplayInfo]) -> Option<u32> {
    displays
        .iter()
        .find(|display| display.flags & (1 << 2) != 0)
        .map(|display| display.display_id)
}

fn handle_command(
    command: &str,
    session: &mut ViewerSession,
    control: &mut ControlConn,
    receiver: &VideoReceiver,
    start: Instant,
    stats: &ProbeStats,
    capture_bytes: u64,
) -> Result<bool, Box<dyn Error>> {
    let mut parts = command.split_ascii_whitespace();
    match parts.next().unwrap_or_default() {
        "help" => print_usage(),
        "list" if parts.next() == Some("displays") => {
            if stats.displays.is_empty() {
                println!("no displays announced");
            }
            for display in &stats.displays {
                let name = sanitized_display_name(&display.name);
                println!(
                    "display id={} name={} size={}x{} scale_milli={} refresh_mhz={} available={} active={}",
                    display.display_id,
                    name,
                    display.width_px,
                    display.height_px,
                    display.scale_milli,
                    display.refresh_mhz,
                    display.flags & (1 << 2) != 0,
                    stats.active_display_id == Some(display.display_id)
                );
            }
        }
        "list" => println!("usage: list displays"),
        "stats" => {
            let reassembly = receiver.reassembly_stats()?;
            println!(
                "phase={:?} capture_ready={} epoch={:?} display={:?} frames={} bytes={} file_bytes={} latest_frame={:?} loss_pkt_pct={:.1} loss_whole_pct={:.1} keyframe_requests={} rtt_ms={:?}",
                session.phase(),
                stats.capture_ready,
                stats.epoch,
                session.selected_display(),
                stats.frames,
                stats.bytes,
                capture_bytes,
                stats.latest_frame_id,
                reassembly.loss.packet_loss_fraction * 100.0,
                reassembly.loss.whole_frame_loss_fraction * 100.0,
                reassembly.counters.keyframe_requests,
                stats.last_rtt_ms
            );
        }
        "pause" => apply_actions(session.on_pause(), session, control, receiver)?,
        "resume" => apply_actions(session.on_resume(), session, control, receiver)?,
        "quality" => {
            let Some(max_height) = parts.next().and_then(parse_quality) else {
                println!("quality must be 480, 720, 1080, or auto");
                return Ok(false);
            };
            control.send(&ControlMessage::SetQuality(SetQuality {
                max_height,
                bitrate_hint_kbps: 0,
            }))?;
        }
        "keyframe" => {
            if let Some(epoch) = session.epoch() {
                control.send(&ControlMessage::RequestKeyframe(RequestKeyframe { epoch }))?;
            }
        }
        "switch" => {
            let Some(id) = parts.next().and_then(|value| value.parse::<u32>().ok()) else {
                println!("usage: switch <display-id>");
                return Ok(false);
            };
            apply_actions(
                session.switch_display(id, elapsed_us(start)),
                session,
                control,
                receiver,
            )?;
        }
        "quit" => return Ok(true),
        "" => {}
        _ => println!("unknown command; type help"),
    }
    Ok(false)
}

fn spawn_stdin_reader(sender: SyncSender<String>, stop: Arc<AtomicBool>) {
    let _ = thread::Builder::new()
        .name("racc-probe-stdin".to_owned())
        .spawn(move || {
            let stdin = io::stdin();
            let mut input = stdin.lock();
            let mut line = Vec::with_capacity(MAX_COMMAND_BYTES);
            let mut one = [0u8; 1];
            let mut too_long = false;
            while !stop.load(Ordering::Acquire) {
                match input.read(&mut one) {
                    Ok(0) => break,
                    Ok(_) if one[0] == b'\n' => {
                        if !too_long {
                            if let Ok(command) = std::str::from_utf8(&line) {
                                let command = command.trim().to_owned();
                                match sender.try_send(command) {
                                    Ok(()) | Err(TrySendError::Full(_)) => {}
                                    Err(TrySendError::Disconnected(_)) => break,
                                }
                            }
                        }
                        line.clear();
                        too_long = false;
                    }
                    Ok(_) if !too_long && line.len() < MAX_COMMAND_BYTES => line.push(one[0]),
                    Ok(_) => too_long = true,
                    Err(_) => break,
                }
            }
        });
}

fn local_os() -> OsType {
    #[cfg(target_os = "windows")]
    {
        OsType::Windows
    }
    #[cfg(target_os = "macos")]
    {
        OsType::MacOs
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        OsType::Unknown
    }
}

fn local_device_name() -> String {
    let variable = if cfg!(target_os = "windows") {
        "COMPUTERNAME"
    } else {
        "HOSTNAME"
    };
    env::var(variable)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            value
                .chars()
                .filter(|ch| !ch.is_control())
                .take(64)
                .collect()
        })
        .unwrap_or_else(|| "racc-probe".to_owned())
}

fn default_capture_path() -> PathBuf {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("target")
        .join("probe-captures")
        .join(format!("racc-probe-{seconds}-{}.h264", std::process::id()))
}

fn elapsed_us(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn bounded_error(message: &str) -> String {
    message.chars().take(160).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_proto::{
        DisplayInfo, HelloAck, StreamCodec, StreamReset, StreamStatus, TopologyAnnounce,
    };
    use racc_session::{VideoDisposition, ViewerPhase};

    #[test]
    fn capture_gate_requires_a_keyframe_after_reset_and_loss() {
        let mut gate = CaptureGate::default();
        gate.on_reset(4, true);
        assert!(!gate.accepts_frame(4, false));
        assert!(gate.accepts_frame(4, true));
        assert!(gate.ready());
        gate.on_loss(4);
        assert!(!gate.accepts_frame(4, false));
        assert!(gate.accepts_frame(4, true));
        gate.on_reset(5, false);
        assert!(!gate.ready());
        assert!(!gate.accepts_frame(4, true));
    }

    #[test]
    fn quality_command_values_match_the_supported_host_tiers() {
        assert_eq!(parse_quality("480"), Some(480));
        assert_eq!(parse_quality("720"), Some(720));
        assert_eq!(parse_quality("1080"), Some(1080));
        assert_eq!(parse_quality("auto"), Some(0));
        assert_eq!(parse_quality("2160"), None);
    }

    #[test]
    fn display_labels_strip_terminal_control_characters_and_are_bounded() {
        assert_eq!(sanitized_display_name("Main\nDisplay"), "MainDisplay");
        assert_eq!(sanitized_display_name(&"x".repeat(100)).len(), 64);
    }

    #[cfg(not(feature = "probe-loopback"))]
    #[test]
    fn parser_accepts_only_tailscale_host_and_bind_addresses() {
        let parsed = parse_args(["100.64.0.9:47473".to_owned(), "100.64.0.10".to_owned()]);
        assert!(parsed.is_ok());
        assert_eq!(
            parse_args(["192.168.1.4:47473".to_owned(), "100.64.0.10".to_owned()]),
            Err(ParseError::NonTailscale)
        );
        assert_eq!(
            parse_args(["100.64.0.9:47473".to_owned(), "127.0.0.1".to_owned()]),
            Err(ParseError::NonTailscale)
        );
    }

    #[cfg(feature = "probe-loopback")]
    #[test]
    fn loopback_probe_feature_accepts_only_loopback_or_tailscale_addresses() {
        assert!(parse_args(["127.0.0.1:47473".to_owned(), "127.0.0.1".to_owned()]).is_ok());
        assert_eq!(
            parse_args(["192.168.1.2:47473".to_owned(), "127.0.0.1".to_owned()]),
            Err(ParseError::NonTailscale)
        );
    }

    #[test]
    fn capture_refuses_to_exceed_its_byte_limit() {
        let mut capture = BoundedCapture::new(Vec::new(), 4);
        assert!(capture.append(b"123").is_ok_and(|written| written));
        assert!(capture.append(b"45").is_ok_and(|written| !written));
        assert_eq!(capture.bytes_written(), 3);
        assert!(capture.is_capped());
        assert_eq!(capture.output, b"123");
    }

    #[test]
    fn session_reaches_streaming_after_an_annex_b_keyframe_arrives() {
        let hello = Hello {
            protocol_version: PROTOCOL_VERSION,
            device_name: "probe-test".to_owned(),
            os: OsType::Windows,
            app_version: "test".to_owned(),
            video_udp_port: 5000,
            codecs: 1,
            max_height: 1080,
            features: 0,
        };
        let mut session = ViewerSession::new(hello);
        assert!(matches!(
            session.on_connect().as_slice(),
            [ViewerAction::SendControl(ControlMessage::Hello(_))]
        ));
        let actions = session.on_hello_ack(HelloAck {
            protocol_version: PROTOCOL_VERSION,
            status: HelloStatus::Ok,
            device_name: "host".to_owned(),
            os: OsType::Windows,
            app_version: "test".to_owned(),
            codecs: 1,
            max_height: 1080,
            features: 0,
            host_cpu_cores: 4,
        });
        assert!(matches!(session.phase(), ViewerPhase::AwaitingTopology));
        assert_eq!(actions.len(), 1);
        session.on_topology(TopologyAnnounce {
            topology_rev: 1,
            active_display_id: 1,
            displays: vec![DisplayInfo {
                display_id: 1,
                name: "Primary".to_owned(),
                x: 0,
                y: 0,
                width_px: 1280,
                height_px: 720,
                scale_milli: 1000,
                refresh_mhz: 60_000,
                flags: 0b111,
            }],
        });
        let reset = StreamReset {
            req_id: 0,
            epoch: 1,
            codec: StreamCodec::H264,
            width: 1280,
            height: 720,
            fps: 30,
            topology_rev: 1,
            display_id: 1,
            status: StreamStatus::Ok,
        };
        assert!(matches!(
            session.on_stream_reset(reset).as_slice(),
            [ViewerAction::ResetDecoder { epoch: 1 }]
        ));
        assert_eq!(
            session.on_decoded_frame(1, true).0,
            VideoDisposition::Replace
        );
        assert_eq!(session.phase(), ViewerPhase::Streaming);
    }
}
