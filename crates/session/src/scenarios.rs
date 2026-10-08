//! Test-only in-memory control and video scenario harness for both session machines.
use crate::*;
use racc_proto::{
    ControlMessage, Hello, HelloAck, HelloStatus, InputEvent, InputEventKind, OsType,
    RequestKeyframe, StreamCodec, StreamReset, StreamStatus, PROTOCOL_VERSION,
};
use racc_topology::{Display, DisplayFlags, DisplayId, Topology};

const MAX_QUEUED: usize = 128;
const MAX_PUMP_STEPS: usize = 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Side {
    Host,
    Viewer,
}

enum Payload {
    Control(Side, ControlMessage),
    Video { epoch: u16, key: bool },
}

struct Scheduled {
    at_us: u64,
    priority: i64,
    sequence: u64,
    payload: Payload,
}

/// Bounded deterministic test transport with delay, drop and reordering knobs.
struct Harness {
    host: HostSession,
    viewer: ViewerSession,
    now_us: u64,
    queue: Vec<Scheduled>,
    next_sequence: u64,
    drop_next_control: usize,
    drop_switch_requests: usize,
    delay_next_control_us: Option<u64>,
    reorder_next_control: bool,
    auto_capture: bool,
    auto_encoder: bool,
    pending_capture_ops: Vec<u64>,
    pending_encoder_ops: Vec<u64>,
    pending_rebuild: Option<(u64, bool)>,
    drop_video_frames: usize,
    dropped_switch_count: usize,
    switch_attempt_count: usize,
    keyframe_count: usize,
    decoder_reset_count: usize,
    injected_inputs: Vec<InputEvent>,
    captured_display: Option<u32>,
    capture_stopped: bool,
    events: Vec<SessionEvent>,
    dispositions: Vec<VideoDisposition>,
    auth_reject: bool,
    reconnect_requests: usize,
    action_count: usize,
    overflowed: bool,
}

impl Harness {
    fn new() -> Self {
        let topology = topology(
            1,
            vec![
                display(1, true, true, 1920, 1080),
                display(2, true, false, 1280, 720),
            ],
        );
        let caps = host_caps();
        Self {
            host: HostSession::new(caps, topology),
            viewer: ViewerSession::new(viewer_hello(PROTOCOL_VERSION)),
            now_us: 0,
            queue: Vec::new(),
            next_sequence: 1,
            drop_next_control: 0,
            drop_switch_requests: 0,
            delay_next_control_us: None,
            reorder_next_control: false,
            auto_capture: true,
            auto_encoder: true,
            pending_capture_ops: Vec::new(),
            pending_encoder_ops: Vec::new(),
            pending_rebuild: None,
            drop_video_frames: 0,
            dropped_switch_count: 0,
            switch_attempt_count: 0,
            keyframe_count: 0,
            decoder_reset_count: 0,
            injected_inputs: Vec::new(),
            captured_display: None,
            capture_stopped: false,
            events: Vec::new(),
            dispositions: Vec::new(),
            auth_reject: false,
            reconnect_requests: 0,
            action_count: 0,
            overflowed: false,
        }
    }

    fn with_viewer_version(version: u8) -> Self {
        let mut harness = Self::new();
        harness.viewer = ViewerSession::new(viewer_hello(version));
        harness
    }

    fn start(&mut self) {
        let actions = self.viewer.on_connect_at(self.now_us);
        self.viewer_actions(actions);
        self.pump();
    }

    fn advance_to(&mut self, now_us: u64) {
        assert!(
            now_us >= self.now_us,
            "virtual clock must not move backwards"
        );
        self.now_us = now_us;
        let actions = self.viewer.tick(self.now_us);
        self.viewer_actions(actions);
        self.pump();
        let actions = self.host.tick(self.now_us);
        self.host_actions(actions);
        self.pump();
    }

