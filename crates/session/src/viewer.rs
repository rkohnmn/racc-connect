use crate::{
    SessionEvent, HEARTBEAT_INTERVAL_US, HEARTBEAT_TIMEOUT_US, KEYFRAME_REQUEST_MIN_INTERVAL_US,
    MAX_STREAM_HEIGHT_PX, MAX_STREAM_WIDTH_PX, STREAM_FPS,
};
use racc_proto::{
    ControlMessage, Goodbye, GoodbyeReason, Hello, HelloAck, HelloStatus, InputEvent,
    InputEventKind, PauseVideo, Ping, Pong, RequestKeyframe, ResumeVideo, StreamCodec, StreamReset,
    StreamStatus, SwitchMonitor, TopologyAnnounce, MAX_DISPLAYS,
};
use racc_telemetry::{PingTracker, RttEstimator, RttSnapshot};
use racc_topology::{is_revision_newer, DisplayId};

/// Required viewer reconnect schedule in microseconds; the final delay repeats.
pub const RECONNECT_BACKOFF_US: [u64; 5] = [500_000, 1_000_000, 2_000_000, 4_000_000, 5_000_000];
/// Delay before the first viewer reconnect attempt.
pub const INITIAL_RECONNECT_DELAY_US: u64 = crate::RECONNECT_BACKOFF_MS[0] * 1_000;
/// Maximum delay between viewer reconnect attempts.
pub const MAX_RECONNECT_DELAY_US: u64 = crate::RECONNECT_BACKOFF_MS[4] * 1_000;
/// Time allowed for HelloAck and the initial topology handshake.
pub const HANDSHAKE_TIMEOUT_US: u64 = crate::HANDSHAKE_TIMEOUT_MS * 1_000;

/// Switch retries once after this timeout before falling back to the old display.
pub const SWITCH_TIMEOUT_US: u64 = crate::SWITCH_TIMEOUT_MS * 1_000;
/// First keyframe nudge delay after a successful StreamReset.
pub const FIRST_KEYFRAME_NUDGE_US: u64 = crate::FIRST_KEYFRAME_NUDGE_MS * 1_000;
/// Decoder reset and repeated keyframe request delay.
pub const FIRST_KEYFRAME_RESET_US: u64 = crate::FIRST_KEYFRAME_RESET_MS * 1_000;
/// Busy host response retry delay.
pub const BUSY_RETRY_DELAY_MS: u64 = 5_000;
/// Busy host response retry delay, in microseconds.
pub const BUSY_RETRY_DELAY_US: u64 = BUSY_RETRY_DELAY_MS * 1_000;

/// Viewer-side lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewerPhase {
    /// No control connection is active.
    Disconnected,
    /// Waiting for the host handshake response.
    AwaitingHelloAck,
    /// Handshake succeeded; waiting for the host topology.
    AwaitingTopology,
    /// Topology is known; waiting for a stream configuration.
    AwaitingStream,
    /// A stream is active and presenting decoded frames.
    Streaming,
    /// A monitor switch is in progress and the last good frame is held.
    Switching,
    /// A decoder reset, resume, or stream reset needs a keyframe.
    AwaitingKeyframe,
    /// Video is paused while the control session remains connected.
    Paused,
    /// The control connection was lost and a reconnect is scheduled.
    Reconnecting,
    /// The session was explicitly closed.
    Closed,
}

/// Decision for a decoded frame at the renderer handoff.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoDisposition {
    /// Accept the frame for the currently displayed stream.
    Present,
    /// Atomically replace the displayed frame with this matching keyframe.
    Replace,
    /// Discard this frame and keep displaying the previous good frame.
    HoldLastFrame,
    /// Discard this frame because it is stale or no stream can accept it.
    Drop,
}

/// Typed work for the caller of the deterministic viewer state machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewerAction {
    /// Send a protocol message on the reliable control channel.
    SendControl(ControlMessage),
    /// Record a session lifecycle event.
    Event(SessionEvent),
    /// Recreate the decoder for the specified stream epoch.
    ResetDecoder {
        /// Epoch the new decoder must accept.
        epoch: u16,
    },
    /// Ask the transport layer to attempt a control connection now.
    ReconnectTransport,
    /// Ask the transport layer to close the active control connection.
    DisconnectTransport,
    /// Forward a keyboard, wheel, or currently mapped pointer event to the host.
    ForwardInput(InputEvent),
}

/// Deterministic exponential reconnect schedule.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Backoff {
    attempts: u32,
    retry_at_us: Option<u64>,
}

impl Backoff {
    /// Returns the number of reconnect attempts scheduled since the last reset.
    pub const fn attempts(self) -> u32 {
        self.attempts
    }

    /// Returns the next reconnect deadline, if one is pending.
    pub const fn retry_at_us(self) -> Option<u64> {
        self.retry_at_us
    }

    /// Schedules the next retry and returns its delay in microseconds.
    pub fn schedule(&mut self, now_us: u64) -> u64 {
        let schedule_index = (self.attempts as usize).min(RECONNECT_BACKOFF_US.len() - 1);
        let delay = RECONNECT_BACKOFF_US[schedule_index];
        self.attempts = self.attempts.saturating_add(1);
        self.retry_at_us = Some(now_us.saturating_add(delay));
        delay
    }

    /// Clears the current deadline when its retry is started.
    pub fn take_due(&mut self, now_us: u64) -> bool {
        if self.retry_at_us.is_some_and(|deadline| now_us >= deadline) {
            self.retry_at_us = None;
            true
        } else {
            false
        }
    }

    /// Clears the retry schedule after a successful connection.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Viewer-side control and video lifecycle controller.
///
/// The caller owns sockets, the decoder, and the renderer. This type only
/// validates lifecycle messages and returns typed actions. Pixel data never
/// enters this state machine; VideoDisposition::Replace tells the renderer
/// when it can atomically promote a decoded keyframe.
#[derive(Clone, Debug)]
pub struct ViewerSession {
    hello: Hello,
    phase: ViewerPhase,
    ever_connected: bool,
    backoff: Backoff,
    topology: Option<TopologyAnnounce>,
    selected_display: Option<DisplayId>,
    invalidated_display: Option<DisplayId>,
    pending_display: Option<DisplayId>,
    pending_switch_req: Option<u32>,
    switch_reset_received: bool,
    next_req_id: u32,
    stream_epoch: Option<u16>,
    rendered_epoch: Option<u16>,
    waiting_for_keyframe: bool,
    require_stream_reset: bool,
    video_paused: bool,
    last_keyframe_request_us: Option<u64>,
    clock_now_us: u64,
    handshake_deadline_us: Option<u64>,
    busy_retry_at_us: Option<u64>,
    last_control_activity_us: Option<u64>,
    next_heartbeat_us: Option<u64>,
    next_ping_nonce: u64,
    switch_deadline_us: Option<u64>,
    switch_retries: u8,
    keyframe_nudge_at_us: Option<u64>,
    keyframe_reset_at_us: Option<u64>,
    ping_tracker: PingTracker,
    rtt_estimator: RttEstimator,
}

impl ViewerSession {
    /// Creates a disconnected viewer controller using local handshake capabilities.
    pub fn new(hello: Hello) -> Self {
        Self {
            hello,
            phase: ViewerPhase::Disconnected,
            ever_connected: false,
            backoff: Backoff::default(),
            topology: None,
            selected_display: None,
            invalidated_display: None,
            pending_display: None,
            pending_switch_req: None,
            switch_reset_received: false,
            next_req_id: 1,
            stream_epoch: None,
            rendered_epoch: None,
            waiting_for_keyframe: true,
            require_stream_reset: false,
            video_paused: false,
            last_keyframe_request_us: None,
            clock_now_us: 0,
            handshake_deadline_us: None,
            busy_retry_at_us: None,
            last_control_activity_us: None,
            next_heartbeat_us: None,
            next_ping_nonce: 1,
            switch_deadline_us: None,
            switch_retries: 0,
            keyframe_nudge_at_us: None,
            keyframe_reset_at_us: None,
            ping_tracker: PingTracker::new(),
            rtt_estimator: RttEstimator::default(),
        }
    }

    /// Returns the current viewer lifecycle phase.
    pub const fn phase(&self) -> ViewerPhase {
        self.phase
    }

    /// Returns the stream epoch currently expected from the decoder.
    pub const fn epoch(&self) -> Option<u16> {
        self.stream_epoch
    }

    /// Returns the selected display whose keyframe has been promoted.
    pub const fn selected_display(&self) -> Option<DisplayId> {
        self.selected_display
    }

    /// Returns the display being prepared for promotion, if any.
    pub const fn pending_display(&self) -> Option<DisplayId> {
        self.pending_display
    }

