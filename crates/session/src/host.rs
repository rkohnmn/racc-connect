use crate::{SessionEvent, STREAM_FPS};
use racc_proto::{
    ControlMessage, Goodbye, Hello, HelloAck, HelloStatus, RequestKeyframe, StreamCodec,
    StreamReset, StreamStatus, SwitchMonitor,
};
use racc_topology::{diff_topologies, requires_stream_reset, DisplayId, Topology};
use std::cmp::min;

/// Delay before the first capture recovery attempt.
pub const INITIAL_CAPTURE_RETRY_US: u64 = 50_000;
/// Maximum delay between capture recovery attempts.
pub const MAX_CAPTURE_RETRY_US: u64 = 1_000_000;
/// Default grace period before a disconnected viewer's stream is stopped.
pub const DEFAULT_CONTROL_DISCONNECT_TIMEOUT_US: u64 = 5_000_000;
/// Maximum encoded width in pixels, matching the 1080p product ceiling.
pub const MAX_STREAM_WIDTH_PX: u16 = 1920;
/// Minimum encoded height in pixels when the display aspect ratio permits it.
pub const MIN_STREAM_HEIGHT_PX: u16 = 480;
/// Maximum encoded height in pixels, matching the 1080p product ceiling.
pub const MAX_STREAM_HEIGHT_PX: u16 = 1080;

/// Encoder output dimensions selected for a display while preserving its aspect ratio.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamDimensions {
    /// Encoded stream width in pixels.
    pub width: u16,
    /// Encoded stream height in pixels.
    pub height: u16,
}

/// Host-side control/capture lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostPhase {
    /// Waiting for a viewer Hello.
    AwaitingHello,
    /// Connected and able to stream.
    Streaming,
    /// Viewer requested video pause.
    Paused,
    /// Capture is being recovered.
    RecoveringCapture,
    /// The control peer disconnected but the stop timeout has not elapsed.
    ControlDisconnected,
    /// The peer closed the session.
    Closed,
}

/// A Tailscale path category reported by the identity/transport layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkPath {
    /// Direct peer-to-peer path.
    Direct,
    /// Relayed through DERP.
    Derp,
    /// Path is not currently known.
    Unknown,
}

/// Capture loss reasons that can be reported by a platform adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryReason {
    /// Desktop duplication or another capture API lost access.
    AccessLost,
    /// Display mode, sleep, wake, lock, or session transition.
    DisplayChanged,
    /// Capture permission is unavailable.
    PermissionDenied,
    /// A capture backend reported an unspecified failure.
    BackendFailure,
}

/// Failure categories returned by a capture adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureFailure {
    /// The display or capture device is temporarily unavailable.
    Unavailable,
    /// The operating system denied capture.
    PermissionDenied,
    /// The backend failed for another reason.
    BackendFailure,
}

/// Failure categories returned by an encoder adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncoderFailure {
    /// The selected encoder could not be configured.
    ConfigureFailed,
    /// The active encoder stopped producing frames.
    RuntimeFailure,
}

/// Commands for the platform capture adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CaptureAction {
    /// Attach the capture engine to a display without ending its worker thread.
    SwitchDisplay {
        /// Operation id echoed to the session on completion.
        operation_id: u64,
        /// Previously selected display, when any.
        from: Option<DisplayId>,
        /// New display to capture.
        to: DisplayId,
    },
    /// Recreate capture after access was lost.
    Recreate {
        /// Operation id echoed to the session on completion.
        operation_id: u64,
        /// Display to reattach.
        display_id: DisplayId,
    },
    /// Stop capture after the viewer disconnect timeout or session close.
    Stop,
}

/// Commands for the platform encoder adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EncoderAction {
    /// Configure the encoder for the bounded output geometry of a display.
    Configure {
        /// Operation id echoed to the session on completion.
        operation_id: u64,
        /// Display being captured.
        display_id: DisplayId,
        /// Encoded stream width in pixels.
        width: u16,
        /// Encoded stream height in pixels.
        height: u16,
    },
    /// Pause or resume the encoder's frame production.
    SetPaused(bool),
    /// Force an IDR frame.
    ForceKeyframe {
        /// Epoch associated with the requested IDR.
        epoch: u16,
    },
    /// Rebuild an encoder, optionally using the software fallback.
    Rebuild {
        /// Operation id echoed to the session on completion.
        operation_id: u64,
        /// Whether to use the OpenH264 software fallback.
        use_software: bool,
    },
}

/// Host-owned quality adaptation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QualityAction {
    /// Lower quality by one supported tier.
    StepDownOneTier,
    /// Begin slowly restoring quality after the path recovers.
    RestoreGradually,
}

/// Actions emitted by the host state machine for its caller to execute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostAction {
    /// Send one protocol message on the reliable control channel.
    SendControl(ControlMessage),
    /// Execute a capture adapter command.
    Capture(CaptureAction),
    /// Execute an encoder adapter command.
    Encoder(EncoderAction),
    /// Notify the quality controller.
    Quality(QualityAction),
    /// Record a lifecycle event.
    Event(SessionEvent),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperationPurpose {
    Initial,
    Switch(u32),
    Topology,
    Recovery,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperationStage {
    Capture,
    Encoder,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingOperation {
    id: u64,
    target: DisplayId,
    purpose: OperationPurpose,
    stage: OperationStage,
    stream_dimensions: Option<StreamDimensions>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CaptureRecovery {
    target: DisplayId,
    attempts: u32,
    retry_at_us: u64,
}

/// Host policy values that are not fixed by the wire protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostConfig {
    /// Grace period before capture and encoding stop after control disconnection.
    pub control_disconnect_timeout_us: u64,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            control_disconnect_timeout_us: DEFAULT_CONTROL_DISCONNECT_TIMEOUT_US,
        }
    }
}

/// Deterministic host-side session controller.
///
/// The caller performs networking, capture and encoding when it receives
/// actions, then reports operation results using the corresponding id.
#[derive(Clone, Debug)]
pub struct HostSession {
    capabilities: HelloAck,
    topology: Topology,
    config: HostConfig,
    phase: HostPhase,
    control_disconnect_deadline_us: Option<u64>,
    selected_display: Option<DisplayId>,
    selected_stream_dimensions: Option<StreamDimensions>,
    epoch: u16,
    next_operation_id: u64,
    pending: Option<PendingOperation>,
    recovery: Option<CaptureRecovery>,
    network_path: NetworkPath,
    video_paused: bool,
    encoder_failures: u8,
    software_encoder: bool,
    encoder_rebuild: Option<(u64, bool)>,
    encoder_rebuild_target: Option<DisplayId>,
}

impl HostSession {
    /// Creates a host controller from local capabilities and validated topology.
    pub fn new(capabilities: HelloAck, topology: Topology) -> Self {
        Self::with_config(capabilities, topology, HostConfig::default())
    }

    /// Creates a host controller with explicit non-wire policy values.
    pub fn with_config(capabilities: HelloAck, topology: Topology, config: HostConfig) -> Self {
        Self {
            capabilities,
            topology,
            config,
            phase: HostPhase::AwaitingHello,
            control_disconnect_deadline_us: None,
            selected_display: None,
            selected_stream_dimensions: None,
            epoch: 0,
            next_operation_id: 1,
            pending: None,
            recovery: None,
            network_path: NetworkPath::Unknown,
            video_paused: false,
            encoder_failures: 0,
            software_encoder: false,
            encoder_rebuild: None,
            encoder_rebuild_target: None,
        }
    }