    fn viewer_actions(&mut self, actions: Vec<ViewerAction>) {
        self.count_actions(actions.len());
        for action in actions {
            match action {
                ViewerAction::SendControl(message) => self.enqueue_control(Side::Host, message),
                ViewerAction::ForwardInput(event) => {
                    self.enqueue_control(Side::Host, ControlMessage::InputEvent(event));
                }
                ViewerAction::ResetDecoder { .. } => self.decoder_reset_count += 1,
                ViewerAction::ReconnectTransport => self.reconnect_requests += 1,
                ViewerAction::Event(event) => self.events.push(event),
                ViewerAction::DisconnectTransport => {}
            }
        }
    }

    fn host_actions(&mut self, actions: Vec<HostAction>) {
        self.count_actions(actions.len());
        let mut follow_up = Vec::new();
        for action in actions {
            match action {
                HostAction::SendControl(message) => self.enqueue_control(Side::Viewer, message),
                HostAction::Capture(CaptureAction::SwitchDisplay {
                    operation_id, to, ..
                }) => {
                    self.captured_display = Some(to.get());
                    if self.auto_capture {
                        follow_up.extend(self.host.on_capture_result(
                            operation_id,
                            Ok(()),
                            self.now_us,
                        ));
                    } else {
                        self.pending_capture_ops.push(operation_id);
                    }
                }
                HostAction::Capture(CaptureAction::Recreate {
                    operation_id,
                    display_id,
                }) => {
                    self.captured_display = Some(display_id.get());
                    if self.auto_capture {
                        follow_up.extend(self.host.on_capture_result(
                            operation_id,
                            Ok(()),
                            self.now_us,
                        ));
                    } else {
                        self.pending_capture_ops.push(operation_id);
                    }
                }
                HostAction::Capture(CaptureAction::Stop) => self.capture_stopped = true,
                HostAction::Encoder(EncoderAction::Configure { operation_id, .. }) => {
                    if self.auto_encoder {
                        follow_up.extend(self.host.on_encoder_configured(
                            operation_id,
                            Ok(()),
                            self.now_us,
                        ));
                    } else {
                        self.pending_encoder_ops.push(operation_id);
                    }
                }
                HostAction::Encoder(EncoderAction::ForceKeyframe { epoch }) => {
                    self.keyframe_count += 1;
                    if self.drop_video_frames > 0 {
                        self.drop_video_frames -= 1;
                    } else {
                        self.enqueue_video(epoch, true);
                    }
                }
                HostAction::Encoder(EncoderAction::Rebuild {
                    operation_id,
                    use_software,
                }) => {
                    self.pending_rebuild = Some((operation_id, use_software));
                }
                HostAction::Encoder(EncoderAction::SetPaused(_)) => {}
                HostAction::Encoder(EncoderAction::SetBitrate(_)) => {}
                HostAction::Quality(_) => {}
                HostAction::InjectInput(event) => self.injected_inputs.push(event),
                HostAction::Event(event) => self.events.push(event),
            }
        }
        if !follow_up.is_empty() {
            self.host_actions(follow_up);
        }
    }

    fn enqueue_control(&mut self, side: Side, message: ControlMessage) {
        if matches!(message, ControlMessage::SwitchMonitor(_)) {
            self.switch_attempt_count += 1;
            if self.drop_switch_requests > 0 {
                self.drop_switch_requests -= 1;
                self.dropped_switch_count += 1;
                return;
            }
        }
        if self.drop_next_control > 0 {
            self.drop_next_control -= 1;
            return;
        }
        let delay = self.delay_next_control_us.take().unwrap_or_default();
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        let priority = if std::mem::take(&mut self.reorder_next_control) {
            -(sequence as i64)
        } else {
            0
        };
        self.push(Scheduled {
            at_us: self.now_us.saturating_add(delay),
            priority,
            sequence,
            payload: Payload::Control(side, message),
        });
    }

    fn enqueue_video(&mut self, epoch: u16, key: bool) {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.push(Scheduled {
            at_us: self.now_us,
            priority: 0,
            sequence,
            payload: Payload::Video { epoch, key },
        });
    }

    fn push(&mut self, event: Scheduled) {
        if self.queue.len() >= MAX_QUEUED {
            self.overflowed = true;
            return;
        }
        self.queue.push(event);
    }