    /// Returns whether a rendered frame is available to hold on screen.
    pub const fn has_last_good_frame(&self) -> bool {
        self.rendered_epoch.is_some()
    }

    /// Returns the current reconnect schedule.
    pub const fn backoff(&self) -> Backoff {
        self.backoff
    }

    /// Records a valid control message and refreshes heartbeat timing.
    pub fn on_control_activity_at(&mut self, now_us: u64) {
        self.clock_now_us = now_us;
        self.last_control_activity_us = Some(now_us);
        self.next_heartbeat_us = Some(now_us.saturating_add(HEARTBEAT_INTERVAL_US));
    }

    /// Matches a Pong, updates RTT estimation, and returns the measured RTT.
    pub fn on_pong(&mut self, pong: Pong, now_us: u64) -> Option<u64> {
        self.on_control_activity_at(now_us);
        let rtt_us = self
            .ping_tracker
            .match_pong(pong.nonce, pong.echo_ts_us, now_us)?;
        self.rtt_estimator.record(now_us, rtt_us);
        Some(rtt_us)
    }

    /// Returns a bounded RTT snapshot at the supplied virtual timestamp.
    pub fn rtt_snapshot(&mut self, now_us: u64) -> RttSnapshot {
        self.rtt_estimator.snapshot(now_us)
    }

    /// Starts a control handshake after the transport reports a connection.
    pub fn on_connect(&mut self) -> Vec<ViewerAction> {
        self.on_connect_at(self.clock_now_us)
    }

    /// Starts a handshake and arms its deadline using injected virtual time.
    pub fn on_connect_at(&mut self, now_us: u64) -> Vec<ViewerAction> {
        if self.phase == ViewerPhase::Closed {
            return Vec::new();
        }
        self.clock_now_us = now_us;
        self.last_control_activity_us = Some(now_us);
        self.next_heartbeat_us = None;
        self.handshake_deadline_us = Some(now_us.saturating_add(HANDSHAKE_TIMEOUT_US));
        self.busy_retry_at_us = None;
        self.backoff.retry_at_us = None;
        self.phase = ViewerPhase::AwaitingHelloAck;
        vec![ViewerAction::SendControl(ControlMessage::Hello(
            self.hello.clone(),
        ))]
    }

    /// Processes the host handshake response.
    pub fn on_hello_ack(&mut self, ack: HelloAck) -> Vec<ViewerAction> {
        self.on_hello_ack_at(ack, self.clock_now_us)
    }

    /// Processes the handshake response and updates the injected control clock.
    pub fn on_hello_ack_at(&mut self, ack: HelloAck, now_us: u64) -> Vec<ViewerAction> {
        if self.phase != ViewerPhase::AwaitingHelloAck {
            return Vec::new();
        }
        self.on_control_activity_at(now_us);
        self.handshake_deadline_us = None;
        if ack.status == HelloStatus::Busy {
            self.phase = ViewerPhase::Reconnecting;
            self.busy_retry_at_us = Some(now_us.saturating_add(BUSY_RETRY_DELAY_US));
            self.backoff.retry_at_us = None;
            return vec![ViewerAction::DisconnectTransport];
        }
        let accepted = ack.status == HelloStatus::Ok
            && ack.protocol_version == self.hello.protocol_version
            && ack.codecs & 1 != 0;
        if !accepted {
            self.phase = ViewerPhase::Closed;
            return vec![ViewerAction::DisconnectTransport];
        }
        self.phase = ViewerPhase::AwaitingTopology;
        self.backoff.reset();
        self.busy_retry_at_us = None;
        let event = if self.ever_connected {
            SessionEvent::Reconnected
        } else {
            SessionEvent::Connected
        };
        self.ever_connected = true;
        let mut actions = vec![ViewerAction::Event(event)];
        if self.video_paused {
            actions.push(ViewerAction::SendControl(ControlMessage::PauseVideo(
                PauseVideo,
            )));
        }
        actions
    }

    /// Accepts a topology announcement after validating its bounded invariants.
    /// Duplicate, stale, and invalid announcements are ignored.
    pub fn on_topology(&mut self, incoming: TopologyAnnounce) -> Vec<ViewerAction> {
        if !matches!(
            self.phase,
            ViewerPhase::AwaitingTopology
                | ViewerPhase::AwaitingStream
                | ViewerPhase::Streaming
                | ViewerPhase::Switching
                | ViewerPhase::AwaitingKeyframe
                | ViewerPhase::Paused
        ) || !valid_topology(&incoming)
        {
            return Vec::new();
        }
        if let Some(current) = &self.topology {
            if !is_revision_newer(incoming.topology_rev, current.topology_rev) {
                return Vec::new();
            }
        }
        self.topology = Some(incoming);
        let mut actions = Vec::new();
        if let Some(selected) = self.selected_display {
            if !self.display_is_available(selected) {
                self.invalidated_display = Some(selected);
                self.selected_display = None;
                if self.pending_switch_req.is_none() {
                    self.phase = if self.video_paused {
                        ViewerPhase::Paused
                    } else {
                        ViewerPhase::AwaitingStream
                    };
                }
                actions.push(ViewerAction::Event(SessionEvent::DisplayUnavailable(
                    selected,
                )));
            }
        }
        if let Some(pending) = self.pending_display {
            if !self.display_is_available(pending) {
                self.pending_display = None;
                self.pending_switch_req = None;
                self.switch_reset_received = false;
                self.waiting_for_keyframe = false;
                self.keyframe_nudge_at_us = None;
                self.keyframe_reset_at_us = None;
                self.switch_deadline_us = None;
                self.switch_retries = 0;
                self.phase = if self.video_paused {
                    ViewerPhase::Paused
                } else if self.stream_epoch.is_some() && self.selected_display.is_some() {
                    ViewerPhase::Streaming
                } else {
                    ViewerPhase::AwaitingStream
                };
                actions.push(ViewerAction::Event(SessionEvent::DisplayUnavailable(
                    pending,
                )));
            }
        }
        if self.phase == ViewerPhase::AwaitingTopology {
            self.phase = if self.video_paused {
                ViewerPhase::Paused
            } else {
                ViewerPhase::AwaitingStream
            };
        }
        actions
    }

    /// Requests a monitor switch while keeping the last good frame displayed.
    /// A newer switch supersedes an unfinished request.
    pub fn switch_display(&mut self, display_id: u32, now_us: u64) -> Vec<ViewerAction> {
        if !matches!(
            self.phase,
            ViewerPhase::Streaming | ViewerPhase::Switching | ViewerPhase::AwaitingStream
        ) || self.video_paused
        {
            return Vec::new();
        }
        let Some(display) = DisplayId::new(display_id) else {
            return Vec::new();
        };
        if !self.display_is_available(display) {
            return vec![ViewerAction::Event(SessionEvent::DisplayUnavailable(
                display,
            ))];
        }
        if self.selected_display == Some(display)
            && self.pending_switch_req.is_none()
            && self.phase == ViewerPhase::Streaming
        {
            return Vec::new();
        }
        let req_id = self.allocate_req_id();
        self.pending_display = Some(display);
        self.pending_switch_req = Some(req_id);
        self.switch_deadline_us = Some(now_us.saturating_add(SWITCH_TIMEOUT_US));
        self.switch_retries = 0;
        self.switch_reset_received = false;
        self.waiting_for_keyframe = true;
        self.phase = ViewerPhase::Switching;
        vec![ViewerAction::SendControl(ControlMessage::SwitchMonitor(
            SwitchMonitor { req_id, display_id },
        ))]
    }

    /// Processes a host stream reset, ignoring stale epochs and stale switch results.
    pub fn on_stream_reset(&mut self, reset: StreamReset) -> Vec<ViewerAction> {
        self.on_stream_reset_at(reset, self.clock_now_us)
    }