    /// Returns the current host lifecycle phase.
    pub const fn phase(&self) -> HostPhase {
        self.phase
    }

    /// Returns the selected display, if a stream has been configured.
    pub const fn selected_display(&self) -> Option<DisplayId> {
        self.selected_display
    }

    /// Returns the active stream epoch.
    pub const fn epoch(&self) -> u16 {
        self.epoch
    }

    /// Processes a viewer Hello and starts or resumes the stream when possible.
    pub fn on_hello(&mut self, hello: &Hello) -> Vec<HostAction> {
        let reconnecting = self.phase == HostPhase::ControlDisconnected;
        if self.phase != HostPhase::AwaitingHello && !reconnecting {
            return vec![HostAction::SendControl(ControlMessage::HelloAck(
                HelloAck {
                    status: HelloStatus::Busy,
                    ..self.capabilities.clone()
                },
            ))];
        }
        if hello.protocol_version != self.capabilities.protocol_version {
            return vec![HostAction::SendControl(ControlMessage::HelloAck(
                HelloAck {
                    status: HelloStatus::UnsupportedVersion,
                    ..self.capabilities.clone()
                },
            ))];
        }
        if hello.codecs & self.capabilities.codecs & 1 == 0 {
            return vec![HostAction::SendControl(ControlMessage::HelloAck(
                HelloAck {
                    status: HelloStatus::Busy,
                    ..self.capabilities.clone()
                },
            ))];
        }

        let mut actions = vec![HostAction::SendControl(ControlMessage::HelloAck(
            HelloAck {
                status: HelloStatus::Ok,
                ..self.capabilities.clone()
            },
        ))];
        if reconnecting {
            self.control_disconnect_deadline_us = None;
            self.phase = if self.recovery.is_some() {
                HostPhase::RecoveringCapture
            } else if self.video_paused {
                HostPhase::Paused
            } else {
                HostPhase::Streaming
            };
            actions.push(HostAction::Event(SessionEvent::Reconnected));
            if let Ok(topology) = self.topology.to_proto() {
                actions.push(HostAction::SendControl(ControlMessage::TopologyAnnounce(
                    topology,
                )));
            }
            if self.recovery.is_none() && !self.video_paused {
                actions.push(HostAction::Encoder(EncoderAction::SetPaused(false)));
                if let Some(display_id) = self.selected_display {
                    self.epoch = self.epoch.wrapping_add(1);
                    actions.push(self.ok_reset(display_id, 0));
                    actions.push(HostAction::Encoder(EncoderAction::ForceKeyframe {
                        epoch: self.epoch,
                    }));
                }
            }
            return actions;
        }

        actions.push(HostAction::Event(SessionEvent::Connected));
        if let Ok(topology) = self.topology.to_proto() {
            actions.push(HostAction::SendControl(ControlMessage::TopologyAnnounce(
                topology,
            )));
        }
        let target = self
            .topology
            .streamed_display_id()
            .filter(|id| self.display_is_available(*id))
            .or_else(|| {
                self.topology
                    .primary_display_id()
                    .filter(|id| self.display_is_available(*id))
            })
            .or_else(|| {
                self.topology
                    .displays()
                    .iter()
                    .find(|display| display.flags().available())
                    .map(|display| display.id())
            });
        self.phase = HostPhase::Streaming;
        if let Some(display_id) = target {
            actions.extend(self.begin_operation(display_id, OperationPurpose::Initial, false));
        }
        actions
    }

    /// Handles a requested monitor switch. A newer request supersedes any
    /// unfinished capture operation; late completions are ignored by operation id.
    pub fn on_switch_monitor(&mut self, request: SwitchMonitor) -> Vec<HostAction> {
        if self.phase == HostPhase::AwaitingHello
            || self.phase == HostPhase::ControlDisconnected
            || self.phase == HostPhase::Closed
        {
            return Vec::new();
        }
        let Some(display_id) = DisplayId::new(request.display_id) else {
            return vec![self.reset_action(
                request.req_id,
                request.display_id,
                StreamStatus::DisplayNotFound,
            )];
        };
        if !self.display_is_available(display_id) {
            return vec![self.reset_action(
                request.req_id,
                request.display_id,
                StreamStatus::DisplayNotFound,
            )];
        }
        if let Some(mut recovery) = self.recovery {
            self.pending = None;
            recovery.target = display_id;
            recovery.attempts = 0;
            self.recovery = Some(recovery);
            self.epoch = self.epoch.wrapping_add(1);
            return vec![self.reset_action(request.req_id, display_id.get(), StreamStatus::Paused)];
        }
        if self.video_paused {
            return vec![self.reset_action(request.req_id, display_id.get(), StreamStatus::Paused)];
        }
        self.begin_operation(display_id, OperationPurpose::Switch(request.req_id), false)
    }

    /// Reports completion of a capture switch or recreation. Stale operation
    /// results are harmlessly discarded.
    pub fn on_capture_result(
        &mut self,
        operation_id: u64,
        result: Result<(), CaptureFailure>,
        now_us: u64,
    ) -> Vec<HostAction> {
        let Some(mut pending) = self.pending else {
            return Vec::new();
        };
        if pending.id != operation_id || pending.stage != OperationStage::Capture {
            return Vec::new();
        }
        if result.is_err() {
            return self.operation_failed(pending, now_us, false);
        }
        let Some(stream_dimensions) = self.stream_dimensions_for(pending.target) else {
            return self.operation_failed(pending, now_us, false);
        };
        pending.stage = OperationStage::Encoder;
        pending.stream_dimensions = Some(stream_dimensions);
        self.pending = Some(pending);
        vec![HostAction::Encoder(EncoderAction::Configure {
            operation_id,
            display_id: pending.target,
            width: stream_dimensions.width,
            height: stream_dimensions.height,
        })]
    }

    /// Reports completion of encoder configuration following a capture change.
    pub fn on_encoder_configured(
        &mut self,
        operation_id: u64,
        result: Result<(), EncoderFailure>,
        now_us: u64,
    ) -> Vec<HostAction> {
        let Some(pending) = self.pending else {
            return Vec::new();
        };
        if pending.id != operation_id || pending.stage != OperationStage::Encoder {
            return Vec::new();
        }
        if result.is_err() {
            if pending.purpose == OperationPurpose::Initial && self.selected_display.is_none() {
                self.encoder_rebuild_target = Some(pending.target);
            }
            let mut actions = self.operation_failed(pending, now_us, true);
            actions.extend(self.on_encoder_failure());
            return actions;
        }
        let Some(stream_dimensions) = pending.stream_dimensions else {
            if pending.purpose == OperationPurpose::Initial && self.selected_display.is_none() {
                self.encoder_rebuild_target = Some(pending.target);
            }
            let mut actions = self.operation_failed(pending, now_us, true);
            actions.extend(self.on_encoder_failure());
            return actions;
        };
        self.pending = None;
        self.selected_display = Some(pending.target);
        self.selected_stream_dimensions = Some(stream_dimensions);
        self.recovery = None;
        self.phase = if self.control_disconnect_deadline_us.is_some() {
            HostPhase::ControlDisconnected
        } else if self.video_paused {
            HostPhase::Paused
        } else {
            HostPhase::Streaming
        };
        self.epoch = self.epoch.wrapping_add(1);
        let mut actions = Vec::new();
        if pending.purpose == OperationPurpose::Recovery
            && !self.video_paused
            && self.control_disconnect_deadline_us.is_none()
        {
            actions.push(HostAction::Encoder(EncoderAction::SetPaused(false)));
        }
        actions.extend([
            self.ok_reset(pending.target, request_id(pending.purpose)),
            HostAction::Encoder(EncoderAction::ForceKeyframe { epoch: self.epoch }),
            HostAction::Event(SessionEvent::DisplaySelected(pending.target)),
        ]);
        if pending.purpose == OperationPurpose::Recovery {
            actions.push(HostAction::Event(SessionEvent::RecoverySucceeded));
        }
        actions
    }

