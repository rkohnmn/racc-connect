//! Windows host runtime shared by the development console and service helper.

use crate::clipboard_bridge::HostClipboardBridge;
use crate::control_server::{
    ControlSendHandle, HostAdapterEvent, HostControlServer, TailscaleAllowlistAuthorizer,
};
use crate::cursor_sender::{CursorDispatch, CursorNetworkTransport, HostCursorDispatcher};
use crate::input_worker::{
    route_runtime_input_action, HostInputHandle, HostInputSession, HostInputWorker,
};
use crate::stream_dispatch::{dispatch_frame, DispatchError, FrameEncoder};
use crate::topology_watch::{changed_topology, TOPOLOGY_POLL_INTERVAL};
use crate::windows::local_ipc::{run_local_ipc_server, WindowsHostIpcHandler};
use crate::windows::SystemCpuSampler;
use racc_capture::windows::WindowsCaptureBackend;
use racc_capture::{CaptureBackend as CaptureBackendTrait, CaptureEvent, CaptureParams, GpuFrame};
use racc_clipboard::windows::{ClipboardListener, WindowsClipboard};
use racc_clipboard::{ClipboardRejection, LocalChangeResult, RemoteChangeResult};
use racc_core::{HostConnectionId, HostRuntime, HostRuntimeEvent, HostSenderObservation};
use racc_encode::windows::MediaFoundationH264Encoder;
use racc_encode::windows_software_fallback::WindowsI420Readback;
use racc_encode::windows_video_processor::WindowsNv12Converter;
use racc_encode::{
    EncodeError, EncodedPacket, Encoder, EncoderConfig, EncoderInput, OpenH264Encoder,
};
use racc_identity::{allowlist_path, TailscaleClient};
use racc_input::windows::WindowsSendInput;
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
use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CONTROL_PORT: u16 = racc_identity::DEFAULT_CONTROL_PORT;
const EVENT_QUEUE_CAPACITY: usize = 8;
const HOST_TICK_INTERVAL: Duration = Duration::from_millis(50);
const CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(4);
const STREAM_RESET_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const UDP_PENDING_FRAME_CAPACITY: usize = 2;

/// Starts a foreground listener and runs host lifecycle actions until `stop` or stdin EOF.
pub fn run_foreground_host() -> Result<(), Box<dyn Error>> {
    run_foreground_host_inner(None)
}

pub fn run_foreground_host_with_stop_event(
    stop_signal: Receiver<()>,
) -> Result<(), Box<dyn Error>> {
    run_foreground_host_inner(Some(stop_signal))
}

fn run_foreground_host_inner(stop_signal: Option<Receiver<()>>) -> Result<(), Box<dyn Error>> {
    let tailscale = TailscaleClient::system();
    let node = tailscale
        .self_node()?
        .filter(|node| node.online)
        .ok_or_else(|| io::Error::other("Tailscale reports no online local node"))?;
    let bind_ip = tailscale.self_bind_addr()?;
    let config_root = config_root()?;
    let displays = WindowsCaptureBackend::new().enumerate_displays()?;
    if displays.is_empty() {
        return Err(io::Error::other("Windows reported no active displays").into());
    }
    let display_refresh_mhz = displays
        .iter()
        .map(|display| (display.display.id(), display.display.refresh_mhz()))
        .collect::<HashMap<_, _>>();
    let topology = Topology::new(
        1,
        displays
            .iter()
            .map(|display| display.display.clone())
            .collect(),
        None,
    )?;
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
        os: OsType::Windows,
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        codecs: 1,
        max_height: 1080,
        features: 1,
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

    let input_worker = HostInputWorker::new(WindowsSendInput::from_environment()?)?;
    let callback_input = input_worker.handle();
    let event_input = callback_input.clone();
    let authorizer = Arc::new(Mutex::new(TailscaleAllowlistAuthorizer::open_config_root(
        &config_root,
    )?));
    let hosting_enabled = Arc::new(AtomicBool::new(true));
    let (event_tx, event_rx) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
    let callback_tx = event_tx.clone();
    let mut server = HostControlServer::bind(
        std::net::SocketAddr::new(bind_ip, CONTROL_PORT),
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
            let _ = callback_tx.send(event);
        },
    )?;
    let control_sender = server.control_sender();
    let mut local_ipc_worker = LocalIpcWorker::start(
        Arc::clone(&runtime),
        Arc::clone(&authorizer),
        Arc::clone(&hosting_enabled),
    )?;
    let command_rx = match stop_signal {
        Some(signal) => start_service_stop_command_reader(signal)?,
        None => start_console_command_reader()?,
    };
    let mut runner = ForegroundHost {
        bind_ip,
        runtime,
        control_sender,
        input_worker,
        input_handle: callback_input,
        cursor_dispatcher: HostCursorDispatcher::new(),
        cursor_transport: None,
        input_topology: None,
        clipboard: None,
        active_connection_id: None,
        display_refresh_mhz,
        topology,
        last_topology_scan: Instant::now(),
        cpu_sampler: SystemCpuSampler::default(),
        last_stats_report: Instant::now(),
        last_stats_bytes_total: 0,
        clipboard_listener: None,
        clipboard_adapter: WindowsClipboard::new(),
        clipboard_last_send_attempt_ms: None,
        capture: WindowsCaptureBackend::new(),
        capture_running: false,
        deferred_capture_actions: VecDeque::new(),
        video_sender: None,
        active_epoch: None,
        next_frame_id: 0,
        video_paused: false,
        pending_encoder: None,
        active_encoder: None,
        encoder_config: None,
        encoder_software: false,
        pending_bitrate: None,
        started_at: Instant::now(),
        last_tick: Instant::now(),
    };

    println!(
        "Foreground host listening on {}. Allowlist: {}",
        server.local_addr(),
        allowlist_path(&config_root).display()
    );
    println!(
        "Approved peers only. Type `approve <node-id>` to approve a pending peer, or `stop` to stop hosting."
    );
    println!("H.264 video uses Media Foundation hardware when available, with an OpenH264 software fallback.");

    loop {
        match command_rx.try_recv() {
            Ok(ConsoleCommand::Stop) | Err(TryRecvError::Disconnected) => break,
            Ok(ConsoleCommand::Approve(peer_key)) => {
                match server.approve_peer(&peer_key, unix_seconds()) {
                    Ok(()) => println!(
                        "Approved {peer_key}. The peer must reconnect before it can open a session."
                    ),
                    Err(error) => eprintln!("could not approve peer: {error}"),
                }
            }
            Err(TryRecvError::Empty) => {}
        }
        match event_rx.recv_timeout(Duration::from_millis(8)) {
            Ok(event) => handle_adapter_event(event, &mut runner)?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        refresh_topology_if_due(&mut runner)?;
        if runner.capture_running {
            match runner.capture.poll_event(CAPTURE_POLL_INTERVAL) {
                Ok(Some(event)) => handle_capture_event(event, &mut runner)?,
                Ok(None) => {}
                Err(error) => {
                    eprintln!("capture poll failed: {error}");
                    runner.capture.stop();
                    runner.capture_running = false;
                    runner.active_epoch = None;
                    runner.cursor_dispatcher.deactivate();
                    let events = lock(&runner.runtime)
                        .on_capture_lost(RecoveryReason::BackendFailure, runner.now_us())?;
                    handle_runtime_events(events, false, &mut runner)?;
                }
            }
        }
        poll_clipboard(&mut runner);
        handle_video_worker_flags(&mut runner);
        send_periodic_stats(&mut runner);
        if runner.last_tick.elapsed() >= HOST_TICK_INTERVAL {
            runner.last_tick = Instant::now();
            let events = lock(&runner.runtime).tick(runner.now_us())?;
            handle_runtime_events(events, false, &mut runner)?;
        }
    }

    server.stop()?;
    hosting_enabled.store(false, Ordering::Release);
    local_ipc_worker.stop();
    runner.input_worker.shutdown();
    reset_clipboard_session(&mut runner);
    runner.capture.stop();
    runner.capture_running = false;
    close_video_sender(&mut runner);
    runner.active_encoder = None;
    runner.active_epoch = None;
    runner.cursor_dispatcher.deactivate();
    let _ = lock(&runner.runtime).stop();
    println!("Foreground host stopped.");
    Ok(())
}