    /// Processes a stream reset at the supplied virtual time.
    pub fn on_stream_reset_at(&mut self, reset: StreamReset, now_us: u64) -> Vec<ViewerAction> {
        self.on_control_activity_at(now_us);
        if matches!(
            self.phase,
            ViewerPhase::Disconnected
                | ViewerPhase::AwaitingHelloAck
                | ViewerPhase::Reconnecting
                | ViewerPhase::Closed
        ) {
            return Vec::new();
        }
        if self
            .topology
            .as_ref()
            .is_none_or(|topology| topology.topology_rev != reset.topology_rev)
        {
            return Vec::new();
        }
        let terminal_encoder_failure =
            reset.status == StreamStatus::EncoderFailed && reset.req_id == 0;
        if terminal_encoder_failure && !self.epoch_is_new(reset.epoch) {
            return Vec::new();
        }
        let selected_invalidation = self.is_selected_display_invalidation(&reset);
        if !self.reset_matches_request(&reset)
            && !selected_invalidation
            && !terminal_encoder_failure
        {
            return Vec::new();
        }
        if !self.reset_display_matches(&reset) {
            return Vec::new();
        }
        if reset.status != StreamStatus::Ok {
            if reset.status == StreamStatus::Paused {
                if !self.epoch_is_new(reset.epoch) {
                    return Vec::new();
                }
                self.stream_epoch = Some(reset.epoch);
                self.keyframe_nudge_at_us = Some(now_us.saturating_add(FIRST_KEYFRAME_NUDGE_US));
                self.keyframe_reset_at_us = Some(now_us.saturating_add(FIRST_KEYFRAME_RESET_US));
                self.switch_deadline_us = None;
                self.waiting_for_keyframe = true;
                self.require_stream_reset = false;
                self.switch_reset_received = self.pending_display.is_some();
                // Recovery follows this acknowledged switch reset with req_id zero.
                if self.pending_switch_req.is_some() {
                    self.pending_switch_req = None;
                }
                self.phase = ViewerPhase::Paused;
                return vec![ViewerAction::ResetDecoder { epoch: reset.epoch }];
            }
            let mut actions = Vec::new();
            if reset.status == StreamStatus::EncoderFailed {
                actions.push(ViewerAction::Event(SessionEvent::EncoderFailed));
            }
            if terminal_encoder_failure {
                self.stream_epoch = Some(reset.epoch);
                self.keyframe_nudge_at_us = Some(now_us.saturating_add(FIRST_KEYFRAME_NUDGE_US));
                self.keyframe_reset_at_us = Some(now_us.saturating_add(FIRST_KEYFRAME_RESET_US));
                self.switch_deadline_us = None;
                self.require_stream_reset = true;
            }
            if reset.status == StreamStatus::DisplayNotFound {
                if let Some(display) = DisplayId::new(reset.display_id) {
                    if self.invalidated_display != Some(display)
                        && self.selected_display != Some(display)
                    {
                        actions.push(ViewerAction::Event(SessionEvent::DisplayUnavailable(
                            display,
                        )));
                    }
                    if self.is_selected_display_invalidation(&reset) {
                        self.stream_epoch = Some(reset.epoch);
                        self.keyframe_nudge_at_us =
                            Some(now_us.saturating_add(FIRST_KEYFRAME_NUDGE_US));
                        self.keyframe_reset_at_us =
                            Some(now_us.saturating_add(FIRST_KEYFRAME_RESET_US));
                        self.switch_deadline_us = None;
                        self.selected_display = None;
                        self.invalidated_display = None;
                        self.waiting_for_keyframe = true;
                        self.require_stream_reset = true;
                        self.switch_reset_received = false;
                        if self.pending_switch_req.is_some() {
                            self.phase = ViewerPhase::Switching;
                        } else {
                            self.pending_display = None;
                            self.phase = if self.video_paused {
                                ViewerPhase::Paused
                            } else {
                                ViewerPhase::AwaitingStream
                            };
                        }
                        return actions;
                    }
                }
            }
            self.pending_display = None;
            self.pending_switch_req = None;
            self.switch_reset_received = false;
            self.waiting_for_keyframe = terminal_encoder_failure;
            self.phase = if self.video_paused {
                ViewerPhase::Paused
            } else if self.stream_epoch.is_some() && self.selected_display.is_some() {
                ViewerPhase::Streaming
            } else {
                ViewerPhase::AwaitingStream
            };
            return actions;
        }

        if !self.epoch_is_new(reset.epoch) {
            return Vec::new();
        }
        let Some(display) = DisplayId::new(reset.display_id) else {
            return Vec::new();
        };
        if self
            .pending_display
            .or(self.selected_display)
            .is_some_and(|expected| expected != display)
        {
            return Vec::new();
        }
        if reset.codec != StreamCodec::H264
            || reset.fps != STREAM_FPS
            || reset.width == 0
            || reset.height == 0
            || reset.width > MAX_STREAM_WIDTH_PX
            || reset.height > MAX_STREAM_HEIGHT_PX
            || !self.display_is_available(display)
        {
            return Vec::new();
        }
        self.stream_epoch = Some(reset.epoch);
        self.keyframe_nudge_at_us = Some(now_us.saturating_add(FIRST_KEYFRAME_NUDGE_US));
        self.keyframe_reset_at_us = Some(now_us.saturating_add(FIRST_KEYFRAME_RESET_US));
        self.switch_deadline_us = None;
        self.invalidated_display = None;
        self.pending_display = Some(display);
        self.switch_reset_received = true;
        self.waiting_for_keyframe = true;
        self.require_stream_reset = false;
        if !self.video_paused {
            self.phase =
                if self.pending_switch_req.is_some() || self.selected_display != Some(display) {
                    ViewerPhase::Switching
                } else {
                    ViewerPhase::AwaitingKeyframe
                };
        }
        vec![ViewerAction::ResetDecoder { epoch: reset.epoch }]
    }

    /// Decides whether a decoded frame is presented, held, dropped, or promoted.
    /// The action list carries metadata only; pixel data stays on the renderer path.
    pub fn on_decoded_frame(
        &mut self,
        epoch: u16,
        is_keyframe: bool,
    ) -> (VideoDisposition, Vec<ViewerAction>) {
        self.on_decoded_frame_at(epoch, is_keyframe, self.clock_now_us)
    }

    /// Handles a decoded frame with an explicit virtual timestamp.
    pub fn on_decoded_frame_at(
        &mut self,
        epoch: u16,
        is_keyframe: bool,
        now_us: u64,
    ) -> (VideoDisposition, Vec<ViewerAction>) {
        self.clock_now_us = now_us;
        let Some(current_epoch) = self.stream_epoch else {
            return (VideoDisposition::Drop, Vec::new());
        };
        if epoch != current_epoch {
            return (VideoDisposition::Drop, Vec::new());
        }
        if self.video_paused || self.require_stream_reset {
            return (self.hold_or_drop(), Vec::new());
        }
        if matches!(
            self.phase,
            ViewerPhase::Disconnected
                | ViewerPhase::AwaitingHelloAck
                | ViewerPhase::AwaitingTopology
                | ViewerPhase::AwaitingStream
                | ViewerPhase::Reconnecting
                | ViewerPhase::Closed
                | ViewerPhase::Paused
        ) {
            return (self.hold_or_drop(), Vec::new());
        }
        if self.pending_switch_req.is_some() && !self.switch_reset_received {
            return (self.hold_or_drop(), Vec::new());
        }
        if self.waiting_for_keyframe {
            if !is_keyframe {
                return (self.hold_or_drop(), Vec::new());
            }
            let previous_display = self.selected_display;
            let promoted = self.pending_display.or(self.selected_display);
            self.selected_display = promoted;
            self.pending_display = None;
            self.pending_switch_req = None;
            self.switch_reset_received = false;
            self.rendered_epoch = Some(epoch);
            self.waiting_for_keyframe = false;
            self.keyframe_nudge_at_us = None;
            self.keyframe_reset_at_us = None;
            self.switch_deadline_us = None;
            self.switch_retries = 0;
            self.phase = ViewerPhase::Streaming;
            let mut actions = Vec::new();
            if promoted.is_some() && promoted != previous_display {
                if let Some(display) = promoted {
                    actions.push(ViewerAction::Event(SessionEvent::DisplaySelected(display)));
                }
            }
            return (VideoDisposition::Replace, actions);
        }
        if self.phase == ViewerPhase::Switching {
            return (self.hold_or_drop(), Vec::new());
        }
        self.rendered_epoch = Some(epoch);
        (VideoDisposition::Present, Vec::new())
    }

    /// Pauses video while leaving the control channel active.
    pub fn on_pause(&mut self) -> Vec<ViewerAction> {
        if matches!(
            self.phase,
            ViewerPhase::Disconnected
                | ViewerPhase::AwaitingHelloAck
                | ViewerPhase::Reconnecting
                | ViewerPhase::Closed
                | ViewerPhase::Paused
        ) {
            return Vec::new();
        }
        self.video_paused = true;
        self.phase = ViewerPhase::Paused;
        vec![ViewerAction::SendControl(ControlMessage::PauseVideo(
            PauseVideo,
        ))]
    }