    /// Processes topology replacement and initiates a stream reconfiguration
    /// when the selected display's geometry or availability requires it.
    pub fn on_topology_update(&mut self, incoming: Topology, now_us: u64) -> Vec<HostAction> {
        let old = self.topology.clone();
        let diff = diff_topologies(&old, &incoming);
        if !racc_topology::is_revision_newer(incoming.revision(), old.revision()) {
            return Vec::new();
        }
        self.topology = incoming;
        let mut actions = self
            .topology
            .to_proto()
            .ok()
            .map(|value| {
                vec![HostAction::SendControl(ControlMessage::TopologyAnnounce(
                    value,
                ))]
            })
            .unwrap_or_default();

        if let Some(pending) = self.pending {
            if !self.display_is_available(pending.target) {
                self.pending = None;
                if pending.purpose == OperationPurpose::Recovery {
                    self.recovery = None;
                }
                if matches!(pending.purpose, OperationPurpose::Switch(_))
                    || Some(pending.target) != self.selected_display
                {
                    actions.push(self.reset_action(
                        request_id(pending.purpose),
                        pending.target.get(),
                        StreamStatus::DisplayNotFound,
                    ));
                }
            } else if pending.stage == OperationStage::Encoder
                && requires_stream_reset(&diff, Some(pending.target))
            {
                // A topology change can arrive while the encoder configures. Restart with
                // a fresh operation id so a late completion cannot acknowledge stale geometry.
                actions.extend(self.begin_operation(pending.target, pending.purpose, false));
            }
        }
        if self
            .recovery
            .is_some_and(|recovery| !self.display_is_available(recovery.target))
        {
            if let Some(recovery) = self.recovery.take() {
                if self.selected_display != Some(recovery.target) {
                    actions.push(self.reset_action(
                        0,
                        recovery.target.get(),
                        StreamStatus::DisplayNotFound,
                    ));
                }
            }
        }

        if let Some(current) = self.selected_display {
            if !self.display_is_available(current) {
                self.selected_display = None;
                self.selected_stream_dimensions = None;
                self.recovery = None;
                self.epoch = self.epoch.wrapping_add(1);
                if self.pending.is_none() {
                    actions.push(HostAction::Encoder(EncoderAction::SetPaused(true)));
                }
                actions.push(self.reset_action(0, current.get(), StreamStatus::DisplayNotFound));
                actions.push(HostAction::Event(SessionEvent::DisplayUnavailable(current)));
            } else if requires_stream_reset(&diff, Some(current))
                && !self.video_paused
                && self.pending.is_none()
            {
                actions.extend(self.begin_operation(current, OperationPurpose::Topology, false));
            }
        }
        let _ = now_us;
        actions
    }

    /// Starts the configured grace period after the control peer disconnects.
    pub fn on_control_disconnected(&mut self, now_us: u64) -> Vec<HostAction> {
        if self.phase == HostPhase::AwaitingHello || self.phase == HostPhase::Closed {
            return Vec::new();
        }
        self.control_disconnect_deadline_us.get_or_insert_with(|| {
            now_us.saturating_add(self.config.control_disconnect_timeout_us)
        });
        self.phase = HostPhase::ControlDisconnected;
        vec![HostAction::Event(SessionEvent::Reconnecting)]
    }

    /// Cancels a pending stop when the viewer reconnects before the timeout.
    pub fn on_control_reconnected(&mut self) -> Vec<HostAction> {
        if self.phase != HostPhase::ControlDisconnected {
            return Vec::new();
        }
        self.control_disconnect_deadline_us = None;
        self.phase = if self.recovery.is_some() {
            HostPhase::RecoveringCapture
        } else if self.video_paused {
            HostPhase::Paused
        } else {
            HostPhase::Streaming
        };
        let mut actions = vec![HostAction::Event(SessionEvent::Reconnected)];
        if self.recovery.is_none() && !self.video_paused {
            actions.push(HostAction::Encoder(EncoderAction::SetPaused(false)));
        }
        actions
    }

    /// Handles viewer pause. Video production stops while control remains live.
    pub fn on_pause(&mut self) -> Vec<HostAction> {
        if self.phase == HostPhase::AwaitingHello || self.phase == HostPhase::Closed {
            return Vec::new();
        }
        self.video_paused = true;
        self.phase = HostPhase::Paused;
        let mut actions = vec![HostAction::Encoder(EncoderAction::SetPaused(true))];
        if let Some(display_id) = self.selected_display {
            self.epoch = self.epoch.wrapping_add(1);
            actions.push(self.reset_action(0, display_id.get(), StreamStatus::Paused));
        }
        actions
    }

    /// Resumes video and requests a fresh epoch and IDR when capture is available.
    pub fn on_resume(&mut self) -> Vec<HostAction> {
        if self.phase == HostPhase::AwaitingHello || self.phase == HostPhase::Closed {
            return Vec::new();
        }
        self.video_paused = false;
        if self.recovery.is_some() {
            self.phase = HostPhase::RecoveringCapture;
            return Vec::new();
        }
        self.phase = HostPhase::Streaming;
        let Some(display_id) = self.selected_display else {
            return Vec::new();
        };
        self.epoch = self.epoch.wrapping_add(1);
        vec![
            HostAction::Encoder(EncoderAction::SetPaused(false)),
            self.ok_reset(display_id, 0),
            HostAction::Encoder(EncoderAction::ForceKeyframe { epoch: self.epoch }),
        ]
    }

    /// Processes a viewer keyframe request if its epoch is current.
    pub fn on_keyframe_request(&mut self, request: RequestKeyframe) -> Vec<HostAction> {
        if self.phase == HostPhase::Streaming && !self.video_paused && request.epoch == self.epoch {
            vec![HostAction::Encoder(EncoderAction::ForceKeyframe {
                epoch: self.epoch,
            })]
        } else {
            Vec::new()
        }
    }

    /// Starts bounded capture recovery after the capture adapter reports loss.
    pub fn on_capture_lost(&mut self, reason: RecoveryReason, now_us: u64) -> Vec<HostAction> {
        if self.phase == HostPhase::Closed || self.phase == HostPhase::AwaitingHello {
            return Vec::new();
        }
        let interrupted = self.pending.take();
        let Some(display_id) = interrupted
            .map(|operation| operation.target)
            .or(self.selected_display)
        else {
            return Vec::new();
        };
        let req_id = interrupted.map_or(0, |operation| request_id(operation.purpose));
        self.recovery = Some(CaptureRecovery {
            target: display_id,
            attempts: 0,
            retry_at_us: now_us.saturating_add(INITIAL_CAPTURE_RETRY_US),
        });
        self.phase = if self.control_disconnect_deadline_us.is_some() {
            HostPhase::ControlDisconnected
        } else {
            HostPhase::RecoveringCapture
        };
        self.epoch = self.epoch.wrapping_add(1);
        vec![
            HostAction::Encoder(EncoderAction::SetPaused(true)),
            self.reset_action(req_id, display_id.get(), StreamStatus::Paused),
            HostAction::Event(SessionEvent::CaptureLost(reason)),
        ]
    }