    fn pump(&mut self) {
        for _ in 0..MAX_PUMP_STEPS {
            let next = self
                .queue
                .iter()
                .enumerate()
                .filter(|(_, event)| event.at_us <= self.now_us)
                .min_by_key(|(_, event)| (event.at_us, event.priority, event.sequence))
                .map(|(index, _)| index);
            let Some(index) = next else {
                return;
            };
            let event = self.queue.remove(index);
            match event.payload {
                Payload::Control(Side::Host, message) => self.receive_host(message),
                Payload::Control(Side::Viewer, message) => self.receive_viewer(message),
                Payload::Video { epoch, key } => {
                    let (disposition, actions) =
                        self.viewer.on_decoded_frame_at(epoch, key, self.now_us);
                    self.dispositions.push(disposition);
                    self.viewer_actions(actions);
                }
            }
        }
        panic!("scenario pump exceeded its bounded event budget");
    }

    fn receive_host(&mut self, message: ControlMessage) {
        if self.auth_reject && matches!(message, ControlMessage::Hello(_)) {
            let mut ack = host_caps();
            ack.status = HelloStatus::NotAuthorized;
            let actions = self.viewer.on_hello_ack_at(ack, self.now_us);
            self.viewer_actions(actions);
            return;
        }
        let actions = match message {
            ControlMessage::Hello(hello) => self.host.on_hello_at(&hello, self.now_us),
            ControlMessage::SwitchMonitor(request) => {
                self.host.on_control_activity(self.now_us);
                self.host.on_switch_monitor(request)
            }
            ControlMessage::RequestKeyframe(request) => {
                self.host.on_keyframe_request_at(request, self.now_us)
            }
            ControlMessage::PauseVideo(_) => {
                self.host.on_control_activity(self.now_us);
                self.host.on_pause()
            }
            ControlMessage::ResumeVideo(_) => {
                self.host.on_control_activity(self.now_us);
                self.host.on_resume()
            }
            ControlMessage::Ping(ping) => self.host.on_ping(ping, self.now_us),
            ControlMessage::Goodbye(goodbye) => self.host.on_goodbye(goodbye),
            ControlMessage::InputEvent(event) => self.host.on_input_event(event, self.now_us),
            _ => Vec::new(),
        };
        self.host_actions(actions);
    }

    fn receive_viewer(&mut self, message: ControlMessage) {
        match message {
            ControlMessage::HelloAck(ack) => {
                let actions = self.viewer.on_hello_ack_at(ack, self.now_us);
                self.viewer_actions(actions);
            }
            ControlMessage::TopologyAnnounce(topology) => {
                let actions = self.viewer.on_topology(topology);
                self.viewer_actions(actions);
            }
            ControlMessage::StreamReset(reset) => {
                let actions = self.viewer.on_stream_reset_at(reset, self.now_us);
                self.viewer_actions(actions);
            }
            ControlMessage::Pong(pong) => {
                self.viewer.on_pong(pong, self.now_us);
            }
            ControlMessage::Goodbye(_) => {}
            _ => {}
        }
    }

    fn complete_capture(&mut self, operation_id: u64) {
        self.pending_capture_ops.retain(|id| *id != operation_id);
        let actions = self
            .host
            .on_capture_result(operation_id, Ok(()), self.now_us);
        self.host_actions(actions);
        self.pump();
    }

    fn inject_to_viewer(&mut self, message: ControlMessage) {
        self.enqueue_control(Side::Viewer, message);
        self.pump();
    }

    fn inject_to_host(&mut self, message: ControlMessage) {
        self.enqueue_control(Side::Host, message);
        self.pump();
    }

    fn disconnect(&mut self) {
        let actions = self.viewer.on_disconnect(self.now_us);
        self.viewer_actions(actions);
        let actions = self.host.on_control_disconnected(self.now_us);
        self.host_actions(actions);
        self.pump();
    }

    fn count_actions(&mut self, count: usize) {
        self.action_count = self.action_count.saturating_add(count);
        if count > 32 {
            self.overflowed = true;
        }
    }
}