    /// Forwards input that is valid for the active stream mapping.
    ///
    /// During a display switch, pointer movement and buttons are dropped while
    /// keyboard and wheel input continue against the newest adopted epoch.
    pub fn on_user_input(&self, event: InputEvent) -> Vec<ViewerAction> {
        if self.video_paused
            || !matches!(
                self.phase,
                ViewerPhase::Streaming | ViewerPhase::Switching | ViewerPhase::AwaitingKeyframe
            )
        {
            return Vec::new();
        }
        let Some(epoch) = self.stream_epoch else {
            return Vec::new();
        };
        let target_display = if self.switch_reset_received {
            self.pending_display.or(self.selected_display)
        } else {
            self.selected_display.or(self.pending_display)
        };
        let Some(display) = target_display else {
            return Vec::new();
        };
        let pointer_event = matches!(
            event.event,
            InputEventKind::MouseMoveAbs { .. }
                | InputEventKind::MouseMoveRel { .. }
                | InputEventKind::MouseButton { .. }
        );
        if pointer_event && self.phase != ViewerPhase::Streaming {
            return Vec::new();
        }
        let display_matches = event.display_id == display.get()
            || self
                .selected_display
                .is_some_and(|selected| event.display_id == selected.get());
        let epoch_matches = event.epoch == epoch || self.rendered_epoch == Some(event.epoch);
        if !display_matches || !epoch_matches {
            return Vec::new();
        }
        vec![ViewerAction::ForwardInput(InputEvent {
            epoch,
            display_id: display.get(),
            event: event.event,
        })]
    }

    /// Resumes video and waits for a fresh host stream reset and keyframe.
    pub fn on_resume(&mut self) -> Vec<ViewerAction> {
        if self.phase != ViewerPhase::Paused {
            return Vec::new();
        }
        self.video_paused = false;
        self.require_stream_reset = true;
        self.waiting_for_keyframe = true;
        if self.switch_reset_received {
            self.pending_switch_req = None;
        }
        self.phase = ViewerPhase::AwaitingKeyframe;
        vec![ViewerAction::SendControl(ControlMessage::ResumeVideo(
            ResumeVideo,
        ))]
    }

    /// Handles a dropped/incomplete video frame and rate-limits keyframe requests.
    pub fn on_packet_loss(&mut self, now_us: u64) -> Vec<ViewerAction> {
        if self.video_paused || self.stream_epoch.is_none() || self.require_stream_reset {
            return Vec::new();
        }
        self.waiting_for_keyframe = true;
        if self.phase != ViewerPhase::Switching {
            self.phase = ViewerPhase::AwaitingKeyframe;
        }
        self.request_keyframe_if_allowed(now_us)
    }

    /// Resets a failed decoder, keeps the last good frame, and asks for an IDR.
    pub fn on_decoder_error(&mut self, now_us: u64) -> Vec<ViewerAction> {
        let mut actions = vec![ViewerAction::Event(SessionEvent::DecoderReset)];
        let Some(epoch) = self.stream_epoch else {
            return actions;
        };
        self.waiting_for_keyframe = true;
        if self.phase != ViewerPhase::Switching && !self.video_paused {
            self.phase = ViewerPhase::AwaitingKeyframe;
        }
        actions.push(ViewerAction::ResetDecoder { epoch });
        actions.extend(self.request_keyframe_if_allowed(now_us));
        actions
    }

    /// Marks the control channel lost and starts the bounded reconnect schedule.
    pub fn on_disconnect(&mut self, now_us: u64) -> Vec<ViewerAction> {
        if self.phase == ViewerPhase::Closed {
            return Vec::new();
        }
        if self.phase == ViewerPhase::Reconnecting {
            if self.backoff.retry_at_us().is_none() {
                self.backoff.schedule(now_us);
            }
            return Vec::new();
        }
        self.phase = ViewerPhase::Reconnecting;
        self.topology = None;
        self.invalidated_display = None;
        self.stream_epoch = None;
        self.pending_display = None;
        self.pending_switch_req = None;
        self.switch_reset_received = false;
        self.waiting_for_keyframe = true;
        self.require_stream_reset = false;
        self.backoff.schedule(now_us);
        vec![ViewerAction::Event(SessionEvent::Reconnecting)]
    }

    /// Advances handshake, heartbeat, switch, keyframe, Busy, and reconnect timers.
    pub fn tick(&mut self, now_us: u64) -> Vec<ViewerAction> {
        self.clock_now_us = now_us;
        if self.phase == ViewerPhase::Closed {
            return Vec::new();
        }
        if self.phase == ViewerPhase::Reconnecting {
            if self
                .busy_retry_at_us
                .is_some_and(|deadline| now_us >= deadline)
            {
                self.busy_retry_at_us = None;
                return vec![ViewerAction::ReconnectTransport];
            }
            if self.backoff.take_due(now_us) {
                return vec![ViewerAction::ReconnectTransport];
            }
            return Vec::new();
        }
        let handshake_timed_out = matches!(
            self.phase,
            ViewerPhase::AwaitingHelloAck | ViewerPhase::AwaitingTopology
        ) && self
            .handshake_deadline_us
            .is_some_and(|deadline| now_us >= deadline);
        let heartbeat_timed_out = !matches!(
            self.phase,
            ViewerPhase::Disconnected | ViewerPhase::AwaitingHelloAck | ViewerPhase::Closed
        ) && self
            .last_control_activity_us
            .is_some_and(|last| now_us.saturating_sub(last) >= HEARTBEAT_TIMEOUT_US);
        if handshake_timed_out || heartbeat_timed_out {
            let mut actions = self.on_disconnect(now_us);
            actions.insert(0, ViewerAction::DisconnectTransport);
            return actions;
        }

        let mut actions = Vec::new();
        if self
            .next_heartbeat_us
            .is_some_and(|deadline| now_us >= deadline)
            && !matches!(
                self.phase,
                ViewerPhase::Disconnected | ViewerPhase::AwaitingHelloAck | ViewerPhase::Closed
            )
        {
            let nonce = self.next_ping_nonce.max(1);
            self.next_ping_nonce = nonce.wrapping_add(1).max(1);
            self.next_heartbeat_us = Some(now_us.saturating_add(HEARTBEAT_INTERVAL_US));
            self.ping_tracker.record_ping(nonce, now_us, now_us);
            actions.push(ViewerAction::SendControl(ControlMessage::Ping(Ping {
                nonce,
                sender_ts_us: now_us,
            })));
        }
        if self.phase == ViewerPhase::Switching
            && self
                .switch_deadline_us
                .is_some_and(|deadline| now_us >= deadline)
        {
            if self.switch_retries == 0 {
                self.switch_retries = 1;
                self.switch_deadline_us = Some(now_us.saturating_add(SWITCH_TIMEOUT_US));
                if let (Some(req_id), Some(display_id)) =
                    (self.pending_switch_req, self.pending_display)
                {
                    actions.push(ViewerAction::SendControl(ControlMessage::SwitchMonitor(
                        SwitchMonitor {
                            req_id,
                            display_id: display_id.get(),
                        },
                    )));
                }
            } else {
                self.pending_display = None;
                self.pending_switch_req = None;
                self.switch_reset_received = false;
                self.switch_deadline_us = None;
                self.switch_retries = 0;
                self.waiting_for_keyframe = false;
                self.phase = if self.selected_display.is_some() {
                    ViewerPhase::Streaming
                } else {
                    ViewerPhase::AwaitingStream
                };
            }
        }
        if self.waiting_for_keyframe && !self.video_paused {
            if self
                .keyframe_nudge_at_us
                .is_some_and(|deadline| now_us >= deadline)
            {
                self.keyframe_nudge_at_us = None;
                actions.extend(self.request_keyframe_if_allowed(now_us));
            }
            if self
                .keyframe_reset_at_us
                .is_some_and(|deadline| now_us >= deadline)
            {
                if let Some(epoch) = self.stream_epoch {
                    actions.push(ViewerAction::ResetDecoder { epoch });
                    actions.extend(self.request_keyframe_if_allowed(now_us));
                }
                self.keyframe_reset_at_us = Some(now_us.saturating_add(FIRST_KEYFRAME_RESET_US));
            }
        }
        actions
    }

    /// Closes the session and asks the caller to send Goodbye then close.
    pub fn close(&mut self) -> Vec<ViewerAction> {
        if self.phase == ViewerPhase::Closed {
            return Vec::new();
        }
        self.phase = ViewerPhase::Closed;
        self.topology = None;
        self.invalidated_display = None;
        self.stream_epoch = None;
        self.pending_display = None;
        self.pending_switch_req = None;
        self.switch_reset_received = false;
        self.backoff.reset();
        vec![
            ViewerAction::Event(SessionEvent::SessionEnded),
            ViewerAction::SendControl(ControlMessage::Goodbye(Goodbye {
                reason: GoodbyeReason::Normal,
            })),
            ViewerAction::DisconnectTransport,
        ]
    }