    /// Advances capture recovery when its retry deadline has elapsed.
    pub fn tick(&mut self, now_us: u64) -> Vec<HostAction> {
        if self
            .control_disconnect_deadline_us
            .is_some_and(|deadline| now_us >= deadline)
        {
            self.control_disconnect_deadline_us = None;
            self.phase = HostPhase::Closed;
            self.pending = None;
            self.recovery = None;
            self.selected_display = None;
            self.selected_stream_dimensions = None;
            self.encoder_rebuild = None;
            return vec![
                HostAction::Capture(CaptureAction::Stop),
                HostAction::Encoder(EncoderAction::SetPaused(true)),
                HostAction::Event(SessionEvent::ControlTimedOut),
                HostAction::Event(SessionEvent::SessionEnded),
            ];
        }
        let Some(mut recovery) = self.recovery else {
            return Vec::new();
        };
        if now_us < recovery.retry_at_us || self.pending.is_some() {
            return Vec::new();
        }
        let display_id = recovery.target;
        recovery.attempts = recovery.attempts.saturating_add(1);
        self.recovery = Some(recovery);
        let operation_id = self.allocate_operation_id();
        self.pending = Some(PendingOperation {
            id: operation_id,
            target: display_id,
            purpose: OperationPurpose::Recovery,
            stage: OperationStage::Capture,
            stream_dimensions: None,
        });
        vec![HostAction::Capture(CaptureAction::Recreate {
            operation_id,
            display_id,
        })]
    }

    /// Adapts quality after a path or MTU transition. Quality remains host-owned.
    pub fn on_network_path_changed(
        &mut self,
        path: NetworkPath,
        mtu_changed: bool,
    ) -> Vec<HostAction> {
        let previous = self.network_path;
        if previous == path && !mtu_changed {
            return Vec::new();
        }
        self.network_path = path;
        let mut actions = vec![HostAction::Event(SessionEvent::NetworkPathChanged(path))];
        if self.selected_display.is_some() && self.phase == HostPhase::Streaming {
            actions.push(HostAction::Encoder(EncoderAction::ForceKeyframe {
                epoch: self.epoch,
            }));
        }
        if path == NetworkPath::Derp || mtu_changed {
            actions.push(HostAction::Quality(QualityAction::StepDownOneTier));
        } else if previous == NetworkPath::Derp && path == NetworkPath::Direct {
            actions.push(HostAction::Quality(QualityAction::RestoreGradually));
        }
        actions
    }

    /// Rebuilds a failed encoder. Repeated hardware failure selects OpenH264.
    pub fn on_encoder_failure(&mut self) -> Vec<HostAction> {
        if self.phase == HostPhase::AwaitingHello || self.phase == HostPhase::Closed {
            return Vec::new();
        }
        self.encoder_failures = self.encoder_failures.saturating_add(1);
        let use_software = self.software_encoder || self.encoder_failures >= 2;
        let operation_id = self.allocate_operation_id();
        self.encoder_rebuild = Some((operation_id, use_software));
        let mut actions = vec![HostAction::Encoder(EncoderAction::Rebuild {
            operation_id,
            use_software,
        })];
        if use_software && !self.software_encoder {
            actions.push(HostAction::Event(SessionEvent::EncoderFallbackToSoftware));
        }
        actions
    }

    /// Completes a previously requested encoder rebuild, ignoring stale results.
    pub fn on_encoder_rebuild_result(
        &mut self,
        operation_id: u64,
        success: bool,
    ) -> Vec<HostAction> {
        let Some((expected_id, use_software)) = self.encoder_rebuild else {
            return Vec::new();
        };
        if expected_id != operation_id {
            return Vec::new();
        }
        self.encoder_rebuild = None;
        if !success {
            if use_software {
                let failed_target = self.encoder_rebuild_target.take().or(self.selected_display);
                self.pending = None;
                self.recovery = None;
                self.epoch = self.epoch.wrapping_add(1);
                let mut actions = vec![HostAction::Encoder(EncoderAction::SetPaused(true))];
                if let Some(display_id) = failed_target {
                    actions.push(self.reset_action(
                        0,
                        display_id.get(),
                        StreamStatus::EncoderFailed,
                    ));
                }
                actions.push(HostAction::Event(SessionEvent::EncoderFailed));
                return actions;
            }
            return self.on_encoder_failure();
        }
        self.software_encoder = use_software;
        self.encoder_failures = 0;
        if let Some(display_id) = self.encoder_rebuild_target.take() {
            let Some(stream_dimensions) = self.stream_dimensions_for(display_id) else {
                return vec![self.reset_action(0, display_id.get(), StreamStatus::DisplayNotFound)];
            };
            let operation_id = self.allocate_operation_id();
            self.pending = Some(PendingOperation {
                id: operation_id,
                target: display_id,
                purpose: OperationPurpose::Initial,
                stage: OperationStage::Encoder,
                stream_dimensions: Some(stream_dimensions),
            });
            return vec![HostAction::Encoder(EncoderAction::Configure {
                operation_id,
                display_id,
                width: stream_dimensions.width,
                height: stream_dimensions.height,
            })];
        }
        let Some(display_id) = self.selected_display else {
            return Vec::new();
        };
        self.epoch = self.epoch.wrapping_add(1);
        vec![
            self.ok_reset(display_id, 0),
            HostAction::Encoder(EncoderAction::ForceKeyframe { epoch: self.epoch }),
        ]
    }

    /// Closes the session after a Goodbye control message.
    pub fn on_goodbye(&mut self, _goodbye: Goodbye) -> Vec<HostAction> {
        self.phase = HostPhase::Closed;
        self.pending = None;
        self.recovery = None;
        self.selected_display = None;
        self.selected_stream_dimensions = None;
        vec![
            HostAction::Capture(CaptureAction::Stop),
            HostAction::Encoder(EncoderAction::SetPaused(true)),
            HostAction::Event(SessionEvent::SessionEnded),
        ]
    }

    fn begin_operation(
        &mut self,
        target: DisplayId,
        purpose: OperationPurpose,
        recreate: bool,
    ) -> Vec<HostAction> {
        let operation_id = self.allocate_operation_id();
        let from = self.selected_display;
        self.pending = Some(PendingOperation {
            id: operation_id,
            target,
            purpose,
            stage: OperationStage::Capture,
            stream_dimensions: None,
        });
        let action = if recreate {
            CaptureAction::Recreate {
                operation_id,
                display_id: target,
            }
        } else {
            CaptureAction::SwitchDisplay {
                operation_id,
                from,
                to: target,
            }
        };
        vec![HostAction::Capture(action)]
    }

    fn operation_failed(
        &mut self,
        pending: PendingOperation,
        now_us: u64,
        encoder_failed: bool,
    ) -> Vec<HostAction> {
        if self.pending.is_some_and(|current| current.id == pending.id) {
            self.pending = None;
        }
        if pending.purpose == OperationPurpose::Recovery {
            let attempts = self.recovery.map_or(1, |recovery| recovery.attempts.max(1));
            let delay = capture_retry_delay(attempts.saturating_add(1));
            let target = self
                .recovery
                .map_or(pending.target, |recovery| recovery.target);
            self.recovery = Some(CaptureRecovery {
                target,
                attempts,
                retry_at_us: now_us.saturating_add(delay),
            });
            return Vec::new();
        }
        let status = if encoder_failed {
            StreamStatus::EncoderFailed
        } else {
            StreamStatus::CaptureFailed
        };
        let actions =
            vec![self.reset_action(request_id(pending.purpose), pending.target.get(), status)];
        if matches!(pending.purpose, OperationPurpose::Switch(_)) {
            self.phase = if self.control_disconnect_deadline_us.is_some() {
                HostPhase::ControlDisconnected
            } else if self.video_paused {
                HostPhase::Paused
            } else {
                HostPhase::Streaming
            };
        }
        actions
    }