fn viewer_hello(version: u8) -> Hello {
    Hello {
        protocol_version: version,
        device_name: "fake viewer".to_owned(),
        os: OsType::Windows,
        app_version: "scenario-test".to_owned(),
        video_udp_port: 5000,
        codecs: 1,
        max_height: 1080,
        features: 0,
    }
}

fn host_caps() -> HelloAck {
    HelloAck {
        protocol_version: PROTOCOL_VERSION,
        status: HelloStatus::Ok,
        device_name: "fake host".to_owned(),
        os: OsType::Windows,
        app_version: "scenario-test".to_owned(),
        codecs: 1,
        max_height: 1080,
        features: 0,
        host_cpu_cores: 8,
    }
}

fn display(raw: u32, available: bool, primary: bool, width: u32, height: u32) -> Display {
    Display::new(
        DisplayId::new(raw).expect("test display id is nonzero"),
        format!("Test display {raw}"),
        (raw as i32 - 1) * 1920,
        0,
        width,
        height,
        1000,
        60_000,
        DisplayFlags::new(primary, true, available, false),
    )
}

fn topology(revision: u32, displays: Vec<Display>) -> Topology {
    Topology::new(revision, displays, None).expect("test topology is valid")
}

fn stream_reset(epoch: u16, display_id: u32, topology_rev: u32, req_id: u32) -> StreamReset {
    StreamReset {
        req_id,
        epoch,
        codec: StreamCodec::H264,
        width: 1280,
        height: 720,
        fps: 30,
        topology_rev,
        display_id,
        status: StreamStatus::Ok,
    }
}

#[test]
fn scenario_happy_path_connects_and_promotes_first_fake_keyframe() {
    let mut h = Harness::new();
    h.start();
    assert_eq!(h.viewer.phase(), ViewerPhase::Streaming);
    assert_eq!(
        h.viewer.selected_display(),
        Some(DisplayId::new(1).unwrap())
    );
    assert_eq!(h.viewer.epoch(), Some(1));
    assert_eq!(h.captured_display, Some(1));
    assert_eq!(h.dispositions.last(), Some(&VideoDisposition::Replace));
    assert!(!h.overflowed);
}

#[test]
fn scenario_selects_second_display_and_holds_then_replaces_keyframe() {
    let mut h = Harness::new();
    h.start();
    let old_epoch = h.viewer.epoch().unwrap();
    h.auto_capture = false;
    let actions = h.viewer.switch_display(2, 100);
    h.viewer_actions(actions);
    h.pump();
    assert_eq!(h.viewer.phase(), ViewerPhase::Switching);
    assert_eq!(
        h.viewer.on_decoded_frame(old_epoch, true).0,
        VideoDisposition::HoldLastFrame
    );
    let operation = *h.pending_capture_ops.last().expect("capture migration");
    h.complete_capture(operation);
    assert_eq!(
        h.viewer.selected_display(),
        Some(DisplayId::new(2).unwrap())
    );
    assert_eq!(h.viewer.phase(), ViewerPhase::Streaming);
    assert_eq!(h.viewer.epoch(), Some(old_epoch.wrapping_add(1)));
}

#[test]
fn scenario_display_disappears_mid_switch_restores_previous_display() {
    let mut h = Harness::new();
    h.start();
    h.auto_capture = false;
    let actions = h.viewer.switch_display(2, 100);
    h.viewer_actions(actions);
    h.pump();
    let updated = topology(2, vec![display(1, true, true, 1920, 1080)]);
    let actions = h.host.on_topology_update(updated, 200);
    h.host_actions(actions);
    h.pump();
    assert_eq!(h.viewer.phase(), ViewerPhase::Streaming);
    assert_eq!(
        h.viewer.selected_display(),
        Some(DisplayId::new(1).unwrap())
    );
    assert!(h
        .events
        .iter()
        .any(|event| matches!(event, SessionEvent::DisplayUnavailable(id) if id.get() == 2)));
}