    fn request_keyframe_if_allowed(&mut self, now_us: u64) -> Vec<ViewerAction> {
        let Some(epoch) = self.stream_epoch else {
            return Vec::new();
        };
        let allowed = self.last_keyframe_request_us.is_none_or(|previous| {
            now_us
                .checked_sub(previous)
                .is_some_and(|elapsed| elapsed >= KEYFRAME_REQUEST_MIN_INTERVAL_US)
        });
        if !allowed {
            return Vec::new();
        }
        self.last_keyframe_request_us = Some(now_us);
        vec![ViewerAction::SendControl(ControlMessage::RequestKeyframe(
            RequestKeyframe { epoch },
        ))]
    }

    fn reset_matches_request(&self, reset: &StreamReset) -> bool {
        match self.pending_switch_req {
            Some(request_id) => reset.req_id == request_id || reset.req_id == 0,
            None => reset.req_id == 0,
        }
    }

    fn is_selected_display_invalidation(&self, reset: &StreamReset) -> bool {
        reset.status == StreamStatus::DisplayNotFound
            && reset.req_id == 0
            && self.epoch_is_new(reset.epoch)
            && DisplayId::new(reset.display_id).is_some_and(|display| {
                self.selected_display == Some(display) || self.invalidated_display == Some(display)
            })
    }

    fn reset_display_matches(&self, reset: &StreamReset) -> bool {
        if self.is_selected_display_invalidation(reset) {
            return true;
        }
        let Some(actual) = DisplayId::new(reset.display_id) else {
            return false;
        };
        if reset.status == StreamStatus::EncoderFailed
            && self.pending_display.is_none()
            && self.selected_display.is_none()
        {
            return self.display_is_available(actual);
        }
        let expected = if reset.status == StreamStatus::Ok {
            self.pending_display.or_else(|| {
                self.selected_display
                    .filter(|display| self.display_is_available(*display))
            })
        } else {
            self.pending_display
                .or(self.selected_display)
                .or(self.invalidated_display)
        };
        match expected {
            Some(target) => target == actual,
            None if reset.status == StreamStatus::Ok => true,
            None if reset.status == StreamStatus::EncoderFailed => {
                self.display_is_available(actual)
            }
            None => false,
        }
    }

    fn epoch_is_new(&self, candidate: u16) -> bool {
        self.stream_epoch.is_none_or(|current| {
            let distance = candidate.wrapping_sub(current);
            distance != 0 && distance < (1_u16 << 15)
        })
    }

    fn display_is_available(&self, display_id: DisplayId) -> bool {
        self.topology.as_ref().is_some_and(|topology| {
            topology.displays.iter().any(|display| {
                display.display_id == display_id.get() && display.flags & (1 << 2) != 0
            })
        })
    }

    fn allocate_req_id(&mut self) -> u32 {
        let request_id = self.next_req_id.max(1);
        self.next_req_id = request_id.wrapping_add(1).max(1);
        request_id
    }

    fn hold_or_drop(&self) -> VideoDisposition {
        if self.rendered_epoch.is_some() {
            VideoDisposition::HoldLastFrame
        } else {
            VideoDisposition::Drop
        }
    }
}

fn valid_topology(topology: &TopologyAnnounce) -> bool {
    if topology.displays.len() > MAX_DISPLAYS {
        return false;
    }
    for (index, display) in topology.displays.iter().enumerate() {
        if display.display_id == 0
            || display.width_px == 0
            || display.height_px == 0
            || display.flags & !0x0f != 0
            || topology.displays[..index]
                .iter()
                .any(|prior| prior.display_id == display.display_id)
        {
            return false;
        }
    }
    topology.active_display_id == 0
        || topology
            .displays
            .iter()
            .any(|display| display.display_id == topology.active_display_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use racc_proto::{DisplayInfo, OsType};

    fn hello() -> Hello {
        Hello {
            protocol_version: 1,
            device_name: "viewer".into(),
            os: OsType::Windows,
            app_version: "test".into(),
            video_udp_port: 5000,
            codecs: 1,
            max_height: 1080,
            features: 0,
        }
    }

    fn ack() -> HelloAck {
        HelloAck {
            protocol_version: 1,
            status: HelloStatus::Ok,
            device_name: "host".into(),
            os: OsType::Windows,
            app_version: "test".into(),
            codecs: 1,
            max_height: 1080,
            features: 0,
            host_cpu_cores: 8,
        }
    }

    fn display(display_id: u32, available: bool) -> DisplayInfo {
        DisplayInfo {
            display_id,
            name: format!("Display {display_id}"),
            x: 0,
            y: 0,
            width_px: 1920,
            height_px: 1080,
            scale_milli: 1000,
            refresh_mhz: 60_000,
            flags: 1 | 2 | if available { 4 } else { 0 },
        }
    }

    fn topology(revision: u32) -> TopologyAnnounce {
        TopologyAnnounce {
            topology_rev: revision,
            active_display_id: 1,
            displays: vec![display(1, true), display(2, true)],
        }
    }

    fn reset(req_id: u32, epoch: u16, display_id: u32) -> StreamReset {
        StreamReset {
            req_id,
            epoch,
            codec: StreamCodec::H264,
            width: 1280,
            height: 720,
            fps: STREAM_FPS,
            topology_rev: 1,
            display_id,
            status: StreamStatus::Ok,
        }
    }

    fn connected_viewer() -> ViewerSession {
        let mut session = ViewerSession::new(hello());
        assert!(matches!(
            session.on_connect().as_slice(),
            [ViewerAction::SendControl(ControlMessage::Hello(_))]
        ));
        assert_eq!(
            session.on_hello_ack(ack()),
            vec![ViewerAction::Event(SessionEvent::Connected)]
        );
        assert!(session.on_topology(topology(1)).is_empty());
        session
    }

    fn start_stream(session: &mut ViewerSession) {
        assert_eq!(
            session.on_stream_reset(reset(0, 1, 1)),
            vec![ViewerAction::ResetDecoder { epoch: 1 }]
        );
        let (disposition, actions) = session.on_decoded_frame(1, true);
        assert_eq!(disposition, VideoDisposition::Replace);
        assert_eq!(
            actions,
            vec![ViewerAction::Event(SessionEvent::DisplaySelected(
                DisplayId::new(1).expect("test id is nonzero")
            ))]
        );
    }

    #[test]
    fn switching_suppresses_pointer_but_keeps_keyboard_and_wheel_input_live() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        let switch = session.switch_display(2, 100);
        let request_id = match switch.as_slice() {
            [ViewerAction::SendControl(ControlMessage::SwitchMonitor(request))] => request.req_id,
            other => panic!("unexpected switch action: {other:?}"),
        };
        let pointer = InputEvent {
            epoch: 1,
            display_id: 1,
            event: InputEventKind::MouseMoveAbs { u: 32000, v: 32000 },
        };
        assert!(session.on_user_input(pointer).is_empty());
        let key = InputEvent {
            epoch: 1,
            display_id: 1,
            event: InputEventKind::Key {
                hid_usage: 0x04,
                pressed: true,
                modifiers: 0,
            },
        };
        assert_eq!(
            session.on_user_input(key),
            vec![ViewerAction::ForwardInput(key)]
        );
        let wheel = InputEvent {
            epoch: 1,
            display_id: 1,
            event: InputEventKind::Wheel { dx: 0, dy: 120 },
        };
        assert_eq!(
            session.on_user_input(wheel),
            vec![ViewerAction::ForwardInput(wheel)]
        );
        assert_eq!(
            session.on_stream_reset_at(reset(request_id, 2, 2), 200),
            vec![ViewerAction::ResetDecoder { epoch: 2 }]
        );
        let mapped_key = match session.on_user_input(key).as_slice() {
            [ViewerAction::ForwardInput(event)] => *event,
            other => panic!("keyboard event not forwarded: {other:?}"),
        };
        assert_eq!((mapped_key.epoch, mapped_key.display_id), (2, 2));
        assert!(session.on_user_input(pointer).is_empty());
        assert_eq!(
            session.on_decoded_frame(2, true).0,
            VideoDisposition::Replace
        );
        let new_pointer = InputEvent {
            epoch: 2,
            display_id: 2,
            event: pointer.event,
        };
        assert_eq!(
            session.on_user_input(new_pointer),
            vec![ViewerAction::ForwardInput(new_pointer)]
        );
    }

    #[test]
    fn host_initiated_reset_for_pending_display_is_accepted_during_switch() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        assert!(matches!(
            session.switch_display(2, 100).as_slice(),
            [ViewerAction::SendControl(ControlMessage::SwitchMonitor(_))]
        ));
        assert_eq!(
            session.on_stream_reset_at(reset(0, 2, 2), 200),
            vec![ViewerAction::ResetDecoder { epoch: 2 }]
        );
        assert_eq!(
            session.pending_display(),
            Some(DisplayId::new(2).expect("test display"))
        );
        assert_eq!(
            session.on_decoded_frame(2, true).0,
            VideoDisposition::Replace
        );
        assert_eq!(
            session.selected_display(),
            Some(DisplayId::new(2).expect("test display"))
        );
        assert_eq!(session.phase(), ViewerPhase::Streaming);
    }