    fn allocate_operation_id(&mut self) -> u64 {
        let id = self.next_operation_id.max(1);
        self.next_operation_id = id.wrapping_add(1).max(1);
        id
    }

    fn display_is_available(&self, id: DisplayId) -> bool {
        self.topology
            .displays()
            .iter()
            .find(|display| display.id() == id)
            .is_some_and(|display| display.flags().available())
    }

    fn stream_dimensions_for(&self, id: DisplayId) -> Option<StreamDimensions> {
        self.topology
            .displays()
            .iter()
            .find(|display| display.id() == id)
            .and_then(|display| {
                let (width, height) = display.size();
                bounded_stream_dimensions(width, height)
            })
    }

    fn reset_action(&mut self, req_id: u32, display_id: u32, status: StreamStatus) -> HostAction {
        let (width, height) = DisplayId::new(display_id)
            .and_then(|id| self.stream_dimensions_for(id))
            .map(|dimensions| (dimensions.width, dimensions.height))
            .unwrap_or((0, 0));
        HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
            req_id,
            epoch: self.epoch,
            codec: StreamCodec::H264,
            width,
            height,
            fps: STREAM_FPS,
            topology_rev: self.topology.revision(),
            display_id,
            status,
        }))
    }

    fn ok_reset(&mut self, display_id: DisplayId, req_id: u32) -> HostAction {
        let dimensions = self
            .selected_display
            .filter(|selected| *selected == display_id)
            .and(self.selected_stream_dimensions)
            .or_else(|| self.stream_dimensions_for(display_id));
        let (width, height) = dimensions
            .map(|value| (value.width, value.height))
            .unwrap_or((0, 0));
        HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
            req_id,
            epoch: self.epoch,
            codec: StreamCodec::H264,
            width,
            height,
            fps: STREAM_FPS,
            topology_rev: self.topology.revision(),
            display_id: display_id.get(),
            status: StreamStatus::Ok,
        }))
    }
}

fn request_id(purpose: OperationPurpose) -> u32 {
    match purpose {
        OperationPurpose::Switch(request_id) => request_id,
        OperationPurpose::Initial | OperationPurpose::Topology | OperationPurpose::Recovery => 0,
    }
}

/// Bounds output to 1920x1080 and scales short sources up to 480px high when possible.
///
/// Aspect ratio is preserved to integer-pixel rounding. At extreme aspect ratios,
/// the width cap can make the 480px minimum impossible; in that case this helper
/// uses the largest size that fits the cap without distorting the source.
/// Returns `None` for a zero-sized source display.
pub fn bounded_stream_dimensions(width_px: u32, height_px: u32) -> Option<StreamDimensions> {
    if width_px == 0 || height_px == 0 {
        return None;
    }
    let max_width = u64::from(MAX_STREAM_WIDTH_PX);
    let min_height = u64::from(MIN_STREAM_HEIGHT_PX);
    let max_height = u64::from(MAX_STREAM_HEIGHT_PX);
    let source_width = u64::from(width_px);
    let source_height = u64::from(height_px);

    let (width, height) =
        if source_width <= max_width && source_height <= max_height && source_height < min_height {
            if source_width * min_height <= max_width * source_height {
                (
                    ((source_width * min_height + source_height / 2) / source_height).max(1),
                    min_height,
                )
            } else {
                (
                    max_width,
                    ((source_height * max_width + source_width / 2) / source_width).max(1),
                )
            }
        } else if source_width <= max_width && source_height <= max_height {
            (source_width, source_height)
        } else if source_width * max_height >= source_height * max_width {
            (
                max_width,
                ((source_height * max_width + source_width / 2) / source_width).max(1),
            )
        } else {
            (
                ((source_width * max_height + source_height / 2) / source_height).max(1),
                max_height,
            )
        };
    Some(StreamDimensions {
        width: width.min(max_width) as u16,
        height: height.min(max_height) as u16,
    })
}