#[test]
fn scenario_rapid_triple_switch_reorders_controls_and_ignores_stale_replies() {
    let mut h = Harness::new();
    h.start();
    h.auto_capture = false;
    let actions = h.viewer.switch_display(2, 100);
    h.viewer_actions(actions);
    h.reorder_next_control = true;
    let actions = h.viewer.switch_display(1, 101);
    h.viewer_actions(actions);
    h.reorder_next_control = true;
    let actions = h.viewer.switch_display(2, 102);
    h.viewer_actions(actions);
    h.pump();
    assert_eq!(h.pending_capture_ops.len(), 1);
    let latest = h.pending_capture_ops[0];
    let stale = h
        .host
        .on_capture_result(latest.wrapping_sub(1), Ok(()), h.now_us);
    assert!(stale.is_empty());
    h.complete_capture(latest);
    assert_eq!(h.host.selected_display(), Some(DisplayId::new(2).unwrap()));
    assert_eq!(
        h.viewer.selected_display(),
        Some(DisplayId::new(2).unwrap())
    );
    assert!(h.host.epoch() >= 2);
}

#[test]
fn scenario_switch_retry_then_failback_after_dropped_control() {
    let mut h = Harness::new();
    h.start();
    h.drop_switch_requests = 2;
    let actions = h.viewer.switch_display(2, 100);
    h.viewer_actions(actions);
    h.pump();
    h.advance_to(2_000_100);
    assert_eq!(h.dropped_switch_count, 2);
    h.advance_to(4_000_100);
    assert_eq!(h.viewer.phase(), ViewerPhase::Streaming);
    assert_eq!(
        h.viewer.selected_display(),
        Some(DisplayId::new(1).unwrap())
    );
}

#[test]
fn scenario_host_initiated_resolution_reset_reconfigures_streaming_display() {
    let mut h = Harness::new();
    h.start();
    let next = topology(
        2,
        vec![
            display(1, true, true, 1280, 720),
            display(2, true, false, 1280, 720),
        ],
    );
    let actions = h.host.on_topology_update(next, 100);
    h.host_actions(actions);
    h.pump();
    assert_eq!(h.viewer.phase(), ViewerPhase::Streaming);
    assert_eq!(
        h.viewer.selected_display(),
        Some(DisplayId::new(1).unwrap())
    );
    assert_eq!(h.viewer.epoch(), Some(2));
}

#[test]
fn scenario_host_reset_for_switch_target_is_accepted_during_switch() {
    let mut h = Harness::new();
    h.start();
    h.auto_capture = false;
    let actions = h.viewer.switch_display(2, 100);
    h.viewer_actions(actions);
    h.pump();
    let reset = stream_reset(2, 2, 1, 0);
    h.inject_to_viewer(ControlMessage::StreamReset(reset));
    h.enqueue_video(2, true);
    h.pump();
    assert_eq!(
        h.viewer.selected_display(),
        Some(DisplayId::new(2).unwrap())
    );
    assert_eq!(h.viewer.phase(), ViewerPhase::Streaming);
}

#[test]
fn scenario_pause_resume_and_unexpected_paused_frames_hold_last_image() {
    let mut h = Harness::new();
    h.start();
    let actions = h.viewer.on_pause();
    h.viewer_actions(actions);
    h.pump();
    let epoch = h.viewer.epoch().expect("paused epoch");
    let disposition = h.viewer.on_decoded_frame_at(epoch, true, h.now_us).0;
    assert_eq!(disposition, VideoDisposition::HoldLastFrame);
    let actions = h.viewer.on_resume();
    h.viewer_actions(actions);
    h.pump();
    assert_eq!(h.viewer.phase(), ViewerPhase::Streaming);
    assert_eq!(
        h.viewer.selected_display(),
        Some(DisplayId::new(1).unwrap())
    );
}

#[test]
fn scenario_control_loss_streaming_reconnects_and_resumes_selected_display() {
    let mut h = Harness::new();
    h.start();
    h.disconnect();
    assert_eq!(h.viewer.phase(), ViewerPhase::Reconnecting);
    h.advance_to(INITIAL_RECONNECT_DELAY_US);
    assert_eq!(h.reconnect_requests, 1);
    let actions = h.viewer.on_connect_at(h.now_us);
    h.viewer_actions(actions);
    h.pump();
    assert_eq!(h.viewer.phase(), ViewerPhase::Streaming);
    assert_eq!(
        h.viewer.selected_display(),
        Some(DisplayId::new(1).unwrap())
    );
}