struct LocalIpcWorker {
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl LocalIpcWorker {
    fn start(
        runtime: Arc<Mutex<HostRuntime>>,
        authorizer: Arc<Mutex<TailscaleAllowlistAuthorizer>>,
        hosting_enabled: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = Arc::clone(&stopping);
        let worker = thread::Builder::new()
            .name("racc-local-ipc".to_owned())
            .spawn(move || {
                let handler_runtime = Arc::clone(&runtime);
                let handler_authorizer = Arc::clone(&authorizer);
                let handler_hosting_enabled = Arc::clone(&hosting_enabled);
                let make_handler = move || {
                    WindowsHostIpcHandler::new(
                        Arc::clone(&handler_runtime),
                        Arc::clone(&handler_authorizer),
                        Arc::clone(&handler_hosting_enabled),
                    )
                };
                if let Err(error) = run_local_ipc_server(make_handler, Arc::clone(&worker_stopping))
                {
                    if !worker_stopping.load(Ordering::Acquire) {
                        eprintln!("local host IPC server stopped: {error}");
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

impl Drop for LocalIpcWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

struct ForegroundHost {
    bind_ip: IpAddr,
    runtime: Arc<Mutex<HostRuntime>>,
    control_sender: ControlSendHandle,
    input_worker: HostInputWorker<WindowsSendInput>,
    input_handle: HostInputHandle,
    cursor_dispatcher: HostCursorDispatcher,
    cursor_transport: Option<CursorNetworkTransport>,
    // Last OS topology snapshot; the input topology follows successful host announcements.
    topology: Topology,
    input_topology: Option<Topology>,
    clipboard: Option<HostClipboardBridge>,
    active_connection_id: Option<HostConnectionId>,
    display_refresh_mhz: HashMap<DisplayId, u32>,
    cpu_sampler: SystemCpuSampler,
    last_stats_report: Instant,
    last_stats_bytes_total: u64,
    clipboard_listener: Option<ClipboardListener>,
    clipboard_adapter: WindowsClipboard,
    clipboard_last_send_attempt_ms: Option<u64>,
    capture: WindowsCaptureBackend,
    capture_running: bool,
    deferred_capture_actions: VecDeque<CaptureAction>,
    video_sender: Option<VideoSendWorker>,
    active_epoch: Option<u16>,
    next_frame_id: u32,
    video_paused: bool,
    pending_encoder: Option<EncoderRequest>,
    active_encoder: Option<ActiveEncoder>,
    encoder_config: Option<EncoderConfig>,
    encoder_software: bool,
    pending_bitrate: Option<u32>,
    started_at: Instant,
    last_tick: Instant,
    last_topology_scan: Instant,
}

impl ForegroundHost {
    fn now_us(&self) -> u64 {
        u64::try_from(self.started_at.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

fn refresh_topology_if_due(runner: &mut ForegroundHost) -> Result<(), Box<dyn Error>> {
    if runner.last_topology_scan.elapsed() < TOPOLOGY_POLL_INTERVAL {
        return Ok(());
    }
    runner.last_topology_scan = Instant::now();
    let displays = match runner.capture.enumerate_displays() {
        Ok(displays) if !displays.is_empty() => displays,
        Ok(_) => {
            eprintln!("Windows returned no active displays during topology refresh; keeping the last topology.");
            return Ok(());
        }
        Err(error) => {
            eprintln!(
                "Windows display topology refresh failed; keeping the last topology: {error}"
            );
            return Ok(());
        }
    };
    let display_values = displays
        .iter()
        .map(|display| display.display.clone())
        .collect::<Vec<_>>();
    let topology = match changed_topology(&runner.topology, display_values) {
        Ok(Some(topology)) => topology,
        Ok(None) => return Ok(()),
        Err(error) => {
            eprintln!(
                "Windows returned an invalid display topology; keeping the last topology: {error}"
            );
            return Ok(());
        }
    };
    runner.display_refresh_mhz = displays
        .iter()
        .map(|display| (display.display.id(), display.display.refresh_mhz()))
        .collect();
    runner.topology = topology.clone();
    let events = lock(&runner.runtime).on_topology_changed(topology, runner.now_us())?;
    handle_runtime_events(events, false, runner)
}

struct EncoderRequest {
    operation_id: u64,
    operation: EncoderOperation,
    config: EncoderConfig,
    software: bool,
}

enum EncoderOperation {
    Configure,
    Rebuild,
}

enum ActiveEncoder {
    Hardware {
        encoder: MediaFoundationH264Encoder,
        converter: WindowsNv12Converter,
    },
    Software {
        encoder: Box<OpenH264Encoder>,
        converter: WindowsNv12Converter,
        readback: Box<WindowsI420Readback>,
    },
}

impl ActiveEncoder {
    fn create(
        frame: &GpuFrame,
        config: EncoderConfig,
        software: bool,
    ) -> Result<Self, EncodeError> {
        let converter = WindowsNv12Converter::new(frame, config)?;
        if software {
            let readback = WindowsI420Readback::new_for_capture(frame, config)?;
            let encoder = OpenH264Encoder::new(config)?;
            Ok(Self::Software {
                encoder: Box::new(encoder),
                converter,
                readback: Box::new(readback),
            })
        } else {
            let encoder = MediaFoundationH264Encoder::new_for_capture(frame, config)?;
            Ok(Self::Hardware { encoder, converter })
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Hardware { .. } => "Media Foundation H.264",
            Self::Software { .. } => "OpenH264 software",
        }
    }

    fn request_keyframe(&mut self) -> Result<(), EncodeError> {
        match self {
            Self::Hardware { encoder, .. } => encoder.request_keyframe(),
            Self::Software { encoder, .. } => encoder.request_keyframe(),
        }
    }
}

impl FrameEncoder for ActiveEncoder {
    fn encode_frame(&mut self, frame: &GpuFrame) -> Result<Option<EncodedPacket>, EncodeError> {
        match self {
            Self::Hardware { encoder, converter } => encoder.encode_capture_frame(converter, frame),
            Self::Software {
                encoder,
                converter,
                readback,
            } => {
                let surface = converter.convert(frame)?;
                readback.with_i420(surface.as_encoder_frame(), |i420| {
                    encoder.encode(EncoderInput::I420(i420))
                })?
            }
        }
    }
}

fn adapter_event_requires_input_release(event: &HostAdapterEvent) -> bool {
    match event {
        HostAdapterEvent::ViewerAccepted { .. } => true,
        HostAdapterEvent::Runtime { event, .. } => runtime_event_requires_input_release(event),
        _ => false,
    }
}

fn runtime_event_requires_input_release(event: &HostRuntimeEvent) -> bool {
    match event {
        HostRuntimeEvent::CloseConnection(_) | HostRuntimeEvent::Stopped => true,
        HostRuntimeEvent::SendControl {
            message: ControlMessage::StreamReset(_) | ControlMessage::TopologyAnnounce(_),
            ..
        } => true,
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
    }
}
fn handle_adapter_event(
    event: HostAdapterEvent,
    runner: &mut ForegroundHost,
) -> Result<(), Box<dyn Error>> {
    match event {
        HostAdapterEvent::ViewerAccepted {
            connection_id,
            remote_addr,
            video_udp_port,
        } => {
            reset_clipboard_session(runner);
            runner.active_connection_id = Some(connection_id);
            runner.clipboard = Some(HostClipboardBridge::new(connection_id));
            match runner.clipboard_adapter.start_listener() {
                Ok(listener) => runner.clipboard_listener = Some(listener),
                Err(error) => eprintln!("host clipboard listener unavailable: {error}"),
            }
            close_video_sender(runner);
            if video_udp_port == 0 {
                eprintln!(
                    "authorized viewer announced an invalid zero UDP port; video is disabled"
                );
                let actions = std::mem::take(&mut runner.deferred_capture_actions);
                for action in actions {
                    fail_capture_action(action, runner)?;
                }
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
                    eprintln!("cursor UDP path unavailable; video may continue: {error}");
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
                    runner.video_sender = Some(VideoSendWorker::new(sender)?);
                    runner.active_epoch = None;
                    runner.next_frame_id = 0;
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
        HostAdapterEvent::Runtime {
            connection_id,
            event,
            ..
        } => {
            let ends_session = runner.clipboard.as_ref().is_some_and(|bridge| {
                bridge.connection_id() == connection_id && clipboard_session_ended(&event)
            });
            if ends_session {
                reset_clipboard_session(runner);
            }
            handle_runtime_events(vec![event], true, runner)?;
        }
        HostAdapterEvent::AuthenticatedMessage {
            connection_id,
            message: ControlMessage::ClipboardUpdate(update),
            ..
        } => handle_host_clipboard_update(connection_id, update, runner),
        HostAdapterEvent::AuthenticatedMessage {
            connection_id,
            message: ControlMessage::ClipboardSyncControl(control),
            ..
        } => {
            if let Some(bridge) = runner
                .clipboard
                .as_mut()
                .filter(|bridge| bridge.connection_id() == connection_id)
            {
                if let Err(error) = bridge.set_enabled(control.enabled) {
                    eprintln!("host clipboard session could not be updated: {error:?}");
                }
            }
        }
        HostAdapterEvent::AuthenticatedMessage {
            connection_id,
            message: ControlMessage::InputEvent(input),
            ..
        } => {
            let _ = runner.input_handle.try_enqueue(connection_id, input);
        }
        HostAdapterEvent::AuthenticatedMessage { .. } => {}
        HostAdapterEvent::IdentityCheckFailed { remote_addr, error } => {
            eprintln!("Tailscale identity check failed for {remote_addr}: {error}");
        }
        HostAdapterEvent::ServerError { message } => eprintln!("control server error: {message}"),
    }
    Ok(())
}

fn handle_host_clipboard_update(
    connection_id: HostConnectionId,
    update: racc_proto::ClipboardUpdate,
    runner: &mut ForegroundHost,
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
            if let Err(error) = runner.clipboard_adapter.write_text(&applied.bytes) {
                eprintln!("could not apply remote clipboard text: {error}");
            }
        }
        Ok(RemoteChangeResult::Rejected(ClipboardRejection::TooLarge { bytes, limit })) => {
            eprintln!("remote clipboard text rejected ({bytes} bytes exceeds {limit} bytes)");
        }
        Ok(RemoteChangeResult::Rejected(ClipboardRejection::InvalidUtf8)) => {
            eprintln!("remote clipboard text rejected (invalid UTF-8)");
        }
        Ok(RemoteChangeResult::Ignored(_)) => {}
        Err(crate::clipboard_bridge::HostClipboardError::UnsupportedLogicalClockVersion) => {
            eprintln!("remote clipboard update rejected (unsupported logical clock version)");
        }
        Err(crate::clipboard_bridge::HostClipboardError::LogicalClockOutOfRange) => {
            eprintln!("remote clipboard update rejected (logical clock out of range)");
        }
        Err(crate::clipboard_bridge::HostClipboardError::SessionStartFailed) => {
            eprintln!("host clipboard session could not start");
        }
        Err(crate::clipboard_bridge::HostClipboardError::InvalidUtf8) => {
            eprintln!("host clipboard policy returned invalid UTF-8");
        }
    }
}

fn poll_clipboard(runner: &mut ForegroundHost) {
    let mut notified = false;
    let mut listener_disconnected = false;
    if let Some(listener) = runner.clipboard_listener.as_ref() {
        loop {
            match listener.notifications().try_recv() {
                Ok(()) => notified = true,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    listener_disconnected = true;
                    break;
                }
            }
        }
    }
    if listener_disconnected {
        runner.clipboard_listener = None;
        eprintln!("host clipboard listener stopped");
    }
    let clipboard_enabled = runner
        .clipboard
        .as_ref()
        .is_some_and(HostClipboardBridge::is_enabled);
    if notified && clipboard_enabled {
        match runner.clipboard_adapter.read_text() {
            Ok(Some(bytes)) => {
                if let Some(bridge) = runner.clipboard.as_mut() {
                    match bridge.local_change(&bytes) {
                        LocalChangeResult::Rejected(ClipboardRejection::TooLarge { .. }) => {
                            eprintln!("local clipboard text rejected (exceeds 512 KiB)");
                        }
                        LocalChangeResult::Rejected(ClipboardRejection::InvalidUtf8) => {
                            eprintln!("local clipboard text rejected (invalid UTF-8)");
                        }
                        _ => {}
                    }
                }
            }
            Ok(None) => {}
            Err(error) => eprintln!("could not read host clipboard text: {error}"),
        }
    }

    if !clipboard_enabled {
        return;
    }
    let now_ms = u64::try_from(runner.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    if runner
        .clipboard_last_send_attempt_ms
        .is_some_and(|last| now_ms < last.saturating_add(50))
    {
        return;
    }
    let Some(bridge) = runner.clipboard.as_mut() else {
        return;
    };
    let message = match bridge.next_outbound(now_ms) {
        Ok(Some(message)) => message,
        Ok(None) => return,
        Err(error) => {
            eprintln!("could not encode host clipboard update: {error:?}");
            return;
        }
    };
    let connection_id = bridge.connection_id();
    runner.clipboard_last_send_attempt_ms = Some(now_ms);
    match runner.control_sender.send(connection_id, message) {
        Ok(()) => bridge.confirm_outbound_queued(),
        Err(error) => eprintln!("could not queue host clipboard update: {error}"),
    }
}

fn send_periodic_stats(runner: &mut ForegroundHost) {
    if runner.last_stats_report.elapsed() < Duration::from_secs(1) {
        return;
    }
    let now = Instant::now();
    let elapsed_us =
        u64::try_from(runner.last_stats_report.elapsed().as_micros()).unwrap_or(u64::MAX);
    runner.last_stats_report = now;

    let Some(connection_id) = runner.active_connection_id else {
        runner.last_stats_bytes_total = runner
            .video_sender
            .as_ref()
            .map_or(0, VideoSendWorker::bytes_sent_total);
        let _ = runner.cpu_sampler.sample_tenths();
        let _ = runner.cpu_sampler.sample_process_tenths();
        return;
    };

    let sent_total = runner
        .video_sender
        .as_ref()
        .map_or(0, VideoSendWorker::bytes_sent_total);
    let sent_delta = sent_total.saturating_sub(runner.last_stats_bytes_total);
    runner.last_stats_bytes_total = sent_total;
    let actual_bitrate_kbps = if elapsed_us == 0 {
        0
    } else {
        u32::try_from(u128::from(sent_delta).saturating_mul(8_000) / u128::from(elapsed_us))
            .unwrap_or(u32::MAX)
    };
    let current_display = lock(&runner.runtime).status().current_display;
    let refresh = current_display
        .and_then(|display_id| runner.display_refresh_mhz.get(&display_id).copied())
        .unwrap_or(0);
    let active_config = runner
        .encoder_config
        .filter(|_| runner.active_encoder.is_some());
    let (width, height, target_bitrate_kbps) = active_config.map_or((0, 0, 0), |config| {
        (
            u16::try_from(config.width()).unwrap_or(u16::MAX),
            u16::try_from(config.height()).unwrap_or(u16::MAX),
            config.bitrate_bps() / 1_000,
        )
    });
    let encoder = match runner.active_encoder.as_ref() {
        Some(ActiveEncoder::Hardware { .. }) => EncoderKind::MediaFoundationHw,
        Some(ActiveEncoder::Software { .. }) => EncoderKind::OpenH264,
        None => EncoderKind::Unknown,
    };
    let host_cpu_pct_x10 = runner.cpu_sampler.sample_tenths().unwrap_or(0);
    let process_cpu_pct_x10 = runner.cpu_sampler.sample_process_tenths();
    let report = StatsReport {
        // GetSystemTimes reports machine-wide utilization. Zero is the protocol's
        // only available unknown sentinel until a pair of samples is available.
        host_cpu_pct_x10,
        process_cpu_pct_x10,
        capture_backend: if runner.capture_running {
            CaptureBackend::Dxgi
        } else {
            CaptureBackend::Unknown
        },
        encoder,
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
        eprintln!("host telemetry report could not be queued: {error}");
    }
}
fn clipboard_session_ended(event: &HostRuntimeEvent) -> bool {
    matches!(
        event,
        HostRuntimeEvent::CloseConnection(_)
            | HostRuntimeEvent::SessionAction {
                action: HostAction::Event(SessionEvent::Reconnecting | SessionEvent::SessionEnded),
                ..
            }
    )
}

fn reset_clipboard_session(runner: &mut ForegroundHost) {
    runner.input_handle.deactivate();
    runner.cursor_dispatcher.deactivate();
    runner.cursor_transport = None;
    if let Some(bridge) = runner.clipboard.as_mut() {
        bridge.end_session();
    }
    runner.clipboard = None;
    runner.active_connection_id = None;
    runner.clipboard_listener = None;
    runner.clipboard_last_send_attempt_ms = None;
}
fn runtime_event_requires_cursor_release(event: &HostRuntimeEvent) -> bool {
    match event {
        HostRuntimeEvent::CloseConnection(_) | HostRuntimeEvent::Stopped => true,
        HostRuntimeEvent::SessionAction { action, .. } => matches!(
            action,
            HostAction::Capture(_)
                | HostAction::Encoder(
                    EncoderAction::Configure { .. } | EncoderAction::Rebuild { .. }
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
    }
}
fn handle_runtime_events(
    events: Vec<HostRuntimeEvent>,
    controls_already_sent: bool,
    runner: &mut ForegroundHost,
) -> Result<(), Box<dyn Error>> {
    for event in events {
        if runtime_event_requires_input_release(&event) {
            runner.input_handle.deactivate();
        }
        if runtime_event_requires_cursor_release(&event) {
            runner.cursor_dispatcher.deactivate();
        }
        match event {
            HostRuntimeEvent::SessionAction { connection_id, action } => match action {
                HostAction::SendControl(message) => {
                    if let Some(connection_id) = connection_id {
                        handle_control_send(connection_id, message, controls_already_sent, runner);
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
                    if event == SessionEvent::EncoderFailed {
                        eprintln!("H.264 hardware and software encoder setup failed; video is stopped.");
                        runner.active_epoch = None;
                        runner.cursor_dispatcher.deactivate();
                        runner.active_encoder = None;
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
                handle_control_send(connection_id, message, controls_already_sent, runner);
            }
            HostRuntimeEvent::BindAddressChanged(_)
            | HostRuntimeEvent::QualityPreference(_)
            | HostRuntimeEvent::ViewerFeedback(_)
            | HostRuntimeEvent::QualityDecision(_)
            | HostRuntimeEvent::CloseConnection(_)
            | HostRuntimeEvent::Stopped => {}
        }
    }
    Ok(())
}

fn report_host_event(
    connection_id: HostConnectionId,
    kind: HostEventKind,
    runner: &mut ForegroundHost,
) {
    let message = ControlMessage::HostEventReport(HostEventReport { kind });
    if let Err(error) = runner.control_sender.send(connection_id, message) {
        eprintln!("could not queue host lifecycle telemetry: {error}");
    }
}

fn handle_control_send(
    connection_id: HostConnectionId,
    message: ControlMessage,
    controls_already_sent: bool,
    runner: &mut ForegroundHost,
) {
    let message = match message {
        ControlMessage::TopologyAnnounce(announced) => {
            match Topology::from_proto(&announced) {
                Ok(topology) => {
                    let current_display = lock(&runner.runtime).status().current_display;
                    let active_mapping = runner
                        .active_epoch
                        .zip(current_display)
                        .filter(|_| runner.active_connection_id == Some(connection_id));
                    let input_session = active_mapping.and_then(|(epoch, display_id)| {
                        let previous = runner.input_topology.as_ref()?;
                        HostInputSession::new(connection_id, epoch, display_id, previous.clone())
                            .with_updated_topology(previous, topology.clone())
                    });
                    if let Some(session) = input_session {
                        runner.input_handle.activate_session(session);
                        if let Some((epoch, display_id)) = active_mapping {
                            if let Some(display) = topology
                                .displays()
                                .iter()
                                .find(|display| display.id() == display_id)
                            {
                                let _ = runner.cursor_dispatcher.update_origin(
                                    connection_id,
                                    epoch,
                                    display_id,
                                    display.origin(),
                                );
                            }
                        }
                    } else {
                        runner.input_handle.deactivate();
                        runner.cursor_dispatcher.deactivate();
                    }
                    runner.input_topology = Some(topology);
                }
                Err(error) => {
                    runner.input_handle.deactivate();
                    runner.cursor_dispatcher.deactivate();
                    runner.input_topology = None;
                    eprintln!(
                        "host topology announcement could not be used for input mapping: {error}"
                    );
                }
            }
            ControlMessage::TopologyAnnounce(announced)
        }
        ControlMessage::StreamReset(reset) => {
            runner.input_handle.deactivate();
            runner.cursor_dispatcher.deactivate();
            let delivered = controls_already_sent
                || runner
                    .control_sender
                    .send_confirmed(
                        connection_id,
                        ControlMessage::StreamReset(reset),
                        STREAM_RESET_WRITE_TIMEOUT,
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
                        .filter(|topology| topology.revision() == reset.topology_rev)
                    {
                        if let Some(display_id) = DisplayId::new(reset.display_id) {
                            if let Some(display) = topology.displays().iter().find(|display| {
                                display.id() == display_id && display.flags().available()
                            }) {
                                runner.input_handle.activate_session(HostInputSession::new(
                                    connection_id,
                                    reset.epoch,
                                    display_id,
                                    topology.clone(),
                                ));
                                if let Some(transport) = runner.cursor_transport.as_mut() {
                                    if let Err(error) = runner.cursor_dispatcher.activate(
                                        connection_id,
                                        reset.epoch,
                                        display_id,
                                        display.origin(),
                                        transport,
                                    ) {
                                        eprintln!("cursor shape could not be confirmed for this epoch: {error:?}");
                                    }
                                }
                            }
                        }
                    }
                }
            } else {
                runner.active_epoch = None;
            }
            if !delivered {
                eprintln!("StreamReset write was not confirmed; UDP video remains gated for connection {connection_id}.");
            }
            return;
        }
        other => other,
    };
    if !controls_already_sent {
        if let Err(error) = runner.control_sender.send(connection_id, message) {
            eprintln!("could not queue host control response: {error}");
        }
    }
}

fn execute_capture_action(
    action: CaptureAction,
    runner: &mut ForegroundHost,
) -> Result<(), Box<dyn Error>> {
    runner.cursor_dispatcher.deactivate();
    if !matches!(&action, CaptureAction::Stop) && runner.video_sender.is_none() {
        runner.deferred_capture_actions.push_back(action);
        return Ok(());
    }
    let (operation_id, result) = match action {
        CaptureAction::SwitchDisplay {
            operation_id,
            from: _,
            to,
        } => {
            runner.active_epoch = None;
            runner.active_encoder = None;
            let result = if runner.capture_running {
                runner.capture.migrate(to)
            } else {
                runner.capture.start(to, CaptureParams::default())
            };
            (Some(operation_id), result)
        }
        CaptureAction::Recreate {
            operation_id,
            display_id,
        } => {
            runner.active_epoch = None;
            runner.active_encoder = None;
            runner.capture.stop();
            runner.capture_running = false;
            (
                Some(operation_id),
                runner.capture.start(display_id, CaptureParams::default()),
            )
        }
        CaptureAction::Stop => {
            runner.active_epoch = None;
            runner.active_encoder = None;
            runner.encoder_config = None;
            runner.pending_encoder = None;
            runner.video_paused = true;
            runner.capture.stop();
            runner.capture_running = false;
            close_video_sender(runner);
            return Ok(());
        }
    };
    runner.capture_running = result.is_ok();
    if let Err(error) = &result {
        eprintln!("capture start or display switch failed: {error}");
    }
    if let Some(operation_id) = operation_id {
        let capture_result = result.map_err(|_| CaptureFailure::BackendFailure);
        let events = lock(&runner.runtime).on_capture_result(
            operation_id,
            capture_result,
            runner.now_us(),
        )?;
        handle_runtime_events(events, false, runner)?;
    }
    Ok(())
}

fn fail_capture_action(
    action: CaptureAction,
    runner: &mut ForegroundHost,
) -> Result<(), Box<dyn Error>> {
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
    runner: &mut ForegroundHost,
) -> Result<(), Box<dyn Error>> {
    match action {
        EncoderAction::Configure {
            operation_id,
            width,
            height,
            ..
        } => match make_encoder_config(width, height, runner.pending_bitrate) {
            Ok(config) => {
                runner.cursor_dispatcher.deactivate();
                runner.encoder_config = Some(config);
                runner.pending_encoder = Some(EncoderRequest {
                    operation_id,
                    operation: EncoderOperation::Configure,
                    config,
                    software: false,
                });
                runner.active_encoder = None;
                runner.active_epoch = None;
            }
            Err(error) => {
                eprintln!("invalid host encoder dimensions: {error}");
                let events = lock(&runner.runtime).on_encoder_configured(
                    operation_id,
                    Err(EncoderFailure::ConfigureFailed),
                    runner.now_us(),
                )?;
                handle_runtime_events(events, false, runner)?;
            }
        },
        EncoderAction::Rebuild {
            operation_id,
            use_software,
        } => {
            runner.cursor_dispatcher.deactivate();
            if let Some(config) = runner.encoder_config {
                runner.pending_encoder = Some(EncoderRequest {
                    operation_id,
                    operation: EncoderOperation::Rebuild,
                    config,
                    software: use_software,
                });
                runner.active_encoder = None;
                runner.active_epoch = None;
            } else {
                let events = lock(&runner.runtime).on_encoder_rebuild_result(
                    operation_id,
                    false,
                    runner.now_us(),
                )?;
                handle_runtime_events(events, false, runner)?;
            }
        }
        EncoderAction::SetPaused(paused) => {
            runner.video_paused = paused;
            if !paused {
                if let Some(encoder) = runner.active_encoder.as_mut() {
                    if let Err(error) = encoder.request_keyframe() {
                        eprintln!("could not request resume keyframe: {error}");
                    }
                }
            }
        }
        EncoderAction::SetBitrate(bitrate_bps) => {
            runner.pending_bitrate = Some(bitrate_bps.clamp(1_000, 20_000_000));
        }
        EncoderAction::ForceKeyframe { .. } => {
            if let Some(encoder) = runner.active_encoder.as_mut() {
                if let Err(error) = encoder.request_keyframe() {
                    eprintln!("encoder did not accept a keyframe request: {error}");
                }
            }
        }
    }
    Ok(())
}

fn make_encoder_config(
    width: u16,
    height: u16,
    bitrate_override: Option<u32>,
) -> Result<EncoderConfig, EncodeError> {
    let height = u32::from(height);
    let bitrate = bitrate_override.unwrap_or(match height {
        0..=480 => 1_500_000,
        481..=720 => 3_500_000,
        _ => 7_000_000,
    });
    EncoderConfig::new(u32::from(width), height, bitrate)
}

fn handle_capture_event(
    event: CaptureEvent,
    runner: &mut ForegroundHost,
) -> Result<(), Box<dyn Error>> {
    let reason = match event {
        CaptureEvent::Frame(frame) => {
            handle_video_frame(frame, runner)?;
            return Ok(());
        }
        CaptureEvent::AccessLost(_) => {
            eprintln!("Capture access was lost. This helper uses WinSta0\\Default and does not switch to the Winlogon secure desktop; secure-desktop capture is unavailable. Capture will retry when desktop access returns.");
            RecoveryReason::AccessLost
        }
        CaptureEvent::DisplayLost => RecoveryReason::DisplayChanged,
        CaptureEvent::DeviceLost | CaptureEvent::Error(_) => RecoveryReason::BackendFailure,
        CaptureEvent::Recovered => {
            println!("Desktop capture recovered.");
            return Ok(());
        }
        CaptureEvent::CursorShape(shape) => {
            if let Err(error) = runner
                .cursor_dispatcher
                .shape_changed(shape, runner.cursor_transport.as_mut())
            {
                eprintln!("captured cursor shape rejected or not delivered: {error:?}");
            }
            return Ok(());
        }
        CaptureEvent::CursorMoved(position) => {
            handle_cursor_position(position, runner)?;
            return Ok(());
        }
    };
    runner.active_epoch = None;
    runner.cursor_dispatcher.deactivate();
    let events = lock(&runner.runtime).on_capture_lost(reason, runner.now_us())?;
    handle_runtime_events(events, false, runner)
}

fn activate_encoder_for_frame(
    frame: &GpuFrame,
    runner: &mut ForegroundHost,
) -> Result<(), Box<dyn Error>> {
    if let Some(request) = runner.pending_encoder.take() {
        match ActiveEncoder::create(frame, request.config, request.software) {
            Ok(encoder) => {
                let name = encoder.name().to_owned();
                runner.encoder_software = request.software;
                runner.active_encoder = Some(encoder);
                runner.encoder_config = Some(request.config);
                lock(&runner.runtime).set_encoder_name(Some(name));
                let events =
                    match request.operation {
                        EncoderOperation::Configure => lock(&runner.runtime)
                            .on_encoder_configured(request.operation_id, Ok(()), runner.now_us())?,
                        EncoderOperation::Rebuild => lock(&runner.runtime)
                            .on_encoder_rebuild_result(
                                request.operation_id,
                                true,
                                runner.now_us(),
                            )?,
                    };
                handle_runtime_events(events, false, runner)?;
            }
            Err(error) => {
                eprintln!(
                    "{} H.264 encoder setup failed: {error}",
                    if request.software {
                        "OpenH264"
                    } else {
                        "Media Foundation hardware"
                    }
                );
                let events = match request.operation {
                    EncoderOperation::Configure => lock(&runner.runtime).on_encoder_configured(
                        request.operation_id,
                        Err(EncoderFailure::ConfigureFailed),
                        runner.now_us(),
                    )?,
                    EncoderOperation::Rebuild => lock(&runner.runtime).on_encoder_rebuild_result(
                        request.operation_id,
                        false,
                        runner.now_us(),
                    )?,
                };
                handle_runtime_events(events, false, runner)?;
            }
        }
    }

    if let Some(bitrate) = runner.pending_bitrate.take() {
        if let Some(previous) = runner
            .encoder_config
            .filter(|_| runner.active_encoder.is_some())
        {
            if previous.bitrate_bps() != bitrate {
                match EncoderConfig::new(previous.width(), previous.height(), bitrate) {
                    Ok(config) => {
                        match ActiveEncoder::create(frame, config, runner.encoder_software) {
                            Ok(mut encoder) => {
                                let name = encoder.name().to_owned();
                                if let Err(error) = encoder.request_keyframe() {
                                    eprintln!(
                                        "could not request IDR after bitrate adjustment: {error}"
                                    );
                                }
                                runner.active_encoder = Some(encoder);
                                runner.encoder_config = Some(config);
                                lock(&runner.runtime).set_encoder_name(Some(name));
                            }
                            Err(error) => {
                                eprintln!("bitrate reconfiguration failed; keeping the prior encoder: {error}");
                                runner.pending_bitrate = Some(bitrate);
                            }
                        }
                    }
                    Err(error) => eprintln!("bitrate reconfiguration is invalid: {error}"),
                }
            }
        } else {
            runner.pending_bitrate = Some(bitrate);
        }
    }
    Ok(())
}

fn handle_cursor_position(
    position: racc_capture::CursorPosition,
    runner: &mut ForegroundHost,
) -> Result<(), Box<dyn Error>> {
    if runner.video_paused {
        return Ok(());
    }
    let Some(epoch) = runner.active_epoch else {
        return Ok(());
    };
    let Some(connection_id) = runner.active_connection_id else {
        return Ok(());
    };
    let Some(display_id) = lock(&runner.runtime).status().current_display else {
        return Ok(());
    };
    let Some(topology) = runner.input_topology.as_ref() else {
        return Ok(());
    };
    if !topology
        .displays()
        .iter()
        .any(|display| display.id() == display_id && display.flags().available())
    {
        return Ok(());
    }
    let Some(transport) = runner.cursor_transport.as_mut() else {
        return Ok(());
    };
    match runner
        .cursor_dispatcher
        .position(connection_id, epoch, display_id, position, transport)
    {
        Ok(
            CursorDispatch::Sent(_)
            | CursorDispatch::DroppedWouldBlock
            | CursorDispatch::Suppressed,
        ) => {}
        Err(error) => eprintln!("cursor UDP send failed: {error:?}"),
    }
    Ok(())
}
fn handle_video_frame(frame: GpuFrame, runner: &mut ForegroundHost) -> Result<(), Box<dyn Error>> {
    activate_encoder_for_frame(&frame, runner)?;
    if runner.video_paused || runner.active_epoch.is_none() || runner.video_sender.is_none() {
        return Ok(());
    }
    let Some(epoch) = runner.active_epoch else {
        return Ok(());
    };
    let (result, queue_overflows, encoder_lag_ms) = {
        let Some(encoder) = runner.active_encoder.as_mut() else {
            return Ok(());
        };
        let Some(sender) = runner.video_sender.as_ref() else {
            return Ok(());
        };
        let started = Instant::now();
        let result = dispatch_frame(
            encoder,
            &frame,
            epoch,
            &mut runner.next_frame_id,
            |packet| sender.try_send(packet),
        );
        let elapsed_ms = started.elapsed().as_millis().min(60_000) as u32;
        (result, sender.take_queue_overflows(), elapsed_ms)
    };
    let can_observe = !matches!(&result, Err(DispatchError::Encode(_)));
    match result {
        Ok(Some(_)) | Ok(None) => {}
        Err(DispatchError::Send(error)) => {
            eprintln!("encoded frame dropped by bounded UDP queue: {error}")
        }
        Err(DispatchError::Encode(error)) => {
            eprintln!("H.264 encode failed; waiting for a session reconfiguration: {error}");
            runner.active_encoder = None;
            runner.active_epoch = None;
            runner.cursor_dispatcher.deactivate();
        }
    }
    if can_observe {
        let sample_count = queue_overflows.clamp(1, 2);
        for index in 0..sample_count {
            let events = lock(&runner.runtime).on_local_sender_observation(
                HostSenderObservation {
                    epoch,
                    queue_overflow: index < queue_overflows,
                    encoder_lag_ms: Some(encoder_lag_ms),
                },
                runner.now_us(),
            )?;
            handle_runtime_events(events, false, runner)?;
        }
    }
    Ok(())
}

fn handle_video_worker_flags(runner: &mut ForegroundHost) {
    let Some(sender) = runner.video_sender.as_ref() else {
        return;
    };
    if sender.take_send_failed() {
        eprintln!("UDP sender failed; this video path has stopped.");
        runner.active_epoch = None;
        runner.cursor_dispatcher.deactivate();
        close_video_sender(runner);
        return;
    }
    if sender.take_keyframe_request() {
        if let Some(encoder) = runner.active_encoder.as_mut() {
            if let Err(error) = encoder.request_keyframe() {
                eprintln!("could not request IDR after sender queue pressure: {error}");
            }
        }
    }
}

fn close_video_sender(runner: &mut ForegroundHost) {
    if let Some(mut sender) = runner.video_sender.take() {
        sender.close();
    }
}

struct VideoSendWorker {
    queue: Arc<VideoFrameQueue>,
    bytes_sent_total: Arc<AtomicU64>,
    keyframe_request: Arc<AtomicBool>,
    send_failed: Arc<AtomicBool>,
    queue_overflows: AtomicU64,
    worker: Option<JoinHandle<()>>,
}
impl VideoSendWorker {
    fn new(mut sender: VideoSender) -> io::Result<Self> {
        let queue = Arc::new(VideoFrameQueue::new());
        let worker_queue = Arc::clone(&queue);
        let keyframe_request = Arc::new(AtomicBool::new(false));
        let worker_keyframe_request = Arc::clone(&keyframe_request);
        let send_failed = Arc::new(AtomicBool::new(false));
        let worker_send_failed = Arc::clone(&send_failed);
        let bytes_sent_total = Arc::new(AtomicU64::new(0));
        let worker_bytes_sent_total = Arc::clone(&bytes_sent_total);
        let worker = thread::Builder::new()
            .name("racc-host-video-send".to_owned())
            .spawn(move || {
                while let Some(frame) = worker_queue.pop() {
                    match sender.send_frame(frame) {
                        Ok(metrics) => {
                            let bytes = u64::try_from(metrics.bytes_sent).unwrap_or(u64::MAX);
                            let _ = worker_bytes_sent_total.fetch_update(
                                Ordering::Relaxed,
                                Ordering::Relaxed,
                                |current| Some(current.saturating_add(bytes)),
                            );
                        }
                        Err(_) => {
                            worker_send_failed.store(true, Ordering::Release);
                            break;
                        }
                    }
                    if sender.try_force_keyframe().is_some() {
                        worker_keyframe_request.store(true, Ordering::Release);
                    }
                }
                let _ = sender.close();
            })?;
        Ok(Self {
            queue,
            bytes_sent_total,
            keyframe_request,
            send_failed,
            queue_overflows: AtomicU64::new(0),
            worker: Some(worker),
        })
    }
    fn try_send(&self, frame: SenderFrame) -> Result<(), VideoWorkerError> {
        match self.queue.push(frame) {
            Ok(dropped_oldest) => {
                if dropped_oldest {
                    self.keyframe_request.store(true, Ordering::Release);
                    let _ = self.queue_overflows.fetch_update(
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                        |current| Some(current.saturating_add(1)),
                    );
                }
                Ok(())
            }
            Err(_) => Err(VideoWorkerError::Closed),
        }
    }
    fn bytes_sent_total(&self) -> u64 {
        self.bytes_sent_total.load(Ordering::Acquire)
    }
    fn take_keyframe_request(&self) -> bool {
        self.keyframe_request.swap(false, Ordering::AcqRel)
    }
    fn take_queue_overflows(&self) -> u64 {
        self.queue_overflows.swap(0, Ordering::AcqRel)
    }
    fn take_send_failed(&self) -> bool {
        self.send_failed.swap(false, Ordering::AcqRel)
    }
    fn close(&mut self) {
        self.queue.close();
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                eprintln!("UDP video worker panicked while stopping");
            }
        }
    }
}
impl Drop for VideoSendWorker {
    fn drop(&mut self) {
        self.close();
    }
}

struct VideoFrameQueue {
    state: Mutex<VideoFrameQueueState>,
    ready: Condvar,
}
struct VideoFrameQueueState {
    frames: VecDeque<SenderFrame>,
    closed: bool,
}
impl VideoFrameQueue {
    fn new() -> Self {
        Self {
            state: Mutex::new(VideoFrameQueueState {
                frames: VecDeque::with_capacity(UDP_PENDING_FRAME_CAPACITY),
                closed: false,
            }),
            ready: Condvar::new(),
        }
    }
    fn push(&self, frame: SenderFrame) -> Result<bool, SenderFrame> {
        let mut state = lock(&self.state);
        if state.closed {
            return Err(frame);
        }
        let dropped = if state.frames.len() >= UDP_PENDING_FRAME_CAPACITY {
            let _ = state.frames.pop_front();
            true
        } else {
            false
        };
        state.frames.push_back(frame);
        self.ready.notify_one();
        Ok(dropped)
    }
    fn pop(&self) -> Option<SenderFrame> {
        let mut state = lock(&self.state);
        loop {
            if let Some(frame) = state.frames.pop_front() {
                return Some(frame);
            }
            if state.closed {
                return None;
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
    fn close(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        state.frames.clear();
        self.ready.notify_all();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VideoWorkerError {
    Closed,
}
impl std::fmt::Display for VideoWorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("video sender worker is closed")
    }
}

fn config_root() -> Result<PathBuf, io::Error> {
    std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("LOCALAPPDATA"))
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("APPDATA and LOCALAPPDATA are unavailable"))
}

fn local_device_name() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .map(|name| bounded_name(&name))
        .unwrap_or_else(|| "Windows host".to_owned())
}

fn bounded_name(name: &str) -> String {
    let mut end = name.len().min(racc_proto::MAX_NAME_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_owned()
}

enum ConsoleCommand {
    Stop,
    Approve(String),
}

fn start_console_command_reader() -> Result<Receiver<ConsoleCommand>, io::Error> {
    let (command_tx, command_rx) = mpsc::sync_channel(8);
    thread::Builder::new()
        .name("racc-host-console".to_owned())
        .spawn(move || {
            let stdin = io::stdin();
            let mut input = stdin.lock();
            let mut line = Vec::with_capacity(600);
            let mut overflow = false;
            let mut buffer = [0_u8; 128];
            loop {
                match input.read(&mut buffer) {
                    Ok(0) | Err(_) => {
                        let _ = command_tx.send(ConsoleCommand::Stop);
                        break;
                    }
                    Ok(length) => {
                        let mut should_stop = false;
                        for byte in &buffer[..length] {
                            if *byte == b'\n' {
                                if !overflow {
                                    let command = String::from_utf8_lossy(&line);
                                    let command = command.trim();
                                    if command.eq_ignore_ascii_case("stop")
                                        || command.eq_ignore_ascii_case("quit")
                                    {
                                        should_stop = true;
                                        break;
                                    }
                                    if let Some(peer_key) = command.strip_prefix("approve ") {
                                        let peer_key = peer_key.trim();
                                        if !peer_key.is_empty()
                                            && peer_key.len() <= 512
                                            && command_tx
                                                .send(ConsoleCommand::Approve(peer_key.to_owned()))
                                                .is_err()
                                        {
                                            return;
                                        }
                                    }
                                }
                                line.clear();
                                overflow = false;
                            } else if *byte != b'\r' {
                                if line.len() < 600 {
                                    line.push(*byte);
                                } else {
                                    overflow = true;
                                }
                            }
                        }
                        if should_stop {
                            let _ = command_tx.send(ConsoleCommand::Stop);
                            break;
                        }
                    }
                }
            }
        })?;
    Ok(command_rx)
}

fn start_service_stop_command_reader(
    stop_signal: Receiver<()>,
) -> Result<Receiver<ConsoleCommand>, io::Error> {
    let (command_tx, command_rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("racc-helper-stop".to_owned())
        .spawn(move || {
            // The stop-event watcher sends once on signal and also on watcher failure;
            // fail closed so an unmonitored helper does not continue streaming.
            let _ = stop_signal.recv();
            let _ = command_tx.send(ConsoleCommand::Stop);
        })?;
    Ok(command_rx)
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
