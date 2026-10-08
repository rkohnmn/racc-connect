//! Logged-in macOS host runtime using ScreenCaptureKit and VideoToolbox.

use crate::clipboard_bridge::HostClipboardBridge;
use crate::control_server::{
    ControlSendHandle, HostAdapterEvent, HostControlServer, TailscaleAllowlistAuthorizer,
};
use crate::cursor_sender::{CursorNetworkTransport, CursorTransport};
use crate::input_worker::{
    route_runtime_input_action, HostInputHandle, HostInputSession, HostInputWorker,
};
use crate::macos::local_ipc::{default_app_data_directory, MacHostIpcHandler, MacLocalIpcServer};
use crate::macos_bounded_log::BoundedMacHostLog;
use crate::macos_host_policy::{
    hidden_cursor_update, measured_bitrate_kbps, stream_dimensions, MAC_HOST_DEFAULT_MAX_HEIGHT,
};
use crate::macos_system_metrics::SystemCpuSampler;
use crate::topology_watch::{changed_topology, TOPOLOGY_POLL_INTERVAL};
use racc_capture::macos::{
    enumerate_displays, screen_recording_access, MacCaptureBackend, MacCaptureConfig,
    MacCaptureError, MacCaptureEvent, MacCaptureNotice, MacDisplay, ScreenRecordingAccess,
};
use racc_clipboard::{
    macos::MacPasteboard,
    macos_poll::{MacPasteboardPoller, PasteboardPollError},
    ClipboardErrorKind, ClipboardRejection, LocalChangeResult, RemoteChangeResult,
};
use racc_core::{HostConnectionId, HostRuntime, HostRuntimeEvent, HostSenderObservation};
use racc_encode::{
    Encoder, EncoderConfig, EncoderInput, VideoToolboxH264Encoder, MAX_ENCODED_PACKET_BYTES,
};
use racc_identity::{allowlist_path, TailscaleClient};
use racc_input::macos::{MacDisplayMap, MacQuartzInputInjector};
use racc_net::{BindPolicy, ControlSettings, SenderFrame, VideoSender, DEFAULT_FRAME_INTERVAL_US};
use racc_proto::{
    CaptureBackend, ControlMessage, Encoder as EncoderKind, HelloAck, HelloStatus, HostEventKind,
    HostEventReport, OsType, StatsReport, StreamStatus,
};
use racc_session::{
    CaptureAction, CaptureFailure, EncoderAction, EncoderFailure, HostAction, HostConfig,
    RecoveryReason, SessionEvent,
};
use racc_topology::{DisplayId, Topology};
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::io::{self, BufRead};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CONTROL_PORT: u16 = racc_identity::DEFAULT_CONTROL_PORT;
const EVENT_QUEUE_CAPACITY: usize = 64;
const HOST_TICK_INTERVAL: Duration = Duration::from_millis(50);
const CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(4);
const RESET_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const STREAM_BPS_480: u32 = 1_500_000;
const STREAM_BPS_720: u32 = 3_500_000;
const STREAM_BPS_1080: u32 = 7_000_000;