#[test]
fn scenario_control_loss_during_switch_discards_pending_selection_on_reconnect() {
    let mut h = Harness::new();
    h.start();
    h.drop_switch_requests = 1;
    let actions = h.viewer.switch_display(2, 100);
    h.viewer_actions(actions);
    h.pump();
    assert_eq!(h.viewer.phase(), ViewerPhase::Switching);
    h.disconnect();
    h.advance_to(INITIAL_RECONNECT_DELAY_US);
    let actions = h.viewer.on_connect_at(h.now_us);
    h.viewer_actions(actions);
    h.pump();
    assert_eq!(h.viewer.phase(), ViewerPhase::Streaming);
    assert_eq!(
        h.viewer.selected_display(),
        Some(DisplayId::new(1).unwrap())
    );
}

#[test]
fn scenario_not_authorized_ends_without_reconnect() {
    let mut h = Harness::new();
    h.auth_reject = true;
    h.start();
    assert_eq!(h.viewer.phase(), ViewerPhase::Closed);
    assert!(h.viewer.tick(10_000_000).is_empty());
    assert_eq!(h.reconnect_requests, 0);
}

#[test]
fn scenario_busy_host_retries_after_five_seconds() {
    let mut h = Harness::new();
    h.start();
    let mut second = ViewerSession::new(viewer_hello(PROTOCOL_VERSION));
    let hello = match second.on_connect_at(h.now_us).as_slice() {
        [ViewerAction::SendControl(ControlMessage::Hello(hello))] => hello.clone(),
        other => panic!("unexpected second viewer connect: {other:?}"),
    };
    let actions = h.host.on_hello_at(&hello, h.now_us);
    let ack = actions
        .into_iter()
        .find_map(|action| match action {
            HostAction::SendControl(ControlMessage::HelloAck(ack)) => Some(ack),
            _ => None,
        })
        .expect("Busy response");
    assert_eq!(ack.status, HelloStatus::Busy);
    assert!(second
        .on_hello_ack_at(ack, h.now_us)
        .contains(&ViewerAction::DisconnectTransport));
    assert_eq!(second.phase(), ViewerPhase::Reconnecting);
    assert!(second.tick(BUSY_RETRY_DELAY_US - 1).is_empty());
    assert_eq!(
        second.tick(BUSY_RETRY_DELAY_US),
        vec![ViewerAction::ReconnectTransport]
    );
}

#[test]
fn scenario_version_mismatch_is_terminal() {
    let mut h = Harness::with_viewer_version(PROTOCOL_VERSION.wrapping_add(1));
    h.start();
    assert_eq!(h.viewer.phase(), ViewerPhase::Closed);
    assert_eq!(h.reconnect_requests, 0);
}

#[test]
fn scenario_capture_loss_recovers_after_a_topology_change() {
    let mut h = Harness::new();
    h.start();
    h.now_us = 100;
    let actions = h.host.on_capture_lost(RecoveryReason::AccessLost, h.now_us);
    h.host_actions(actions);
    h.pump();
    let changed = topology(
        2,
        vec![
            display(1, true, true, 1920, 1080),
            display(2, true, false, 1280, 720),
            display(3, true, false, 1024, 768),
        ],
    );
    let actions = h.host.on_topology_update(changed, 200);
    h.host_actions(actions);
    h.pump();
    h.advance_to(50_100);
    assert_eq!(h.viewer.phase(), ViewerPhase::Streaming);
    assert_eq!(
        h.viewer.selected_display(),
        Some(DisplayId::new(1).unwrap())
    );
    assert_eq!(h.viewer.epoch(), Some(h.host.epoch()));
}