/// Returns the 50, 100, 200, 400, 800, then 1000 ms retry delay.
pub fn capture_retry_delay(attempt: u32) -> u64 {
    let shift = attempt.saturating_sub(1).min(31);
    min(
        INITIAL_CAPTURE_RETRY_US.saturating_mul(1_u64 << shift),
        MAX_CAPTURE_RETRY_US,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_proto::{GoodbyeReason, OsType, PROTOCOL_VERSION};
    use racc_topology::{Display, DisplayFlags};

    fn display(raw: u32, available: bool, primary: bool) -> Display {
        display_with_size(raw, available, primary, 1920, 1080)
    }

    fn display_with_size(
        raw: u32,
        available: bool,
        primary: bool,
        width: u32,
        height: u32,
    ) -> Display {
        Display::new(
            DisplayId::new(raw).expect("nonzero test id"),
            format!("Display {raw}"),
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
        Topology::new(revision, displays, None).expect("valid test topology")
    }

    fn host() -> HostSession {
        HostSession::new(
            HelloAck {
                protocol_version: PROTOCOL_VERSION,
                status: HelloStatus::Ok,
                device_name: "host".to_owned(),
                os: OsType::Windows,
                app_version: "test".to_owned(),
                codecs: 1,
                max_height: 1080,
                features: 0,
                host_cpu_cores: 8,
            },
            topology(1, vec![display(1, true, true), display(2, true, false)]),
        )
    }

    fn hello() -> Hello {
        Hello {
            protocol_version: PROTOCOL_VERSION,
            device_name: "viewer".to_owned(),
            os: OsType::Windows,
            app_version: "test".to_owned(),
            video_udp_port: 5000,
            codecs: 1,
            max_height: 1080,
            features: 0,
        }
    }

    #[test]
    fn stream_dimensions_are_capped_and_preserve_aspect_ratio() {
        assert_eq!(
            bounded_stream_dimensions(3840, 2160),
            Some(StreamDimensions {
                width: 1920,
                height: 1080,
            })
        );
        assert_eq!(
            bounded_stream_dimensions(3440, 1440),
            Some(StreamDimensions {
                width: 1920,
                height: 804,
            })
        );
        assert_eq!(
            bounded_stream_dimensions(1280, 720),
            Some(StreamDimensions {
                width: 1280,
                height: 720,
            })
        );
        assert_eq!(
            bounded_stream_dimensions(640, 360),
            Some(StreamDimensions {
                width: 853,
                height: 480,
            })
        );
        assert_eq!(
            bounded_stream_dimensions(10_000, 1_000),
            Some(StreamDimensions {
                width: 1920,
                height: 192,
            })
        );
        assert_eq!(bounded_stream_dimensions(0, 720), None);
    }

    #[test]
    fn configure_action_and_stream_reset_share_capped_4k_dimensions() {
        let mut host = host();
        host.topology = topology(
            1,
            vec![
                display_with_size(1, true, true, 3840, 2160),
                display(2, true, false),
            ],
        );
        let initial = connect(&mut host);
        let configure = host.on_capture_result(initial, Ok(()), 10);
        assert!(configure.iter().any(|action| matches!(
            action,
            HostAction::Encoder(EncoderAction::Configure {
                width: 1920,
                height: 1080,
                ..
            })
        )));
        let completed = host.on_encoder_configured(initial, Ok(()), 11);
        assert!(completed.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                width: 1920,
                height: 1080,
                status: StreamStatus::Ok,
                ..
            }))
        )));
    }

    #[test]
    fn hello_without_h264_is_rejected_before_capture_starts() {
        let mut host = host();
        let mut peer = hello();
        peer.codecs = 0;
        let actions = host.on_hello(&peer);
        assert!(actions.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::HelloAck(HelloAck {
                status: HelloStatus::Busy,
                ..
            }))
        )));
        assert!(!actions
            .iter()
            .any(|action| matches!(action, HostAction::Capture(_))));
        assert_eq!(host.phase(), HostPhase::AwaitingHello);
    }

    fn operation_id(actions: &[HostAction]) -> u64 {
        actions
            .iter()
            .find_map(|action| match action {
                HostAction::Capture(CaptureAction::SwitchDisplay { operation_id, .. })
                | HostAction::Capture(CaptureAction::Recreate { operation_id, .. }) => {
                    Some(*operation_id)
                }
                _ => None,
            })
            .expect("capture operation emitted")
    }

    fn connect(host: &mut HostSession) -> u64 {
        let actions = host.on_hello(&hello());
        assert!(actions.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::HelloAck(HelloAck {
                status: HelloStatus::Ok,
                ..
            }))
        )));
        operation_id(&actions)
    }

    fn finish_capture(host: &mut HostSession, operation_id: u64) -> Vec<HostAction> {
        let configure = host.on_capture_result(operation_id, Ok(()), 10);
        assert!(configure.iter().any(|action| matches!(
            action,
            HostAction::Encoder(EncoderAction::Configure { operation_id: id, .. })
                if *id == operation_id
        )));
        host.on_encoder_configured(operation_id, Ok(()), 10)
    }

    #[test]
    fn handshake_and_monitor_switch_increment_epoch_and_force_idr() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        let id_one = DisplayId::new(1).expect("nonzero test id");
        assert_eq!(host.selected_display(), Some(id_one));
        assert_eq!(host.epoch(), 1);

        let actions = host.on_switch_monitor(SwitchMonitor {
            req_id: 42,
            display_id: 2,
        });
        let switch = operation_id(&actions);
        let actions = finish_capture(&mut host, switch);
        assert_eq!(
            host.selected_display(),
            Some(DisplayId::new(2).expect("id"))
        );
        assert_eq!(host.epoch(), 2);
        assert!(actions.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                req_id: 42,
                epoch: 2,
                status: StreamStatus::Ok,
                display_id: 2,
                ..
            }))
        )));
        assert!(
            actions.contains(&HostAction::Encoder(EncoderAction::ForceKeyframe {
                epoch: 2
            }))
        );
    }

    #[test]
    fn late_switch_completion_is_ignored_after_newer_request() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        let first = operation_id(&host.on_switch_monitor(SwitchMonitor {
            req_id: 10,
            display_id: 2,
        }));
        let second = operation_id(&host.on_switch_monitor(SwitchMonitor {
            req_id: 11,
            display_id: 1,
        }));
        assert_ne!(first, second);
        assert!(host.on_capture_result(first, Ok(()), 20).is_empty());
        finish_capture(&mut host, second);
        assert_eq!(
            host.selected_display(),
            Some(DisplayId::new(1).expect("id"))
        );
    }

    #[test]
    fn capture_loss_during_switch_recovers_the_requested_display() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        let switch_actions = host.on_switch_monitor(SwitchMonitor {
            req_id: 55,
            display_id: 2,
        });
        let stale_switch = operation_id(&switch_actions);
        let loss = host.on_capture_lost(RecoveryReason::AccessLost, 1_000);
        assert!(loss.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                req_id: 55,
                status: StreamStatus::Paused,
                display_id: 2,
                ..
            }))
        )));
        assert!(host
            .on_capture_result(stale_switch, Ok(()), 1_001)
            .is_empty());

        let recovery = operation_id(&host.tick(51_000));
        assert!(matches!(
            host.on_capture_result(recovery, Ok(()), 51_000).as_slice(),
            [HostAction::Encoder(EncoderAction::Configure { display_id, .. })]
                if *display_id == DisplayId::new(2).expect("id")
        ));
        let completed = host.on_encoder_configured(recovery, Ok(()), 51_001);
        assert_eq!(
            host.selected_display(),
            Some(DisplayId::new(2).expect("id"))
        );
        assert!(completed.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                req_id: 0,
                status: StreamStatus::Ok,
                display_id: 2,
                ..
            }))
        )));
        assert!(completed.contains(&HostAction::Encoder(EncoderAction::SetPaused(false))));
    }

    #[test]
    fn selected_display_removal_preserves_a_switch_to_an_available_target() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        let switch = operation_id(&host.on_switch_monitor(SwitchMonitor {
            req_id: 71,
            display_id: 2,
        }));

        let topology_actions =
            host.on_topology_update(topology(2, vec![display(2, true, true)]), 20);
        assert!(topology_actions.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                req_id: 0,
                display_id: 1,
                status: StreamStatus::DisplayNotFound,
                ..
            }))
        )));
        assert!(!topology_actions.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                req_id: 71,
                status: StreamStatus::DisplayNotFound,
                ..
            }))
        )));
        let configure = host.on_capture_result(switch, Ok(()), 21);
        assert!(configure.iter().any(|action| matches!(
            action,
            HostAction::Encoder(EncoderAction::Configure { display_id, .. })
                if *display_id == DisplayId::new(2).expect("id")
        )));
        let completed = host.on_encoder_configured(switch, Ok(()), 22);
        assert!(completed.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                req_id: 71,
                display_id: 2,
                status: StreamStatus::Ok,
                ..
            }))
        )));
    }

    #[test]
    fn pending_switch_geometry_change_restarts_and_preserves_request_id() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        let switch = operation_id(&host.on_switch_monitor(SwitchMonitor {
            req_id: 72,
            display_id: 2,
        }));
        let first_config = host.on_capture_result(switch, Ok(()), 20);
        assert!(first_config.iter().any(|action| matches!(
            action,
            HostAction::Encoder(EncoderAction::Configure {
                width: 1920,
                height: 1080,
                ..
            })
        )));

        let replacement = topology(
            2,
            vec![
                display(1, true, true),
                display_with_size(2, true, false, 3440, 1440),
            ],
        );
        let restarted = host.on_topology_update(replacement, 21);
        let fresh_operation = operation_id(&restarted);
        assert_ne!(fresh_operation, switch);
        assert!(host.on_encoder_configured(switch, Ok(()), 22).is_empty());
        let configure = host.on_capture_result(fresh_operation, Ok(()), 23);
        assert!(configure.iter().any(|action| matches!(
            action,
            HostAction::Encoder(EncoderAction::Configure {
                width: 1920,
                height: 804,
                ..
            })
        )));
        let completed = host.on_encoder_configured(fresh_operation, Ok(()), 24);
        assert!(completed.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                req_id: 72,
                width: 1920,
                height: 804,
                status: StreamStatus::Ok,
                ..
            }))
        )));
    }

    #[test]
    fn removed_display_is_reported_and_cannot_be_selected_again() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        let removed_id = host.selected_display().expect("initial selection");
        let actions = host.on_topology_update(topology(2, vec![display(2, true, true)]), 100);
        assert_eq!(host.selected_display(), None);
        assert!(actions.iter().any(|action| matches!(
            action,
            HostAction::Event(SessionEvent::DisplayUnavailable(id)) if *id == removed_id
        )));
        assert!(actions.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                status: StreamStatus::DisplayNotFound,
                display_id: 1,
                ..
            }))
        )));
        let actions = host.on_switch_monitor(SwitchMonitor {
            req_id: 99,
            display_id: 1,
        });
        assert!(actions.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                req_id: 99,
                status: StreamStatus::DisplayNotFound,
                ..
            }))
        )));
    }

    #[test]
    fn switch_requested_during_capture_recovery_becomes_recovery_target() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        host.on_capture_lost(RecoveryReason::AccessLost, 0);
        let actions = host.on_switch_monitor(SwitchMonitor {
            req_id: 77,
            display_id: 2,
        });
        assert!(actions.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                req_id: 77,
                epoch: 3,
                display_id: 2,
                status: StreamStatus::Paused,
                ..
            }))
        )));
        assert!(actions
            .iter()
            .all(|action| !matches!(action, HostAction::Capture(_))));

        let recovery = operation_id(&host.tick(50_000));
        assert!(matches!(
            host.on_capture_result(recovery, Ok(()), 50_000).as_slice(),
            [HostAction::Encoder(EncoderAction::Configure { display_id, .. })]
                if *display_id == DisplayId::new(2).expect("id")
        ));
        let completed = host.on_encoder_configured(recovery, Ok(()), 50_001);
        assert_eq!(
            host.selected_display(),
            Some(DisplayId::new(2).expect("id"))
        );
        assert!(completed.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                req_id: 0,
                status: StreamStatus::Ok,
                display_id: 2,
                ..
            }))
        )));
    }

    #[test]
    fn pending_switch_to_removed_display_is_rejected() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        let switch_actions = host.on_switch_monitor(SwitchMonitor {
            req_id: 22,
            display_id: 2,
        });
        let stale_operation = operation_id(&switch_actions);
        let actions = host.on_topology_update(topology(2, vec![display(1, true, true)]), 100);
        assert!(actions.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                req_id: 22,
                display_id: 2,
                status: StreamStatus::DisplayNotFound,
                ..
            }))
        )));
        assert!(host
            .on_capture_result(stale_operation, Ok(()), 101)
            .is_empty());
        assert_eq!(
            host.selected_display(),
            Some(DisplayId::new(1).expect("id"))
        );
    }

    #[test]
    fn pause_resume_and_capture_recovery_advance_epochs_once() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        let epoch = host.epoch();

        let paused = host.on_pause();
        assert_eq!(host.epoch(), epoch.wrapping_add(1));
        assert!(paused.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                status: StreamStatus::Paused,
                ..
            }))
        )));
        let resumed = host.on_resume();
        assert_eq!(host.epoch(), epoch.wrapping_add(2));
        assert!(resumed.iter().any(|action| matches!(
            action,
            HostAction::Encoder(EncoderAction::ForceKeyframe { .. })
        )));

        let lost = host.on_capture_lost(RecoveryReason::AccessLost, 1_000);
        assert_eq!(host.epoch(), epoch.wrapping_add(3));
        assert!(lost.iter().any(|action| matches!(
            action,
            HostAction::Event(SessionEvent::CaptureLost(RecoveryReason::AccessLost))
        )));
        assert!(host.tick(50_999).is_empty());
        let retry = host.tick(51_000);
        let retry_id = operation_id(&retry);
        assert!(matches!(
            retry.first(),
            Some(HostAction::Capture(CaptureAction::Recreate { .. }))
        ));
        assert!(host
            .on_capture_result(retry_id, Err(CaptureFailure::Unavailable), 51_000)
            .is_empty());
        assert!(host.tick(150_999).is_empty());
        let second_retry = operation_id(&host.tick(151_000));
        let recovered = finish_capture(&mut host, second_retry);
        assert_eq!(host.epoch(), epoch.wrapping_add(4));
        assert!(recovered
            .iter()
            .any(|action| matches!(action, HostAction::Event(SessionEvent::RecoverySucceeded))));
    }

    #[test]
    fn recovery_stays_paused_while_disconnected_and_unpauses_on_reconnect() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        host.on_control_disconnected(1_000);
        host.on_capture_lost(RecoveryReason::AccessLost, 2_000);
        let recovery = operation_id(&host.tick(52_000));
        host.on_capture_result(recovery, Ok(()), 52_000);
        let completed = host.on_encoder_configured(recovery, Ok(()), 52_001);
        assert!(!completed.contains(&HostAction::Encoder(EncoderAction::SetPaused(false))));
        let reconnected = host.on_control_reconnected();
        assert!(reconnected.contains(&HostAction::Encoder(EncoderAction::SetPaused(false))));
    }

    #[test]
    fn recovery_respects_hidden_pause_until_resume() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        host.on_pause();
        host.on_capture_lost(RecoveryReason::AccessLost, 2_000);
        let recovery = operation_id(&host.tick(52_000));
        host.on_capture_result(recovery, Ok(()), 52_000);
        let completed = host.on_encoder_configured(recovery, Ok(()), 52_001);
        assert!(!completed.contains(&HostAction::Encoder(EncoderAction::SetPaused(false))));
        assert!(host
            .on_resume()
            .contains(&HostAction::Encoder(EncoderAction::SetPaused(false))));
    }

    #[test]
    fn capture_backoff_is_capped_at_the_required_ladder() {
        assert_eq!(
            (1..=7).map(capture_retry_delay).collect::<Vec<_>>(),
            vec![50_000, 100_000, 200_000, 400_000, 800_000, 1_000_000, 1_000_000]
        );
    }

    #[test]
    fn disconnect_timeout_defaults_to_five_seconds() {
        assert_eq!(
            HostConfig::default().control_disconnect_timeout_us,
            DEFAULT_CONTROL_DISCONNECT_TIMEOUT_US
        );
        assert_eq!(DEFAULT_CONTROL_DISCONNECT_TIMEOUT_US, 5_000_000);
    }

    #[test]
    fn capture_retry_transitions_follow_the_backoff_ladder() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        host.on_capture_lost(RecoveryReason::AccessLost, 0);

        let delays = [50_000, 100_000, 200_000, 400_000, 800_000, 1_000_000];
        let mut now_us: u64 = 0;
        for (index, delay_us) in delays.into_iter().enumerate() {
            now_us = now_us.saturating_add(delay_us);
            let operation = operation_id(&host.tick(now_us));
            if index + 1 < delays.len() {
                assert!(host
                    .on_capture_result(operation, Err(CaptureFailure::Unavailable), now_us,)
                    .is_empty());
                let next_delay = delays[index + 1];
                assert!(host.tick(now_us + next_delay - 1).is_empty());
            }
        }
    }

    #[test]
    fn disconnected_stream_stops_only_after_configured_timeout() {
        let mut host = host();
        host.config = HostConfig {
            control_disconnect_timeout_us: 100,
        };
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        host.on_control_disconnected(1_000);
        assert_eq!(host.phase(), HostPhase::ControlDisconnected);
        assert!(host.tick(1_099).is_empty());
        let stopped = host.tick(1_100);
        assert_eq!(host.phase(), HostPhase::Closed);
        assert!(stopped.contains(&HostAction::Capture(CaptureAction::Stop)));
        assert!(stopped
            .iter()
            .any(|action| matches!(action, HostAction::Encoder(EncoderAction::SetPaused(true)))));
        assert!(stopped.contains(&HostAction::Event(SessionEvent::ControlTimedOut)));
        assert!(stopped.contains(&HostAction::Event(SessionEvent::SessionEnded)));
    }

    #[test]
    fn goodbye_stops_capture_and_ends_the_session() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        let actions = host.on_goodbye(Goodbye {
            reason: GoodbyeReason::Normal,
        });
        assert_eq!(host.phase(), HostPhase::Closed);
        assert!(actions.contains(&HostAction::Capture(CaptureAction::Stop)));
        assert!(actions.contains(&HostAction::Event(SessionEvent::SessionEnded)));
    }

    #[test]
    fn hello_reconnect_cancels_timeout_and_starts_a_fresh_epoch() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        let old_epoch = host.epoch();
        host.on_control_disconnected(1_000);
        let actions = host.on_hello(&hello());
        assert!(actions.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::HelloAck(HelloAck {
                status: HelloStatus::Ok,
                ..
            }))
        )));
        assert!(actions.contains(&HostAction::Event(SessionEvent::Reconnected)));
        assert_eq!(host.phase(), HostPhase::Streaming);
        assert_eq!(host.epoch(), old_epoch.wrapping_add(1));
        assert!(actions.iter().any(|action| matches!(
            action,
            HostAction::Encoder(EncoderAction::ForceKeyframe { .. })
        )));
        assert!(host.tick(10_000_000).is_empty());
    }

    #[test]
    fn reconnect_cancels_control_disconnect_stop_deadline() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        host.on_control_disconnected(1_000);
        assert!(host
            .on_control_reconnected()
            .contains(&HostAction::Event(SessionEvent::Reconnected)));
        assert_eq!(host.phase(), HostPhase::Streaming);
        assert!(host.tick(10_000_000).is_empty());
    }

    #[test]
    fn initial_configure_failure_rebuild_resumes_the_initial_target() {
        let mut host = host();
        let initial = connect(&mut host);
        host.on_capture_result(initial, Ok(()), 10);
        let failed = host.on_encoder_configured(initial, Err(EncoderFailure::ConfigureFailed), 11);
        let rebuild_id = failed
            .iter()
            .find_map(|action| match action {
                HostAction::Encoder(EncoderAction::Rebuild { operation_id, .. }) => {
                    Some(*operation_id)
                }
                _ => None,
            })
            .expect("rebuild requested");
        let configure = host.on_encoder_rebuild_result(rebuild_id, true);
        let configure_id = configure
            .iter()
            .find_map(|action| match action {
                HostAction::Encoder(EncoderAction::Configure {
                    operation_id,
                    display_id,
                    width: 1920,
                    height: 1080,
                }) if *display_id == DisplayId::new(1).expect("id") => Some(*operation_id),
                _ => None,
            })
            .expect("initial target configure resumed");
        let completed = host.on_encoder_configured(configure_id, Ok(()), 12);
        assert_eq!(
            host.selected_display(),
            Some(DisplayId::new(1).expect("id"))
        );
        assert!(completed.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                status: StreamStatus::Ok,
                display_id: 1,
                ..
            }))
        )));
        assert!(completed.iter().any(|action| matches!(
            action,
            HostAction::Encoder(EncoderAction::ForceKeyframe { .. })
        )));
    }

    #[test]
    fn software_rebuild_failure_ends_initial_stream_and_clears_rebuild_target() {
        let mut host = host();
        let initial = connect(&mut host);
        host.on_capture_result(initial, Ok(()), 10);
        let configure_failed =
            host.on_encoder_configured(initial, Err(EncoderFailure::ConfigureFailed), 11);
        let hardware_id = configure_failed
            .iter()
            .find_map(|action| match action {
                HostAction::Encoder(EncoderAction::Rebuild {
                    operation_id,
                    use_software: false,
                }) => Some(*operation_id),
                _ => None,
            })
            .expect("hardware rebuild requested");
        let fallback = host.on_encoder_rebuild_result(hardware_id, false);
        let software_id = fallback
            .iter()
            .find_map(|action| match action {
                HostAction::Encoder(EncoderAction::Rebuild {
                    operation_id,
                    use_software: true,
                }) => Some(*operation_id),
                _ => None,
            })
            .expect("software rebuild requested");

        let failed = host.on_encoder_rebuild_result(software_id, false);
        assert!(failed.contains(&HostAction::Encoder(EncoderAction::SetPaused(true))));
        assert!(failed.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                req_id: 0,
                display_id: 1,
                status: StreamStatus::EncoderFailed,
                ..
            }))
        )));
        assert!(failed.contains(&HostAction::Event(SessionEvent::EncoderFailed)));
        assert_eq!(host.encoder_rebuild_target, None);
        assert_eq!(host.encoder_rebuild, None);
    }

    #[test]
    fn software_rebuild_failure_ends_current_selected_stream() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        let pending_switch = operation_id(&host.on_switch_monitor(SwitchMonitor {
            req_id: 91,
            display_id: 2,
        }));
        let hardware = host.on_encoder_failure();
        let hardware_id = hardware
            .iter()
            .find_map(|action| match action {
                HostAction::Encoder(EncoderAction::Rebuild {
                    operation_id,
                    use_software: false,
                }) => Some(*operation_id),
                _ => None,
            })
            .expect("hardware rebuild requested");
        let fallback = host.on_encoder_rebuild_result(hardware_id, false);
        let software_id = fallback
            .iter()
            .find_map(|action| match action {
                HostAction::Encoder(EncoderAction::Rebuild {
                    operation_id,
                    use_software: true,
                }) => Some(*operation_id),
                _ => None,
            })
            .expect("software rebuild requested");

        let failed = host.on_encoder_rebuild_result(software_id, false);
        assert!(failed.contains(&HostAction::Encoder(EncoderAction::SetPaused(true))));
        assert!(failed.iter().any(|action| matches!(
            action,
            HostAction::SendControl(ControlMessage::StreamReset(StreamReset {
                req_id: 0,
                display_id: 1,
                status: StreamStatus::EncoderFailed,
                ..
            }))
        )));
        assert!(failed.contains(&HostAction::Event(SessionEvent::EncoderFailed)));
        assert_eq!(host.encoder_rebuild_target, None);
        assert_eq!(host.encoder_rebuild, None);
        assert!(host
            .on_capture_result(pending_switch, Ok(()), 20)
            .is_empty());
    }

    #[test]
    fn path_degradation_and_repeated_encoder_failure_emit_fallback_actions() {
        let mut host = host();
        let initial = connect(&mut host);
        finish_capture(&mut host, initial);
        let path = host.on_network_path_changed(NetworkPath::Derp, false);
        assert!(path.contains(&HostAction::Quality(QualityAction::StepDownOneTier)));
        let restored = host.on_network_path_changed(NetworkPath::Direct, false);
        assert!(restored.contains(&HostAction::Quality(QualityAction::RestoreGradually)));

        let hardware = host.on_encoder_failure();
        let hw_id = hardware
            .iter()
            .find_map(|action| match action {
                HostAction::Encoder(EncoderAction::Rebuild {
                    operation_id,
                    use_software: false,
                }) => Some(*operation_id),
                _ => None,
            })
            .expect("hardware rebuild first");
        let fallback = host.on_encoder_rebuild_result(hw_id, false);
        assert!(fallback.iter().any(|action| matches!(
            action,
            HostAction::Encoder(EncoderAction::Rebuild {
                use_software: true,
                ..
            })
        )));
        assert!(fallback.contains(&HostAction::Event(SessionEvent::EncoderFallbackToSoftware)));
    }
}