/// Starts the Mac host in the current logged-in GUI session. No launchd state is modified.
pub fn run_foreground_host() -> Result<(), Box<dyn Error>> {
    let tailscale = TailscaleClient::system();
    let node = tailscale
        .self_node()?
        .filter(|node| node.online)
        .ok_or_else(|| io::Error::other("Tailscale reports no online local node"))?;
    let bind_ip = tailscale.self_bind_addr()?;
    let config_root = config_root()?;
    let host_log = BoundedMacHostLog::open(config_root.join("logs"))?;
    host_log.record("Mac host startup requested")?;
    let displays = enumerate_displays()?;
    if displays.is_empty() {
        return Err(io::Error::other("macOS reported no active displays").into());
    }
    let topology = Topology::new(
        1,
        displays
            .iter()
            .map(|d| d.captured.display.clone())
            .collect(),
        None,
    )?;
    let display_map = displays
        .into_iter()
        .map(|d| (d.captured.display.id(), d))
        .collect::<HashMap<_, _>>();
    let input_maps = display_map
        .values()
        .map(|display| {
            MacDisplayMap::new(
                display.captured.display.origin(),
                display.captured.display.size(),
                display.points,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !MacQuartzInputInjector::accessibility_trusted() {
        host_log.record("Accessibility permission is missing")?;
        eprintln!("Accessibility permission is missing. Video hosting can continue, but remote input will be ignored until permission is granted in System Settings > Privacy & Security > Accessibility.");
    }
    if screen_recording_access() == ScreenRecordingAccess::Missing {
        host_log.record("Screen Recording permission is missing")?;
        eprintln!("Screen Recording permission is missing. The host listener will stay available, but capture requests will fail visibly until permission is granted in System Settings > Privacy & Security > Screen Recording. No permission prompt was opened automatically.");
    }

    let device_name = node
        .display_name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .map(bounded_name)
        .unwrap_or_else(local_device_name);
    let capabilities = HelloAck {
        protocol_version: racc_proto::PROTOCOL_VERSION,
        status: HelloStatus::Ok,
        device_name,
        os: OsType::MacOs,
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        codecs: 1,
        max_height: MAC_HOST_DEFAULT_MAX_HEIGHT,
        features: racc_proto::FEATURE_TEXT_CLIPBOARD,
        host_cpu_cores: thread::available_parallelism()
            .map(|cores| u8::try_from(cores.get()).unwrap_or(u8::MAX))
            .unwrap_or(1),
    };
    let runtime = Arc::new(Mutex::new(HostRuntime::new(
        capabilities,
        topology.clone(),
        CONTROL_PORT,
        HostConfig::default(),
    )?));
    lock(&runtime).update_tailscale_address(Some(bind_ip))?;

    let input_injector = MacQuartzInputInjector::new(input_maps);
    let input_worker = HostInputWorker::new(input_injector.clone())?;
    let callback_input = input_worker.handle();
    let event_input = callback_input.clone();
    let (event_tx, event_rx) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
    let callback_tx = event_tx.clone();
    let authorizer = Arc::new(Mutex::new(TailscaleAllowlistAuthorizer::open_config_root(
        &config_root,
    )?));
    let hosting_enabled = Arc::new(AtomicBool::new(true));
    let mut server = HostControlServer::bind(
        SocketAddr::new(bind_ip, CONTROL_PORT),
        ControlSettings::default(),
        Arc::clone(&runtime),
        Arc::clone(&authorizer),
        move |event| {
            if let HostAdapterEvent::AuthenticatedMessage {
                connection_id,
                message: ControlMessage::InputEvent(input),
                ..
            } = &event
            {
                let _ = event_input.try_enqueue(*connection_id, *input);
                return;
            }
            if adapter_event_requires_input_release(&event) {
                event_input.request_deactivate_nonblocking();
            }
            let _ = callback_tx.try_send(event);
        },
    )?;
    let control_sender = server.control_sender();
    let ipc_server = MacLocalIpcServer::bind_default()?;
    let mut local_ipc_worker = MacLocalIpcWorker::start(
        ipc_server,
        Arc::clone(&runtime),
        Arc::clone(&authorizer),
        Arc::clone(&hosting_enabled),
    )?;
    let command_rx = start_console_command_reader();
    let mut runner = MacHost::new(
        bind_ip,
        runtime,
        control_sender,
        input_worker,
        callback_input,
        input_injector,
        topology,
        display_map,
    );

    host_log.record("Mac host listener started")?;
    println!(
        "Mac host listening on {}. Allowlist: {}",
        server.local_addr(),
        allowlist_path(&config_root).display()
    );
    println!("Hosting runs only in this logged-in user session. Type `approve <node-id>` to approve a peer, or `stop` to stop the host.");
    println!("Mac streaming is capped at 720p30 until the 2015 Intel Mac completes the sustained higher-tier test. The cursor is included in captured video; separate cursor metadata is disabled to avoid drawing it twice.");

    loop {
        match command_rx.try_recv() {
            Ok(ConsoleCommand::Stop) => break,
            Ok(ConsoleCommand::Approve(peer_key)) => {
                match server.approve_peer(&peer_key, unix_seconds()) {
                    Ok(()) => println!(
                    "Approved {peer_key}. The peer must reconnect before it can open a session."
                ),
                    Err(error) => eprintln!("could not approve peer: {error}"),
                }
            }
            // LaunchAgents have no interactive stdin. EOF closes only this optional
            // console-command channel; the supervised host remains alive until stopped.
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
        }
        match event_rx.recv_timeout(Duration::from_millis(8)) {
            Ok(event) => handle_adapter_event(event, &mut runner)?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if runner.capture_running {
            if let Some(event) = runner.capture.poll_event(CAPTURE_POLL_INTERVAL) {
                handle_capture_event(event, &mut runner)?;
            }
        }
        refresh_topology_if_due(&mut runner)?;
        handle_sender_keyframe_requests(&mut runner);
        poll_host_clipboard(&mut runner);
        send_periodic_stats(&mut runner);
        if runner.last_tick.elapsed() >= HOST_TICK_INTERVAL {
            runner.last_tick = Instant::now();
            let events = lock(&runner.runtime).tick(runner.now_us())?;
            handle_runtime_events(events, false, &mut runner)?;
        }
    }
    host_log.record("Mac host listener stopped")?;
    local_ipc_worker.stop();
    server.stop()?;
    reset_macos_clipboard(&mut runner);
    runner.input_worker.shutdown();
    runner.capture.stop();
    runner.capture_running = false;
    close_video_sender(&mut runner);
    runner.encoder = None;
    runner.active_epoch = None;
    let events = lock(&runner.runtime).stop();
    handle_runtime_events(events, false, &mut runner)?;
    println!("Mac host stopped.");
    Ok(())
}
struct MacLocalIpcWorker {
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MacLocalIpcWorker {
    fn start(
        server: MacLocalIpcServer,
        runtime: Arc<Mutex<HostRuntime>>,
        authorizer: Arc<Mutex<TailscaleAllowlistAuthorizer>>,
        hosting_enabled: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = Arc::clone(&stopping);
        let worker = thread::Builder::new()
            .name("racc-macos-local-ipc".to_owned())
            .spawn(move || {
                let result = server.serve_until(Arc::clone(&worker_stopping), move || {
                    MacHostIpcHandler::new(
                        Arc::clone(&runtime),
                        Arc::clone(&authorizer),
                        Arc::clone(&hosting_enabled),
                    )
                });
                if let Err(error) = result {
                    if !worker_stopping.load(Ordering::Acquire) {
                        eprintln!("local Mac host IPC server stopped: {error}");
                    }
                }
            })?;
        Ok(Self {
            stopping,
            thread: Some(worker),
        })
    }

    fn stop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for MacLocalIpcWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

struct MacHost {
    bind_ip: IpAddr,
    runtime: Arc<Mutex<HostRuntime>>,
    control_sender: ControlSendHandle,
    input_worker: HostInputWorker<MacQuartzInputInjector>,
    input_handle: HostInputHandle,
    input_injector: MacQuartzInputInjector,
    input_topology: Option<Topology>,
    topology: Topology,
    display_map: HashMap<DisplayId, MacDisplay>,
    capture: MacCaptureBackend,
    capture_running: bool,
    capture_display: Option<DisplayId>,
    capture_config: Option<(u32, u32)>,
    capture_path: Option<racc_capture::macos::MacCapturePath>,
    deferred_capture_actions: VecDeque<CaptureAction>,
    video_sender: Option<VideoSender>,
    cursor_transport: Option<CursorNetworkTransport>,
    active_connection_id: Option<HostConnectionId>,
    clipboard: Option<HostClipboardBridge>,
    clipboard_poller: Option<MacPasteboardPoller<MacPasteboard>>,
    clipboard_last_send_attempt_ms: Option<u64>,
    active_epoch: Option<u16>,
    next_frame_id: u32,
    video_paused: bool,
    encoder: Option<VideoToolboxH264Encoder>,
    encoder_config: Option<EncoderConfig>,
    pending_bitrate: Option<u32>,
    started_at: Instant,
    last_tick: Instant,
    last_stats_report: Instant,
    last_stats_bytes_total: u64,
    stats_bytes_total: u64,
    cpu_sampler: SystemCpuSampler,
    last_topology_scan: Instant,
}

impl MacHost {
    fn new(
        bind_ip: IpAddr,
        runtime: Arc<Mutex<HostRuntime>>,
        control_sender: ControlSendHandle,
        input_worker: HostInputWorker<MacQuartzInputInjector>,
        input_handle: HostInputHandle,
        input_injector: MacQuartzInputInjector,
        topology: Topology,
        display_map: HashMap<DisplayId, MacDisplay>,
    ) -> Self {
        Self {
            bind_ip,
            runtime,
            control_sender,
            input_worker,
            input_handle,
            input_injector,
            input_topology: Some(topology.clone()),
            topology,
            display_map,
            capture: MacCaptureBackend::new(MacCaptureConfig::default()),
            capture_running: false,
            capture_display: None,
            capture_config: None,
            capture_path: None,
            deferred_capture_actions: VecDeque::new(),
            video_sender: None,
            cursor_transport: None,
            active_connection_id: None,
            clipboard: None,
            clipboard_poller: None,
            clipboard_last_send_attempt_ms: None,
            active_epoch: None,
            next_frame_id: 0,
            video_paused: true,
            encoder: None,
            encoder_config: None,
            pending_bitrate: None,
            started_at: Instant::now(),
            last_tick: Instant::now(),
            last_stats_report: Instant::now(),
            last_stats_bytes_total: 0,
            stats_bytes_total: 0,
            cpu_sampler: SystemCpuSampler::new(),
            last_topology_scan: Instant::now(),
        }
    }

    fn now_us(&self) -> u64 {
        u64::try_from(self.started_at.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

#[derive(Clone, Debug)]
enum ConsoleCommand {
    Stop,
    Approve(String),
}

fn start_console_command_reader() -> Receiver<ConsoleCommand> {
    let (sender, receiver) = mpsc::sync_channel(8);
    let _ = thread::Builder::new()
        .name("racc-mac-host-console".to_owned())
        .spawn(move || {
            let stdin = io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                let trimmed = line.trim();
                if trimmed == "stop" {
                    let _ = sender.try_send(ConsoleCommand::Stop);
                    break;
                }
                if let Some(peer_key) = trimmed.strip_prefix("approve ") {
                    if !peer_key.is_empty() && peer_key.len() <= 512 {
                        let _ = sender.try_send(ConsoleCommand::Approve(peer_key.to_owned()));
                    }
                }
            }
        });
    receiver
}

fn adapter_event_requires_input_release(event: &HostAdapterEvent) -> bool {
    match event {
        HostAdapterEvent::ViewerAccepted { .. } => true,
        HostAdapterEvent::Runtime { event, .. } => match event {
            HostRuntimeEvent::CloseConnection(_) | HostRuntimeEvent::Stopped => true,
            HostRuntimeEvent::SessionAction { action, .. } => matches!(
                action,
                HostAction::Capture(_)
                    | HostAction::Encoder(
                        EncoderAction::Configure { .. }
                            | EncoderAction::Rebuild { .. }
                            | EncoderAction::SetPaused(true)
                    )
                    | HostAction::SendControl(
                        ControlMessage::StreamReset(_) | ControlMessage::TopologyAnnounce(_)
                    )
                    | HostAction::Event(
                        SessionEvent::Reconnecting
                            | SessionEvent::CaptureLost(_)
                            | SessionEvent::EncoderFailed
                            | SessionEvent::ControlTimedOut
                            | SessionEvent::SessionEnded
                    )
            ),
            _ => false,
        },
        _ => false,
    }
}

fn handle_adapter_event(
    event: HostAdapterEvent,
    runner: &mut MacHost,
) -> Result<(), Box<dyn Error>> {
    match event {
        HostAdapterEvent::ViewerAccepted {
            connection_id,
            remote_addr,
            video_udp_port,
        } => {
            reset_macos_clipboard(runner);
            runner.input_handle.deactivate();
            runner.active_connection_id = Some(connection_id);
            runner.last_stats_bytes_total = 0;
            runner.stats_bytes_total = 0;
            runner.last_stats_report = Instant::now();
            runner.clipboard = Some(HostClipboardBridge::new(connection_id));
            runner.active_epoch = None;
            close_video_sender(runner);
            if video_udp_port == 0 {
                eprintln!(
                    "authorized viewer announced an invalid zero UDP port; video is disabled"
                );
                return Ok(());
            }
            let target = SocketAddr::new(remote_addr.ip(), video_udp_port);
            runner.cursor_transport = match CursorNetworkTransport::bind(
                runner.bind_ip,
                target,
                runner.control_sender.clone(),
            ) {
                Ok(transport) => Some(transport),
                Err(error) => {
                    eprintln!("cursor metadata path unavailable; captured video excludes the pointer and the remote cursor will be hidden: {error}");
                    None
                }
            };
            match VideoSender::bind(
                SocketAddr::new(runner.bind_ip, 0),
                target,
                BindPolicy::Tailscale,
                DEFAULT_FRAME_INTERVAL_US,
            ) {
                Ok(sender) => {
                    runner.video_sender = Some(sender);
                    runner.video_paused = false;
                    println!("Authorized viewer {connection_id} from {remote_addr}; Tailscale UDP video path is ready.");
                    let actions = std::mem::take(&mut runner.deferred_capture_actions);
                    for action in actions {
                        execute_capture_action(action, runner)?;
                    }
                }
                Err(error) => {
                    eprintln!("could not bind Tailscale video sender: {error}");
                    let actions = std::mem::take(&mut runner.deferred_capture_actions);
                    for action in actions {
                        fail_capture_action(action, runner)?;
                    }
                }
            }
        }
        HostAdapterEvent::Runtime { event, .. } => {
            handle_runtime_events(vec![event], true, runner)?
        }
        HostAdapterEvent::AuthenticatedMessage {
            connection_id,
            message: ControlMessage::ClipboardSyncControl(control),
            ..
        } => handle_host_clipboard_control(connection_id, control.enabled, runner),
        HostAdapterEvent::AuthenticatedMessage {
            connection_id,
            message: ControlMessage::ClipboardUpdate(update),
            ..
        } => handle_host_clipboard_update(connection_id, update, runner),
        HostAdapterEvent::AuthenticatedMessage {
            connection_id,
            message: ControlMessage::InputEvent(input),
            ..
        } => {
            let _ = runner.input_handle.try_enqueue(connection_id, input);
        }
        HostAdapterEvent::AuthenticatedMessage { .. } => {}
        HostAdapterEvent::IdentityCheckFailed { remote_addr, error } => {
            eprintln!("Tailscale identity check failed for {remote_addr}: {error}")
        }
        HostAdapterEvent::ServerError { message } => eprintln!("control server error: {message}"),
    }
    Ok(())
}

fn handle_runtime_events(
    events: Vec<HostRuntimeEvent>,
    controls_already_sent: bool,
    runner: &mut MacHost,
) -> Result<(), Box<dyn Error>> {
    for event in events {
        reset_clipboard_if_session_ended(&event, runner);
        match event {
            HostRuntimeEvent::SessionAction { connection_id, action } => match action {
                HostAction::SendControl(message) => {
                    if let Some(connection_id) = connection_id {
                        handle_control_send(connection_id, message, controls_already_sent, runner)?;
                    }
                }
                HostAction::Capture(action) => execute_capture_action(action, runner)?,
                HostAction::Encoder(action) => match action {
                    EncoderAction::SetPaused(paused) => {
                        if let Some(connection_id) = connection_id {
                            report_host_event(
                                connection_id,
                                if paused { HostEventKind::Paused } else { HostEventKind::Resumed },
                                runner,
                            );
                        }
                        execute_encoder_action(EncoderAction::SetPaused(paused), runner)?;
                    }
                    action => execute_encoder_action(action, runner)?,
                },
                HostAction::Quality(_) => {}
                HostAction::InjectInput(input) => {
                    route_runtime_input_action(&runner.input_handle, connection_id, input);
                }
                HostAction::Event(event) => {
                    if let Some(connection_id) = connection_id {
                        let kind = match event {
                            SessionEvent::CaptureLost(_) => Some(HostEventKind::CaptureLost),
                            SessionEvent::RecoverySucceeded => Some(HostEventKind::CaptureRecovered),
                            SessionEvent::EncoderFallbackToSoftware => Some(HostEventKind::EncoderFallback),
                            _ => None,
                        };
                        if let Some(kind) = kind {
                            report_host_event(connection_id, kind, runner);
                        }
                    }
                    if matches!(event, SessionEvent::CaptureLost(_) | SessionEvent::EncoderFailed | SessionEvent::ControlTimedOut | SessionEvent::SessionEnded) {
                        runner.active_epoch = None;
                        runner.input_handle.deactivate();
                    }
                    if event == SessionEvent::EncoderFailed {
                        eprintln!("VideoToolbox hardware encoding failed; native Mac frames have no software fallback.");
                        runner.encoder = None;
                        runner.capture.stop();
                        runner.capture_running = false;
                    }
                }
            },
            HostRuntimeEvent::PendingAuthorization(peer) => eprintln!(
                "Peer approval required; connection remains rejected: {} ({}). Type `approve {}` to approve.",
                peer.label, peer.peer_key, peer.peer_key
            ),
            HostRuntimeEvent::SendControl { connection_id, message } => {
                handle_control_send(connection_id, message, controls_already_sent, runner)?;
            }
            HostRuntimeEvent::CloseConnection(connection_id) => {
                if runner.active_connection_id == Some(connection_id) {
                    runner.active_epoch = None;
                    runner.input_handle.deactivate();
                }
            }
            HostRuntimeEvent::BindAddressChanged(_) | HostRuntimeEvent::QualityPreference(_) | HostRuntimeEvent::ViewerFeedback(_) | HostRuntimeEvent::QualityDecision(_) | HostRuntimeEvent::Stopped => {}
        }
    }
    Ok(())
}

fn report_host_event(connection_id: HostConnectionId, kind: HostEventKind, runner: &mut MacHost) {
    if let Err(error) = runner.control_sender.send(
        connection_id,
        ControlMessage::HostEventReport(HostEventReport { kind }),
    ) {
        eprintln!("Mac host lifecycle telemetry could not be queued: {error}");
    }
}

fn handle_host_clipboard_control(
    connection_id: HostConnectionId,
    enabled: bool,
    runner: &mut MacHost,
) {
    if runner.active_connection_id != Some(connection_id) {
        return;
    }
    if enabled {
        if runner
            .clipboard
            .as_ref()
            .map_or(true, |bridge| bridge.connection_id() != connection_id)
        {
            runner.clipboard = Some(HostClipboardBridge::new(connection_id));
        }
        let mut poller = MacPasteboardPoller::new(MacPasteboard::new());
        let now_ms = u64::try_from(runner.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        if let Err(error) = poller.start_session(now_ms) {
            eprintln!("Mac clipboard poller could not start: {error:?}");
            return;
        }
        let Some(bridge) = runner.clipboard.as_mut() else {
            return;
        };
        if let Err(error) = bridge.set_enabled(true) {
            eprintln!("Mac clipboard session could not be enabled: {error:?}");
            return;
        }
        runner.clipboard_poller = Some(poller);
        runner.clipboard_last_send_attempt_ms = None;
    } else {
        if let Some(bridge) = runner
            .clipboard
            .as_mut()
            .filter(|bridge| bridge.connection_id() == connection_id)
        {
            if let Err(error) = bridge.set_enabled(false) {
                eprintln!("Mac clipboard session could not be disabled: {error:?}");
            }
        }
        if let Some(mut poller) = runner.clipboard_poller.take() {
            poller.end_session();
        }
        runner.clipboard_last_send_attempt_ms = None;
    }
}

fn handle_host_clipboard_update(
    connection_id: HostConnectionId,
    update: racc_proto::ClipboardUpdate,
    runner: &mut MacHost,
) {
    let Some(bridge) = runner
        .clipboard
        .as_mut()
        .filter(|bridge| bridge.connection_id() == connection_id)
    else {
        return;
    };
    match bridge.receive_remote(update) {
        Ok(RemoteChangeResult::Apply(applied)) => {
            let Some(poller) = runner.clipboard_poller.as_mut() else {
                return;
            };
            if let Err(error) = poller.apply_remote(&applied.bytes) {
                eprintln!("Mac clipboard could not apply remote text: {error:?}");
            }
        }
        Ok(RemoteChangeResult::Rejected(ClipboardRejection::TooLarge { bytes, limit })) => {
            eprintln!("remote Mac clipboard text rejected ({bytes} bytes exceeds {limit} bytes)");
        }
        Ok(RemoteChangeResult::Rejected(ClipboardRejection::InvalidUtf8)) => {
            eprintln!("remote Mac clipboard text rejected (invalid UTF-8)");
        }
        Ok(RemoteChangeResult::Ignored(_)) => {}
        Err(crate::clipboard_bridge::HostClipboardError::UnsupportedLogicalClockVersion) => {
            eprintln!("remote Mac clipboard update rejected (unsupported logical-clock version)");
        }
        Err(crate::clipboard_bridge::HostClipboardError::LogicalClockOutOfRange) => {
            eprintln!("remote Mac clipboard update rejected (logical clock out of range)");
        }
        Err(crate::clipboard_bridge::HostClipboardError::SessionStartFailed) => {
            eprintln!("Mac clipboard policy session could not start");
        }
        Err(crate::clipboard_bridge::HostClipboardError::InvalidUtf8) => {
            eprintln!("Mac clipboard policy returned invalid UTF-8");
        }
    }
}

fn poll_host_clipboard(runner: &mut MacHost) {
    let Some(connection_id) = runner.active_connection_id else {
        return;
    };
    if !runner
        .clipboard
        .as_ref()
        .is_some_and(|bridge| bridge.connection_id() == connection_id && bridge.is_enabled())
    {
        return;
    }
    let now_ms = u64::try_from(runner.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let Some(poller) = runner.clipboard_poller.as_mut() else {
        return;
    };
    let result = poller.poll(now_ms);
    match result {
        Ok(Some(bytes)) => {
            if let Some(bridge) = runner.clipboard.as_mut() {
                match bridge.local_change(&bytes) {
                    LocalChangeResult::Rejected(ClipboardRejection::TooLarge { .. }) => {
                        eprintln!("local Mac clipboard text rejected (exceeds 512 KiB)");
                    }
                    LocalChangeResult::Rejected(ClipboardRejection::InvalidUtf8) => {
                        eprintln!("local Mac clipboard text rejected (invalid UTF-8)");
                    }
                    _ => {}
                }
            }
        }
        Ok(None) => {}
        Err(PasteboardPollError::Adapter(error)) if error.kind == ClipboardErrorKind::TooLarge => {
            eprintln!("local Mac clipboard text rejected (exceeds 512 KiB)");
        }
        Err(PasteboardPollError::Adapter(error)) => {
            eprintln!("Mac clipboard text read failed: {error}");
        }
        Err(PasteboardPollError::TooLarge { bytes, maximum }) => {
            eprintln!("local Mac clipboard text rejected ({bytes} bytes exceeds {maximum} bytes)");
        }
        Err(PasteboardPollError::InvalidUtf8) => {
            eprintln!("local Mac clipboard text rejected (invalid UTF-8)");
        }
    }

    if runner
        .clipboard_last_send_attempt_ms
        .is_some_and(|last| now_ms < last.saturating_add(50))
    {
        return;
    }
    let Some(bridge) = runner
        .clipboard
        .as_mut()
        .filter(|bridge| bridge.connection_id() == connection_id && bridge.is_enabled())
    else {
        return;
    };
    let message = match bridge.next_outbound(now_ms) {
        Ok(Some(message)) => message,
        Ok(None) => return,
        Err(error) => {
            eprintln!("Mac clipboard update could not be encoded: {error:?}");
            return;
        }
    };
    runner.clipboard_last_send_attempt_ms = Some(now_ms);
    match runner.control_sender.send(connection_id, message) {
        Ok(()) => bridge.confirm_outbound_queued(),
        Err(error) => eprintln!("Mac clipboard update could not be queued: {error}"),
    }
}

fn reset_macos_clipboard(runner: &mut MacHost) {
    if let Some(bridge) = runner.clipboard.as_mut() {
        bridge.end_session();
    }
    if let Some(mut poller) = runner.clipboard_poller.take() {
        poller.end_session();
    }
    runner.clipboard = None;
    runner.clipboard_last_send_attempt_ms = None;
}

fn reset_clipboard_if_session_ended(event: &HostRuntimeEvent, runner: &mut MacHost) {
    let (connection_id, ended) = match event {
        HostRuntimeEvent::CloseConnection(connection_id) => (Some(*connection_id), true),
        HostRuntimeEvent::SessionAction {
            connection_id,
            action: HostAction::Event(SessionEvent::Reconnecting | SessionEvent::SessionEnded),
        } => (*connection_id, true),
        _ => (None, false),
    };
    if ended
        && runner
            .clipboard
            .as_ref()
            .is_some_and(|bridge| connection_id.map_or(true, |id| bridge.connection_id() == id))
    {
        reset_macos_clipboard(runner);
    }
}

fn handle_control_send(
    connection_id: HostConnectionId,
    message: ControlMessage,
    controls_already_sent: bool,
    runner: &mut MacHost,
) -> Result<(), Box<dyn Error>> {
    match message {
        ControlMessage::TopologyAnnounce(announced) => {
            match Topology::from_proto(&announced) {
                Ok(topology) => {
                    let current_display = lock(&runner.runtime).status().current_display;
                    let input_session = runner
                        .active_epoch
                        .zip(current_display)
                        .filter(|_| runner.active_connection_id == Some(connection_id))
                        .and_then(|(epoch, display_id)| {
                            let previous = runner.input_topology.as_ref()?;
                            HostInputSession::new(
                                connection_id,
                                epoch,
                                display_id,
                                previous.clone(),
                            )
                            .with_updated_topology(previous, topology.clone())
                        });
                    if let Some(session) = input_session {
                        runner.input_handle.activate_session(session);
                    } else {
                        runner.input_handle.deactivate();
                    }
                    runner.input_topology = Some(topology);
                }
                Err(error) => {
                    runner.input_handle.deactivate();
                    runner.input_topology = None;
                    eprintln!(
                        "host topology announcement could not be used for input mapping: {error}"
                    );
                }
            }
            if !controls_already_sent {
                let _ = runner
                    .control_sender
                    .send(connection_id, ControlMessage::TopologyAnnounce(announced));
            }
        }
        ControlMessage::StreamReset(reset) => {
            runner.input_handle.deactivate();
            let delivered = controls_already_sent
                || runner
                    .control_sender
                    .send_confirmed(
                        connection_id,
                        ControlMessage::StreamReset(reset),
                        RESET_WRITE_TIMEOUT,
                    )
                    .is_ok();
            if delivered && reset.status == StreamStatus::Ok {
                runner.active_epoch = Some(reset.epoch);
                runner.next_frame_id = 0;
                runner.video_paused = false;
                if runner.active_connection_id == Some(connection_id) {
                    if let Some(topology) = runner
                        .input_topology
                        .as_ref()
                        .filter(|t| t.revision() == reset.topology_rev)
                    {
                        if let Some(display_id) = DisplayId::new(reset.display_id) {
                            if topology
                                .displays()
                                .iter()
                                .any(|d| d.id() == display_id && d.flags().available())
                            {
                                runner.input_handle.activate_session(HostInputSession::new(
                                    connection_id,
                                    reset.epoch,
                                    display_id,
                                    topology.clone(),
                                ));
                            }
                        }
                    }
                }
                if let Some(transport) = runner.cursor_transport.as_mut() {
                    if let Err(error) =
                        transport.send_cursor_datagram(hidden_cursor_update(reset.epoch))
                    {
                        eprintln!("could not send hidden Mac cursor metadata: {error}");
                    }
                }
            } else {
                runner.active_epoch = None;
                runner.video_paused = true;
            }
        }
        other => {
            if !controls_already_sent {
                let _ = runner.control_sender.send(connection_id, other);
            }
        }
    }
    Ok(())
}

fn execute_capture_action(
    action: CaptureAction,
    runner: &mut MacHost,
) -> Result<(), Box<dyn Error>> {
    if !matches!(&action, CaptureAction::Stop) && runner.video_sender.is_none() {
        runner.deferred_capture_actions.push_back(action);
        return Ok(());
    }
    runner.input_handle.deactivate();
    runner.active_epoch = None;
    runner.encoder = None;
    match action {
        CaptureAction::SwitchDisplay {
            operation_id, to, ..
        } => {
            let result = restart_capture(to, None, runner);
            if let Err(error) = &result {
                eprintln!("Mac display capture could not start: {error}");
            }
            let result = result.map_err(|error| failure_for_capture(&error));
            let events =
                lock(&runner.runtime).on_capture_result(operation_id, result, runner.now_us())?;
            handle_runtime_events(events, false, runner)?;
        }
        CaptureAction::Recreate {
            operation_id,
            display_id,
        } => {
            let result = restart_capture(display_id, None, runner);
            if let Err(error) = &result {
                eprintln!("Mac capture recovery attempt failed: {error}");
            }
            let result = result.map_err(|error| failure_for_capture(&error));
            let events =
                lock(&runner.runtime).on_capture_result(operation_id, result, runner.now_us())?;
            handle_runtime_events(events, false, runner)?;
        }
        CaptureAction::Stop => {
            runner.capture.stop();
            runner.capture_running = false;
            runner.capture_display = None;
            runner.capture_config = None;
            runner.encoder_config = None;
            runner.video_paused = true;
            close_video_sender(runner);
        }
    }
    Ok(())
}

fn fail_capture_action(action: CaptureAction, runner: &mut MacHost) -> Result<(), Box<dyn Error>> {
    let operation_id = match action {
        CaptureAction::SwitchDisplay { operation_id, .. }
        | CaptureAction::Recreate { operation_id, .. } => Some(operation_id),
        CaptureAction::Stop => None,
    };
    if let Some(operation_id) = operation_id {
        let events = lock(&runner.runtime).on_capture_result(
            operation_id,
            Err(CaptureFailure::BackendFailure),
            runner.now_us(),
        )?;
        handle_runtime_events(events, false, runner)?;
    }
    Ok(())
}

fn execute_encoder_action(
    action: EncoderAction,
    runner: &mut MacHost,
) -> Result<(), Box<dyn Error>> {
    match action {
        EncoderAction::Configure {
            operation_id,
            display_id,
            width,
            height,
        } => {
            runner.active_epoch = None;
            runner.encoder = None;
            let bitrate = runner
                .pending_bitrate
                .unwrap_or_else(|| bitrate_for_height(height));
            runner.pending_bitrate = Some(bitrate);
            let config = EncoderConfig::new(u32::from(width), u32::from(height), bitrate)?;
            if let Err(error) = ensure_capture_format(display_id, width, height, runner) {
                eprintln!("Mac capture format reconfiguration failed: {error}");
                let reason = if matches!(
                    error.downcast_ref::<MacCaptureError>(),
                    Some(MacCaptureError::PermissionMissing)
                ) {
                    RecoveryReason::PermissionDenied
                } else {
                    RecoveryReason::BackendFailure
                };
                let events = lock(&runner.runtime).on_capture_lost(reason, runner.now_us())?;
                handle_runtime_events(events, false, runner)?;
                return Ok(());
            }
            runner.encoder_config = Some(config);
            match VideoToolboxH264Encoder::new(config) {
                Ok(encoder) => {
                    println!("VideoToolbox H.264 hardware encoder ready at {width}x{height}; low-latency rate control: {}.", if encoder.low_latency_specification_accepted() { "accepted" } else { "unavailable" });
                    runner.encoder_config = Some(config);
                    runner.encoder = Some(encoder);
                    lock(&runner.runtime)
                        .set_encoder_name(Some("VideoToolbox hardware H.264".to_owned()));
                    let events = lock(&runner.runtime).on_encoder_configured(
                        operation_id,
                        Ok(()),
                        runner.now_us(),
                    )?;
                    handle_runtime_events(events, false, runner)?;
                }
                Err(error) => {
                    eprintln!("could not configure VideoToolbox hardware H.264: {error}");
                    let events = lock(&runner.runtime).on_encoder_configured(
                        operation_id,
                        Err(EncoderFailure::ConfigureFailed),
                        runner.now_us(),
                    )?;
                    handle_runtime_events(events, false, runner)?;
                }
            }
        }
        EncoderAction::Rebuild {
            operation_id,
            use_software,
        } => {
            let success = if use_software {
                false
            } else if let Some(config) = runner.encoder_config {
                match VideoToolboxH264Encoder::new(config) {
                    Ok(encoder) => {
                        runner.encoder = Some(encoder);
                        true
                    }
                    Err(error) => {
                        eprintln!("VideoToolbox rebuild failed: {error}");
                        false
                    }
                }
            } else {
                false
            };
            let events = lock(&runner.runtime).on_encoder_rebuild_result(
                operation_id,
                success,
                runner.now_us(),
            )?;
            handle_runtime_events(events, false, runner)?;
        }
        EncoderAction::SetPaused(paused) => {
            runner.video_paused = paused;
            if !paused {
                if let Some(encoder) = runner.encoder.as_mut() {
                    if let Err(error) = encoder.request_keyframe() {
                        eprintln!("could not request resume keyframe: {error}");
                    }
                }
            }
        }
        EncoderAction::SetBitrate(bitrate_bps) => {
            let bitrate = bitrate_bps.clamp(1_000, 20_000_000);
            runner.pending_bitrate = Some(bitrate);
            if let Some(encoder) = runner.encoder.as_mut() {
                if let Err(error) = encoder.set_bitrate(bitrate) {
                    eprintln!("VideoToolbox bitrate adjustment failed: {error}");
                }
                if let Err(error) = encoder.request_keyframe() {
                    eprintln!("encoder did not accept bitrate-change keyframe: {error}");
                }
            }
        }
        EncoderAction::ForceKeyframe { .. } => {
            if let Some(encoder) = runner.encoder.as_mut() {
                if let Err(error) = encoder.request_keyframe() {
                    eprintln!("VideoToolbox did not accept keyframe request: {error}");
                }
            }
        }
    }
    Ok(())
}

fn failure_for_capture(error: &MacCaptureError) -> CaptureFailure {
    match error {
        MacCaptureError::PermissionMissing => CaptureFailure::PermissionDenied,
        MacCaptureError::DisplayUnavailable => CaptureFailure::Unavailable,
        MacCaptureError::AlreadyStarted
        | MacCaptureError::NotStarted
        | MacCaptureError::Platform(_) => CaptureFailure::BackendFailure,
    }
}
fn restart_capture(
    display_id: DisplayId,
    requested: Option<(u16, u16)>,
    runner: &mut MacHost,
) -> Result<(), MacCaptureError> {
    let display = runner
        .display_map
        .get(&display_id)
        .ok_or(MacCaptureError::DisplayUnavailable)?
        .clone();
    let dimensions = requested
        .map(|(width, height)| racc_session::StreamDimensions { width, height })
        .or_else(|| {
            stream_dimensions(
                display.captured.display.size().0,
                display.captured.display.size().1,
            )
        })
        .ok_or(MacCaptureError::DisplayUnavailable)?;
    runner.capture.stop();
    runner.capture_running = false;
    let capture = MacCaptureBackend::new(MacCaptureConfig {
        max_width: u32::from(dimensions.width),
        max_height: u32::from(dimensions.height),
        fps: 30,
    });
    runner.capture = capture;
    runner.capture.start(&display)?;
    runner.capture_running = true;
    runner.capture_display = Some(display_id);
    runner.capture_config = Some((u32::from(dimensions.width), u32::from(dimensions.height)));
    runner.capture_path = None;
    Ok(())
}

fn ensure_capture_format(
    display_id: DisplayId,
    width: u16,
    height: u16,
    runner: &mut MacHost,
) -> Result<(), Box<dyn Error>> {
    let requested = (u32::from(width), u32::from(height));
    if runner.capture_running
        && runner.capture_display == Some(display_id)
        && runner.capture_config == Some(requested)
    {
        return Ok(());
    }
    restart_capture(display_id, Some((width, height)), runner)?;
    Ok(())
}

fn handle_capture_event(
    event: MacCaptureEvent,
    runner: &mut MacHost,
) -> Result<(), Box<dyn Error>> {
    match event {
        MacCaptureEvent::Frame(frame) => dispatch_frame(frame, runner),
        MacCaptureEvent::Lifecycle(MacCaptureNotice::Started { path, display_id }) => {
            runner.capture_path = Some(path);
            println!("Mac screen capture started: {path:?}, native display {display_id}.");
            Ok(())
        }
        MacCaptureEvent::Lifecycle(MacCaptureNotice::AccessLost { display_id }) => {
            eprintln!("Mac screen capture access was lost for native display {display_id}; host recovery is starting.");
            runner.active_epoch = None;
            runner.encoder = None;
            let reason = if screen_recording_access() == ScreenRecordingAccess::Missing {
                RecoveryReason::PermissionDenied
            } else {
                RecoveryReason::AccessLost
            };
            let events = lock(&runner.runtime).on_capture_lost(reason, runner.now_us())?;
            handle_runtime_events(events, false, runner)
        }
        MacCaptureEvent::Lifecycle(MacCaptureNotice::FrameStalled { display_id }) => {
            eprintln!("Mac screen capture produced no callback activity for two seconds on native display {display_id}; host recovery is starting.");
            runner.active_epoch = None;
            runner.encoder = None;
            let events = lock(&runner.runtime)
                .on_capture_lost(RecoveryReason::AccessLost, runner.now_us())?;
            handle_runtime_events(events, false, runner)
        }
        MacCaptureEvent::Lifecycle(MacCaptureNotice::DisplayRemoved { display_id }) => {
            eprintln!("Mac display {display_id} was removed; refreshing host topology.");
            refresh_topology(runner)
        }
        MacCaptureEvent::Lifecycle(MacCaptureNotice::DisplayReconfigured { display_id }) => {
            eprintln!("Mac display {display_id} changed; refreshing host topology.");
            refresh_topology(runner)
        }
        MacCaptureEvent::Lifecycle(MacCaptureNotice::Recovered { display_id }) => {
            println!("Mac capture delivered a frame again for display {display_id}; recovery will use a fresh capture and keyframe.");
            Ok(())
        }
        MacCaptureEvent::Error(message) => {
            eprintln!("Mac capture reported an error: {message}");
            runner.active_epoch = None;
            runner.encoder = None;
            let events = lock(&runner.runtime)
                .on_capture_lost(RecoveryReason::BackendFailure, runner.now_us())?;
            handle_runtime_events(events, false, runner)
        }
    }
}

fn refresh_topology(runner: &mut MacHost) -> Result<(), Box<dyn Error>> {
    let displays = enumerate_displays()?;
    refresh_topology_from_displays(displays, runner)
}

fn refresh_topology_if_due(runner: &mut MacHost) -> Result<(), Box<dyn Error>> {
    if runner.last_topology_scan.elapsed() < TOPOLOGY_POLL_INTERVAL {
        return Ok(());
    }
    runner.last_topology_scan = Instant::now();
    match enumerate_displays() {
        Ok(displays) if !displays.is_empty() => refresh_topology_from_displays(displays, runner),
        Ok(_) => {
            eprintln!("macOS returned no active displays during topology refresh; keeping the last topology.");
            Ok(())
        }
        Err(error) => {
            eprintln!("macOS display topology refresh failed; keeping the last topology: {error}");
            Ok(())
        }
    }
}

fn refresh_topology_from_displays(
    displays: Vec<MacDisplay>,
    runner: &mut MacHost,
) -> Result<(), Box<dyn Error>> {
    if displays.is_empty() {
        return Ok(());
    }
    let changed = changed_topology(
        &runner.topology,
        displays
            .iter()
            .map(|display| display.captured.display.clone())
            .collect(),
    )?;
    runner.display_map = displays
        .into_iter()
        .map(|d| (d.captured.display.id(), d))
        .collect();
    let input_maps = runner
        .display_map
        .values()
        .map(|display| {
            MacDisplayMap::new(
                display.captured.display.origin(),
                display.captured.display.size(),
                display.points,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    runner.input_injector.replace_display_maps(input_maps)?;
    let Some(topology) = changed else {
        return Ok(());
    };
    runner.topology = topology.clone();
    let events = lock(&runner.runtime).on_topology_changed(topology, runner.now_us())?;
    handle_runtime_events(events, false, runner)
}

fn dispatch_frame(
    frame: racc_capture::macos::MacCapturedFrame,
    runner: &mut MacHost,
) -> Result<(), Box<dyn Error>> {
    let Some(epoch) = runner.active_epoch else {
        return Ok(());
    };
    if runner.video_paused {
        return Ok(());
    }
    let (Some(encoder), Some(sender)) = (runner.encoder.as_mut(), runner.video_sender.as_ref())
    else {
        return Ok(());
    };
    let encode_started = Instant::now();
    let encoded = encoder.encode(EncoderInput::MacPixelBuffer(
        frame.pixel_buffer(),
        frame.capture_ts_us,
    ));
    let encoder_lag_ms = encode_started.elapsed().as_millis().min(60_000) as u32;
    let packet = match encoded {
        Ok(packet) => packet,
        Err(error) => {
            eprintln!("VideoToolbox encode failed: {error}");
            runner.active_epoch = None;
            let events = lock(&runner.runtime).on_encoder_failure()?;
            handle_runtime_events(events, false, runner)?;
            return Ok(());
        }
    };
    let Some(packet) = packet else {
        observe_macos_sender(runner, epoch, encoder_lag_ms)?;
        return Ok(());
    };
    if packet.bytes.is_empty() || packet.bytes.len() > MAX_ENCODED_PACKET_BYTES {
        eprintln!("VideoToolbox returned an invalid bounded access unit.");
        observe_macos_sender(runner, epoch, encoder_lag_ms)?;
        return Ok(());
    }
    let config = annex_b_has_sps_and_pps(&packet.bytes);
    let send = sender.send_frame(SenderFrame {
        epoch,
        frame_id: runner.next_frame_id,
        keyframe: packet.keyframe,
        config,
        capture_ts_us: frame.capture_ts_us as u32,
        bytes: packet.bytes,
    });
    match send {
        Ok(metrics) => {
            runner.next_frame_id = runner.next_frame_id.wrapping_add(1);
            runner.stats_bytes_total = runner
                .stats_bytes_total
                .saturating_add(u64::try_from(metrics.bytes_sent).unwrap_or(u64::MAX));
        }
        Err(error) => {
            eprintln!("paced Mac video send failed: {error}");
            if let Some(encoder) = runner.encoder.as_mut() {
                let _ = encoder.request_keyframe();
            }
        }
    }
    observe_macos_sender(runner, epoch, encoder_lag_ms)?;
    Ok(())
}

fn observe_macos_sender(
    runner: &mut MacHost,
    epoch: u16,
    encoder_lag_ms: u32,
) -> Result<(), Box<dyn Error>> {
    let events = lock(&runner.runtime).on_local_sender_observation(
        HostSenderObservation {
            epoch,
            queue_overflow: false,
            encoder_lag_ms: Some(encoder_lag_ms),
        },
        runner.now_us(),
    )?;
    handle_runtime_events(events, false, runner)
}

fn send_periodic_stats(runner: &mut MacHost) {
    if runner.last_stats_report.elapsed() < Duration::from_secs(1) {
        return;
    }
    let now = Instant::now();
    let elapsed_us =
        u64::try_from(runner.last_stats_report.elapsed().as_micros()).unwrap_or(u64::MAX);
    runner.last_stats_report = now;

    let Some(connection_id) = runner.active_connection_id else {
        runner.last_stats_bytes_total = runner.stats_bytes_total;
        return;
    };
    let sent_delta = runner
        .stats_bytes_total
        .saturating_sub(runner.last_stats_bytes_total);
    runner.last_stats_bytes_total = runner.stats_bytes_total;
    let actual_bitrate_kbps = measured_bitrate_kbps(sent_delta, elapsed_us);
    let current_display = lock(&runner.runtime).status().current_display;
    let refresh = current_display
        .and_then(|id| runner.display_map.get(&id))
        .map_or(0, |display| display.captured.display.refresh_mhz());
    let active_config = runner
        .encoder_config
        .as_ref()
        .filter(|_| runner.encoder.is_some());
    let (width, height, target_bitrate_kbps) = active_config.map_or((0, 0, 0), |config| {
        (
            u16::try_from(config.width()).unwrap_or(u16::MAX),
            u16::try_from(config.height()).unwrap_or(u16::MAX),
            config.bitrate_bps() / 1_000,
        )
    });
    let capture_backend = if runner.capture_running {
        match runner.capture_path {
            Some(racc_capture::macos::MacCapturePath::ScreenCaptureKit) => {
                CaptureBackend::ScreenCaptureKit
            }
            Some(racc_capture::macos::MacCapturePath::CGDisplayStream) => {
                CaptureBackend::CgDisplayStream
            }
            None => CaptureBackend::Unknown,
        }
    } else {
        CaptureBackend::Unknown
    };
    let report = StatsReport {
        // Zero is the protocol's unknown or exact-idle value until two valid tick
        // samples are available. The sampler reads aggregate machine-wide counters.
        host_cpu_pct_x10: runner.cpu_sampler.sample_tenths().unwrap_or(0),
        process_cpu_pct_x10: runner.cpu_sampler.process_sample_tenths(),
        capture_backend,
        encoder: if runner.encoder.is_some() {
            EncoderKind::VideoToolbox
        } else {
            EncoderKind::Unknown
        },
        width,
        height,
        display_refresh_mhz: refresh,
        target_bitrate_kbps,
        actual_bitrate_kbps,
    };
    if let Err(error) = runner
        .control_sender
        .send(connection_id, ControlMessage::StatsReport(report))
    {
        eprintln!("Mac host telemetry report could not be queued: {error}");
    }
}

fn handle_sender_keyframe_requests(runner: &mut MacHost) {
    if runner
        .video_sender
        .as_ref()
        .is_some_and(|sender| sender.try_force_keyframe().is_some())
    {
        if let Some(encoder) = runner.encoder.as_mut() {
            if let Err(error) = encoder.request_keyframe() {
                eprintln!("VideoToolbox did not accept transport keyframe request: {error}");
            }
        }
    }
}

fn close_video_sender(runner: &mut MacHost) {
    runner.active_epoch = None;
    runner.video_paused = true;
    runner.cursor_transport = None;
    if let Some(mut sender) = runner.video_sender.take() {
        let _ = sender.close();
    }
}

fn bitrate_for_height(height: u16) -> u32 {
    match height {
        0..=480 => STREAM_BPS_480,
        481..=720 => STREAM_BPS_720,
        _ => STREAM_BPS_1080,
    }
}

fn annex_b_has_sps_and_pps(bytes: &[u8]) -> bool {
    let mut sps = false;
    let mut pps = false;
    let mut offset = 0;
    while offset < bytes.len() {
        let Some((start, prefix)) = find_start_code(bytes, offset) else {
            break;
        };
        let header = start.saturating_add(prefix);
        let next = find_start_code(bytes, header).map_or(bytes.len(), |(index, _)| index);
        if let Some(nal) = bytes.get(header) {
            match nal & 0x1f {
                7 => sps = true,
                8 => pps = true,
                _ => {}
            }
        }
        if sps && pps {
            return true;
        }
        if next <= offset {
            break;
        }
        offset = next;
    }
    false
}

fn find_start_code(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut index = from;
    while index < bytes.len() {
        let tail = bytes.get(index..)?;
        if tail.starts_with(&[0, 0, 0, 1]) {
            return Some((index, 4));
        }
        if tail.starts_with(&[0, 0, 1]) {
            return Some((index, 3));
        }
        index = index.checked_add(1)?;
    }
    None
}

fn config_root() -> Result<PathBuf, io::Error> {
    default_app_data_directory()
}

fn bounded_name(name: &str) -> String {
    let mut result = name.trim().chars().take(128).collect::<String>();
    if result.is_empty() {
        result = "Mac host".to_owned();
    }
    result
}

fn local_device_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .map(|name| bounded_name(&name))
        .unwrap_or_else(|| "Mac host".to_owned())
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