#[test]
fn scenario_hardware_encoder_failures_fall_back_then_report_software_failure() {
    let mut h = Harness::new();
    h.start();
    let a1 = h.host.on_encoder_failure_at(10);
    h.host_actions(a1);
    let a2 = h.host.on_encoder_failure_at(20);
    h.host_actions(a2);
    let a3 = h.host.on_encoder_failure_at(30);
    assert!(a3.iter().any(|action| matches!(
        action,
        HostAction::Event(SessionEvent::EncoderFallbackToSoftware)
    )));
    h.host_actions(a3);
    let (operation, software) = h.pending_rebuild.expect("software fallback request");
    assert!(software);
    let failed = h.host.on_encoder_rebuild_result(operation, false);
    assert!(failed
        .iter()
        .any(|action| matches!(action, HostAction::Event(SessionEvent::EncoderFailed))));
    h.host_actions(failed);
    h.pump();
}

#[test]
fn scenario_first_keyframe_nudge_and_decoder_reset_use_virtual_time() {
    let mut h = Harness::new();
    h.drop_video_frames = 3;
    h.start();
    assert_eq!(h.viewer.phase(), ViewerPhase::Switching);
    h.advance_to(FIRST_KEYFRAME_NUDGE_US);
    h.advance_to(FIRST_KEYFRAME_RESET_US);
    assert!(h.decoder_reset_count >= 2);
    assert!(h.keyframe_count >= 3);
}

#[test]
fn scenario_forced_idr_requests_are_coalesced_to_one_frame() {
    let mut h = Harness::new();
    h.start();
    let epoch = h.host.epoch();
    h.now_us = 10_000;
    h.inject_to_host(ControlMessage::RequestKeyframe(RequestKeyframe { epoch }));
    h.now_us = 20_000;
    h.inject_to_host(ControlMessage::RequestKeyframe(RequestKeyframe { epoch }));
    assert_eq!(h.keyframe_count, 2); // Initial IDR plus one coalesced follow-up.
    h.advance_to(FORCE_IDR_MIN_INTERVAL_US + 10_000);
    assert_eq!(h.keyframe_count, 3);
}

#[test]
fn scenario_delayed_control_message_is_delivered_at_virtual_deadline() {
    let mut h = Harness::new();
    h.start();
    let epoch = h.host.epoch();
    let initial_keyframes = h.keyframe_count;
    h.now_us = 100_000;
    h.delay_next_control_us = Some(25_000);
    h.inject_to_host(ControlMessage::RequestKeyframe(RequestKeyframe { epoch }));
    assert_eq!(h.keyframe_count, initial_keyframes);
    assert_eq!(h.queue.len(), 1);
    h.advance_to(124_999);
    assert_eq!(h.keyframe_count, initial_keyframes);
    h.advance_to(125_000);
    assert_eq!(h.keyframe_count, initial_keyframes + 1);
    assert!(!h.overflowed);
}

#[test]
fn scenario_orphan_timeout_stops_stream_at_owner_selected_five_seconds() {
    let mut h = Harness::new();
    h.start();
    let actions = h.host.on_control_disconnected(0);
    h.host_actions(actions);
    h.advance_to(HOST_ORPHAN_TIMEOUT_US - 1);
    assert!(!h.capture_stopped);
    h.advance_to(HOST_ORPHAN_TIMEOUT_US);
    assert!(h.capture_stopped);
    assert_eq!(h.host.phase(), HostPhase::Closed);
}

#[test]
fn scenario_stuck_key_is_released_when_control_disconnects() {
    let mut h = Harness::new();
    h.start();
    let epoch = h.viewer.epoch().unwrap();
    let key = InputEvent {
        epoch,
        display_id: 1,
        event: InputEventKind::Key {
            hid_usage: 0x04,
            pressed: true,
            modifiers: 0,
        },
    };
    h.viewer_actions(h.viewer.on_user_input(key));
    h.pump();
    assert!(h.injected_inputs.contains(&key));
    h.disconnect();
    assert!(h.injected_inputs.contains(&InputEvent {
        epoch,
        display_id: 1,
        event: InputEventKind::Key {
            hid_usage: 0x04,
            pressed: false,
            modifiers: 0
        }
    }));
}