    #[test]
    fn stale_viewer_input_is_dropped() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        assert!(session
            .on_user_input(InputEvent {
                epoch: 0,
                display_id: 1,
                event: InputEventKind::Key {
                    hid_usage: 0x04,
                    pressed: true,
                    modifiers: 0
                },
            })
            .is_empty());
        assert!(session
            .on_user_input(InputEvent {
                epoch: 1,
                display_id: 2,
                event: InputEventKind::MouseMoveRel { dx: 1, dy: 1 },
            })
            .is_empty());
    }

    #[test]
    fn viewer_timer_constants_and_handshake_timeout_are_exact() {
        assert_eq!(crate::HEARTBEAT_INTERVAL_MS, 1_000);
        assert_eq!(crate::HEARTBEAT_TIMEOUT_MS, 5_000);
        assert_eq!(crate::HANDSHAKE_TIMEOUT_MS, 5_000);
        assert_eq!(BUSY_RETRY_DELAY_MS, 5_000);
        assert_eq!(crate::SWITCH_TIMEOUT_MS, 2_000);
        assert_eq!(crate::FIRST_KEYFRAME_NUDGE_MS, 1_000);
        assert_eq!(crate::FIRST_KEYFRAME_RESET_MS, 5_000);
        assert_eq!(
            crate::RECONNECT_BACKOFF_MS,
            [500, 1_000, 2_000, 4_000, 5_000]
        );
        assert_eq!(HEARTBEAT_INTERVAL_US, 1_000_000);
        assert_eq!(HEARTBEAT_TIMEOUT_US, 5_000_000);
        assert_eq!(HANDSHAKE_TIMEOUT_US, 5_000_000);
        assert_eq!(BUSY_RETRY_DELAY_US, 5_000_000);
        assert_eq!(SWITCH_TIMEOUT_US, 2_000_000);
        assert_eq!(FIRST_KEYFRAME_NUDGE_US, 1_000_000);
        assert_eq!(FIRST_KEYFRAME_RESET_US, 5_000_000);
        assert_eq!(
            RECONNECT_BACKOFF_US,
            [500_000, 1_000_000, 2_000_000, 4_000_000, 5_000_000]
        );

        let mut session = ViewerSession::new(hello());
        assert_eq!(session.on_connect_at(100).len(), 1);
        assert!(session.tick(5_000_099).is_empty());
        assert_eq!(
            session.tick(5_000_100),
            vec![
                ViewerAction::DisconnectTransport,
                ViewerAction::Event(SessionEvent::Reconnecting),
            ]
        );
        assert_eq!(session.backoff().retry_at_us(), Some(5_500_100));
    }

    #[test]
    fn heartbeat_rtt_first_keyframe_nudge_and_decoder_reset_use_virtual_time() {
        let mut session = ViewerSession::new(hello());
        session.on_connect_at(0);
        session.on_hello_ack_at(ack(), 0);
        let ping_actions = session.tick(HEARTBEAT_INTERVAL_US);
        let ping = ping_actions
            .iter()
            .find_map(|action| match action {
                ViewerAction::SendControl(ControlMessage::Ping(ping)) => Some(*ping),
                _ => None,
            })
            .expect("heartbeat ping");
        assert_eq!(
            session.on_pong(
                Pong {
                    nonce: ping.nonce,
                    echo_ts_us: ping.sender_ts_us,
                },
                HEARTBEAT_INTERVAL_US + 25_000
            ),
            Some(25_000)
        );
        assert_eq!(
            session
                .rtt_snapshot(HEARTBEAT_INTERVAL_US + 25_000)
                .last_rtt_us,
            Some(25_000)
        );

        let mut session = connected_viewer();
        assert!(session
            .on_stream_reset_at(reset(0, 1, 1), 100)
            .contains(&ViewerAction::ResetDecoder { epoch: 1 }));
        session.on_control_activity_at(4_000_000);
        let nudge = session.tick(1_000_100);
        assert!(nudge.iter().any(|action| matches!(
            action,
            ViewerAction::SendControl(ControlMessage::RequestKeyframe(RequestKeyframe {
                epoch: 1
            }))
        )));
        let reset = session.tick(5_000_100);
        assert!(reset.contains(&ViewerAction::ResetDecoder { epoch: 1 }));
        assert!(reset.iter().any(|action| matches!(
            action,
            ViewerAction::SendControl(ControlMessage::RequestKeyframe(RequestKeyframe {
                epoch: 1
            }))
        )));
    }

    #[test]
    fn switch_retries_once_then_restores_previous_display_and_busy_waits_five_seconds() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        let request = session.switch_display(2, 100);
        let req = match request.as_slice() {
            [ViewerAction::SendControl(ControlMessage::SwitchMonitor(req))] => *req,
            other => panic!("unexpected request: {other:?}"),
        };
        let retry = session.tick(2_000_100);
        assert!(
            retry.contains(&ViewerAction::SendControl(ControlMessage::SwitchMonitor(
                req
            )))
        );
        assert_eq!(
            session
                .tick(4_000_100)
                .iter()
                .filter(|action| matches!(
                    action,
                    ViewerAction::SendControl(ControlMessage::SwitchMonitor(_))
                ))
                .count(),
            0
        );
        assert_eq!(session.phase(), ViewerPhase::Streaming);
        assert_eq!(session.selected_display(), DisplayId::new(1));

        let mut busy = ViewerSession::new(hello());
        busy.on_connect_at(100);
        let mut busy_ack = ack();
        busy_ack.status = HelloStatus::Busy;
        assert_eq!(
            busy.on_hello_ack_at(busy_ack, 100),
            vec![ViewerAction::DisconnectTransport]
        );
        assert!(busy.tick(5_000_099).is_empty());
        assert_eq!(busy.tick(5_000_100), vec![ViewerAction::ReconnectTransport]);
    }

    proptest! {
        #[test]
        fn arbitrary_viewer_event_sequences_preserve_epoch_and_keyframe_promotion(
            ops in prop::collection::vec(any::<u8>(), 1..192),
        ) {
            let mut session = connected_viewer();
            start_stream(&mut session);
            for (index, op) in ops.into_iter().enumerate() {
                let now_us = (index as u64).saturating_mul(250_000);
                let before = session.epoch();
                let mut replaced = None;
                let actions = match op % 11 {
                    0 => session.switch_display(1 + u32::from(op & 1), now_us),
                    1 => session.on_packet_loss(now_us),
                    2 => session.on_decoder_error(now_us),
                    3 => session.tick(now_us),
                    4 => {
                        let epoch = before.unwrap_or(1);
                        let (disposition, actions) = session.on_decoded_frame_at(epoch, op & 0x80 != 0, now_us);
                        replaced = Some((disposition, epoch, op & 0x80 != 0));
                        actions
                    }
                    5 => session.on_pause(),
                    6 => session.on_resume(),
                    7 => session.on_disconnect(now_us),
                    8 => session.on_connect_at(now_us),
                    9 => session.on_hello_ack_at(ack(), now_us),
                    _ => {
                        let epoch = before.map_or(1, |value| value.wrapping_add(1));
                        session.on_stream_reset_at(reset(0, epoch, 1), now_us)
                    }
                };
                prop_assert!(actions.len() <= 8);
                if op % 11 == 3 {
                    prop_assert!(actions.len() <= 8);
                }
                if let Some((VideoDisposition::Replace, frame_epoch, is_keyframe)) = replaced {
                    prop_assert!(is_keyframe);
                    prop_assert_eq!(Some(frame_epoch), session.epoch());
                }
                if let (Some(old), Some(new)) = (before, session.epoch()) {
                    let distance = new.wrapping_sub(old);
                    prop_assert!(distance == 0 || distance < (1 << 15));
                }
            }
        }
    }

    #[test]
    fn handshake_and_initial_keyframe_promote_stream() {
        let mut session = connected_viewer();
        assert_eq!(session.phase(), ViewerPhase::AwaitingStream);
        start_stream(&mut session);
        assert_eq!(session.phase(), ViewerPhase::Streaming);
        assert_eq!(session.selected_display(), DisplayId::new(1));
    }

    #[test]
    fn switch_holds_old_frame_until_matching_epoch_keyframe() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        let request = session.switch_display(2, 10);
        let req_id = match request.as_slice() {
            [ViewerAction::SendControl(ControlMessage::SwitchMonitor(request))] => request.req_id,
            other => panic!("unexpected switch actions: {other:?}"),
        };
        assert_eq!(
            session.on_decoded_frame(1, false).0,
            VideoDisposition::HoldLastFrame
        );
        assert!(session
            .on_stream_reset(reset(req_id.wrapping_add(1), 2, 2))
            .is_empty());
        assert_eq!(
            session.on_stream_reset(reset(req_id, 2, 2)),
            vec![ViewerAction::ResetDecoder { epoch: 2 }]
        );
        assert_eq!(
            session.on_decoded_frame(2, false).0,
            VideoDisposition::HoldLastFrame
        );
        let (disposition, actions) = session.on_decoded_frame(2, true);
        assert_eq!(disposition, VideoDisposition::Replace);
        assert_eq!(
            actions,
            vec![ViewerAction::Event(SessionEvent::DisplaySelected(
                DisplayId::new(2).expect("test id is nonzero")
            ))]
        );
        assert_eq!(session.selected_display(), DisplayId::new(2));
    }

    #[test]
    fn same_epoch_switch_failure_restores_the_previous_stream() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        let request = session.switch_display(2, 10);
        let req_id = match request.as_slice() {
            [ViewerAction::SendControl(ControlMessage::SwitchMonitor(request))] => request.req_id,
            other => panic!("unexpected switch actions: {other:?}"),
        };
        let mut failed = reset(req_id, 1, 2);
        failed.status = StreamStatus::CaptureFailed;
        failed.width = 0;
        failed.height = 0;
        assert!(session.on_stream_reset(failed).is_empty());
        assert_eq!(session.phase(), ViewerPhase::Streaming);
        assert_eq!(session.pending_display(), None);
        assert_eq!(session.selected_display(), DisplayId::new(1));
        assert_eq!(
            session.on_decoded_frame(1, false).0,
            VideoDisposition::Present
        );
    }

    #[test]
    fn reset_must_match_the_current_topology_revision() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        session.on_topology(topology(2));

        let mut stale = reset(0, 2, 1);
        stale.topology_rev = 1;
        assert!(session.on_stream_reset(stale).is_empty());
        assert_eq!(session.epoch(), Some(1));

        let mut current = reset(0, 2, 1);
        current.topology_rev = 2;
        assert_eq!(
            session.on_stream_reset(current),
            vec![ViewerAction::ResetDecoder { epoch: 2 }]
        );
    }

    #[test]
    fn successful_switch_reset_must_match_the_requested_display() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        let actions = session.switch_display(2, 10);
        let req_id = match actions.as_slice() {
            [ViewerAction::SendControl(ControlMessage::SwitchMonitor(request))] => request.req_id,
            other => panic!("unexpected switch actions: {other:?}"),
        };

        assert!(session.on_stream_reset(reset(req_id, 2, 1)).is_empty());
        assert_eq!(session.pending_display(), DisplayId::new(2));
        assert_eq!(session.epoch(), Some(1));

        assert_eq!(
            session.on_stream_reset(reset(req_id, 2, 2)),
            vec![ViewerAction::ResetDecoder { epoch: 2 }]
        );
    }

    #[test]
    fn active_stream_reset_rejects_dimensions_above_product_cap() {
        let mut session = connected_viewer();
        let mut too_wide = reset(0, 1, 1);
        too_wide.width = 1921;
        assert!(session.on_stream_reset(too_wide).is_empty());
        assert_eq!(session.epoch(), None);

        let mut too_tall = reset(0, 1, 1);
        too_tall.height = 1081;
        assert!(session.on_stream_reset(too_tall).is_empty());
        assert_eq!(session.epoch(), None);
    }

    #[test]
    fn initial_encoder_failure_is_reported_without_a_selected_stream() {
        let mut session = connected_viewer();
        let mut failed = reset(0, 1, 1);
        failed.status = StreamStatus::EncoderFailed;
        failed.width = 0;
        failed.height = 0;
        assert_eq!(
            session.on_stream_reset(failed),
            vec![ViewerAction::Event(SessionEvent::EncoderFailed)]
        );
        assert_eq!(session.selected_display(), None);
        assert_eq!(session.phase(), ViewerPhase::AwaitingStream);

        let mut unavailable = reset(0, 1, 99);
        unavailable.status = StreamStatus::EncoderFailed;
        unavailable.width = 0;
        unavailable.height = 0;
        assert!(session.on_stream_reset(unavailable).is_empty());
    }

    #[test]
    fn switch_encoder_failure_requires_the_pending_target_and_is_reported() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        let request = session.switch_display(2, 10);
        let req_id = match request.as_slice() {
            [ViewerAction::SendControl(ControlMessage::SwitchMonitor(request))] => request.req_id,
            other => panic!("unexpected switch actions: {other:?}"),
        };
        assert_ne!(req_id, 0);

        // The host's terminal software-encoder failure is unsolicited and uses
        // req_id zero, even when a switch target was pending.
        let mut wrong_target = reset(0, 2, 1);
        wrong_target.status = StreamStatus::EncoderFailed;
        wrong_target.width = 0;
        wrong_target.height = 0;
        assert!(session.on_stream_reset(wrong_target).is_empty());
        assert_eq!(session.pending_display(), DisplayId::new(2));

        let mut failed = reset(0, 2, 2);
        failed.status = StreamStatus::EncoderFailed;
        failed.width = 0;
        failed.height = 0;
        assert_eq!(
            session.on_stream_reset(failed),
            vec![ViewerAction::Event(SessionEvent::EncoderFailed)]
        );
        assert_eq!(session.pending_display(), None);
        assert_eq!(session.selected_display(), DisplayId::new(1));
        assert_eq!(session.epoch(), Some(2));
        assert_eq!(session.phase(), ViewerPhase::Streaming);
        assert!(session.has_last_good_frame());
        assert_eq!(session.on_decoded_frame(1, true).0, VideoDisposition::Drop);
        assert_eq!(
            session.on_decoded_frame(2, false).0,
            VideoDisposition::HoldLastFrame
        );
    }

    #[test]
    fn removed_selected_display_retires_epoch_and_drops_queued_frames() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        assert!(session.has_last_good_frame());

        assert_eq!(
            session.on_topology(TopologyAnnounce {
                topology_rev: 2,
                active_display_id: 2,
                displays: vec![display(2, true)],
            }),
            vec![ViewerAction::Event(SessionEvent::DisplayUnavailable(
                DisplayId::new(1).expect("test id is nonzero")
            ))]
        );
        assert_eq!(session.selected_display(), None);

        let mut removed = reset(0, 2, 1);
        removed.topology_rev = 2;
        removed.status = StreamStatus::DisplayNotFound;
        removed.width = 0;
        removed.height = 0;
        assert!(session.on_stream_reset(removed).is_empty());

        assert_eq!(session.epoch(), Some(2));
        assert_eq!(session.selected_display(), None);
        assert!(session.has_last_good_frame());
        assert_eq!(session.on_decoded_frame(1, true).0, VideoDisposition::Drop);
        assert_eq!(
            session.on_decoded_frame(2, false).0,
            VideoDisposition::HoldLastFrame
        );
    }

    #[test]
    fn stale_epochs_and_non_keyframes_cannot_replace_the_last_image() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        assert!(session.on_stream_reset(reset(0, 0, 1)).is_empty());
        assert_eq!(session.on_decoded_frame(0, true).0, VideoDisposition::Drop);
        assert_eq!(
            session.on_stream_reset(reset(0, 2, 1)),
            vec![ViewerAction::ResetDecoder { epoch: 2 }]
        );
        assert_eq!(
            session.on_decoded_frame(2, false).0,
            VideoDisposition::HoldLastFrame
        );
        assert_eq!(session.on_decoded_frame(1, true).0, VideoDisposition::Drop);
    }

    #[test]
    fn pause_resume_requires_a_fresh_stream_reset_and_keyframe() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        assert_eq!(
            session.on_pause(),
            vec![ViewerAction::SendControl(ControlMessage::PauseVideo(
                PauseVideo
            ))]
        );
        assert_eq!(
            session.on_decoded_frame(1, true).0,
            VideoDisposition::HoldLastFrame
        );
        assert_eq!(
            session.on_resume(),
            vec![ViewerAction::SendControl(ControlMessage::ResumeVideo(
                ResumeVideo
            ))]
        );
        assert_eq!(
            session.on_decoded_frame(1, true).0,
            VideoDisposition::HoldLastFrame
        );
        assert_eq!(
            session.on_stream_reset(reset(0, 2, 1)),
            vec![ViewerAction::ResetDecoder { epoch: 2 }]
        );
        assert_eq!(
            session.on_decoded_frame(2, true).0,
            VideoDisposition::Replace
        );
        assert_eq!(session.phase(), ViewerPhase::Streaming);
    }

    #[test]
    fn reconnect_backoff_is_bounded_and_first_retry_waits() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        assert_eq!(
            session.on_disconnect(1_000),
            vec![ViewerAction::Event(SessionEvent::Reconnecting)]
        );
        assert_eq!(session.phase(), ViewerPhase::Reconnecting);
        assert!(session.tick(500_999).is_empty());
        assert_eq!(
            session.tick(501_000),
            vec![ViewerAction::ReconnectTransport]
        );
        assert!(session.tick(501_001).is_empty());
        assert_eq!(session.backoff().attempts(), 1);
        assert_eq!(session.backoff().retry_at_us(), None);
        assert_eq!(session.on_connect().len(), 1);
        assert_eq!(
            session.on_hello_ack(ack()),
            vec![ViewerAction::Event(SessionEvent::Reconnected)]
        );
        assert_eq!(session.backoff(), Backoff::default());
        assert!(session.on_topology(topology(1)).is_empty());
        assert_eq!(session.phase(), ViewerPhase::AwaitingStream);
        assert_eq!(
            session.on_stream_reset(reset(0, 1, 1)),
            vec![ViewerAction::ResetDecoder { epoch: 1 }]
        );
        assert_eq!(
            session.on_decoded_frame(1, true).0,
            VideoDisposition::Replace
        );
    }

    #[test]
    fn paused_state_survives_reconnect_until_explicit_resume() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        assert_eq!(
            session.on_pause(),
            vec![ViewerAction::SendControl(ControlMessage::PauseVideo(
                PauseVideo
            ))]
        );
        assert_eq!(
            session.on_disconnect(1_000),
            vec![ViewerAction::Event(SessionEvent::Reconnecting)]
        );
        assert!(session.video_paused);

        assert_eq!(
            session.tick(501_000),
            vec![ViewerAction::ReconnectTransport]
        );
        assert_eq!(session.on_connect().len(), 1);
        assert_eq!(
            session.on_hello_ack(ack()),
            vec![
                ViewerAction::Event(SessionEvent::Reconnected),
                ViewerAction::SendControl(ControlMessage::PauseVideo(PauseVideo)),
            ]
        );
        assert!(session.on_topology(topology(1)).is_empty());
        assert_eq!(session.phase(), ViewerPhase::Paused);
        assert!(session.video_paused);

        assert_eq!(
            session.on_resume(),
            vec![ViewerAction::SendControl(ControlMessage::ResumeVideo(
                ResumeVideo
            ))]
        );
        assert!(!session.video_paused);
        assert_eq!(session.phase(), ViewerPhase::AwaitingKeyframe);
        assert_eq!(
            session.on_stream_reset(reset(0, 2, 1)),
            vec![ViewerAction::ResetDecoder { epoch: 2 }]
        );
        assert_eq!(
            session.on_decoded_frame(2, true).0,
            VideoDisposition::Replace
        );
    }

    #[test]
    fn failed_reconnect_attempt_schedules_the_next_backoff() {
        let mut session = connected_viewer();
        assert_eq!(
            session.on_disconnect(1_000),
            vec![ViewerAction::Event(SessionEvent::Reconnecting)]
        );
        assert_eq!(
            session.tick(501_000),
            vec![ViewerAction::ReconnectTransport]
        );

        // A failed transport attempt reports disconnect while the session is still
        // in Reconnecting and the previous deadline has already been consumed.
        assert!(session.on_disconnect(501_000).is_empty());
        assert_eq!(session.backoff().retry_at_us(), Some(1_501_000));
        assert!(session.tick(1_500_999).is_empty());
        assert_eq!(
            session.tick(1_501_000),
            vec![ViewerAction::ReconnectTransport]
        );
    }

    #[test]
    fn decoder_error_preserves_frame_and_rate_limits_keyframe_request() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        let actions = session.on_decoder_error(1_000_000);
        assert!(actions.contains(&ViewerAction::Event(SessionEvent::DecoderReset)));
        assert!(actions.contains(&ViewerAction::ResetDecoder { epoch: 1 }));
        assert!(
            actions.contains(&ViewerAction::SendControl(ControlMessage::RequestKeyframe(
                RequestKeyframe { epoch: 1 }
            )))
        );
        assert!(session.has_last_good_frame());
        assert_eq!(
            session.on_decoded_frame(1, false).0,
            VideoDisposition::HoldLastFrame
        );
        assert!(session.on_packet_loss(1_199_999).is_empty());
        assert_eq!(
            session.on_packet_loss(1_200_000),
            vec![ViewerAction::SendControl(ControlMessage::RequestKeyframe(
                RequestKeyframe { epoch: 1 }
            ))]
        );
        assert_eq!(
            session.on_decoded_frame(1, true).0,
            VideoDisposition::Replace
        );
    }

    #[test]
    fn unavailable_switch_is_rejected_without_mutating_stream() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        session.on_topology(TopologyAnnounce {
            topology_rev: 2,
            active_display_id: 1,
            displays: vec![display(1, true), display(2, false)],
        });
        assert_eq!(
            session.switch_display(2, 100),
            vec![ViewerAction::Event(SessionEvent::DisplayUnavailable(
                DisplayId::new(2).expect("test id is nonzero")
            ))]
        );
        assert_eq!(session.selected_display(), DisplayId::new(1));
        assert_eq!(session.phase(), ViewerPhase::Streaming);
    }

    #[test]
    fn invalid_topology_and_old_revision_are_ignored() {
        let mut session = connected_viewer();
        assert!(session.on_topology(topology(1)).is_empty());
        let mut invalid = topology(2);
        invalid.displays[1].display_id = 1;
        assert!(session.on_topology(invalid).is_empty());
        assert!(session.on_topology(topology(0)).is_empty());
        assert_eq!(session.phase(), ViewerPhase::AwaitingStream);
    }

    #[test]
    fn close_emits_session_ended_before_transport_shutdown() {
        let mut session = connected_viewer();
        assert_eq!(
            session.close(),
            vec![
                ViewerAction::Event(SessionEvent::SessionEnded),
                ViewerAction::SendControl(ControlMessage::Goodbye(Goodbye {
                    reason: GoodbyeReason::Normal
                })),
                ViewerAction::DisconnectTransport,
            ]
        );
    }

    #[test]
    fn reconnect_schedule_repeats_five_second_cap() {
        let mut backoff = Backoff::default();
        let expected = [
            500_000, 1_000_000, 2_000_000, 4_000_000, 5_000_000, 5_000_000, 5_000_000,
        ];
        for delay in expected {
            assert_eq!(backoff.schedule(0), delay);
        }
        assert_eq!(backoff.attempts(), 7);
        assert_eq!(backoff.retry_at_us(), Some(5_000_000));
    }

    #[test]
    fn paused_switch_ack_keeps_target_for_recovery_reset() {
        let mut session = connected_viewer();
        start_stream(&mut session);
        let request = session.switch_display(2, 10);
        let req_id = match request.as_slice() {
            [ViewerAction::SendControl(ControlMessage::SwitchMonitor(request))] => request.req_id,
            other => panic!("unexpected switch actions: {other:?}"),
        };
        let mut wrong_target = reset(req_id, 2, 1);
        wrong_target.status = StreamStatus::Paused;
        wrong_target.width = 0;
        wrong_target.height = 0;
        assert!(session.on_stream_reset(wrong_target).is_empty());
        assert_eq!(session.pending_display(), DisplayId::new(2));
        assert_eq!(session.epoch(), Some(1));
        assert_eq!(session.phase(), ViewerPhase::Switching);

        let mut paused = reset(req_id, 2, 2);
        paused.status = StreamStatus::Paused;
        paused.width = 0;
        paused.height = 0;
        assert_eq!(
            session.on_stream_reset(paused),
            vec![ViewerAction::ResetDecoder { epoch: 2 }]
        );
        assert_eq!(session.pending_display(), DisplayId::new(2));
        assert_eq!(session.phase(), ViewerPhase::Paused);
        assert_eq!(
            session.on_resume(),
            vec![ViewerAction::SendControl(ControlMessage::ResumeVideo(
                ResumeVideo
            ))]
        );
        assert_eq!(
            session.on_stream_reset(reset(0, 3, 2)),
            vec![ViewerAction::ResetDecoder { epoch: 3 }]
        );
        let (disposition, actions) = session.on_decoded_frame(3, true);
        assert_eq!(disposition, VideoDisposition::Replace);
        assert_eq!(
            actions,
            vec![ViewerAction::Event(SessionEvent::DisplaySelected(
                DisplayId::new(2).expect("test id is nonzero")
            ))]
        );
    }
}
