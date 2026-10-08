//! Platform-neutral host session state and adapter boundary.
//!
//! This module performs no OS or network I/O. A host-agent worker supplies
//! Tailscale addresses and whois/allowlist decisions, executes emitted
//! HostSession actions through capture, encoder, and transport adapters, and
//! performs any listener bind. BindReady means only that a safe bind target
//! was validated; it does not claim that an OS socket is listening.
//!
//! Encoded video does not belong on this metadata/action interface.
use racc_proto::{
    ControlMessage, Goodbye, GoodbyeReason, Hello, HelloAck, HelloStatus, PauseVideo, Pong,
    QualityAdjustment, QualityAdjustmentReason, ResumeVideo, SetQuality, ViewerReport,
};
use racc_session::{
    CaptureFailure, EncoderAction, EncoderFailure, HostAction, HostConfig, HostPhase, HostSession,
    QualityController, QualityEvent, QualityFeedback, QualityPreference, QualityTier,
    RecoveryReason,
};
use racc_topology::{DisplayId, Topology};
use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Maximum pending peer approvals held in memory.
pub const MAX_PENDING_HOST_AUTHORIZATIONS: usize = 128;
/// Maximum byte length of a stable Tailscale peer key.
pub const MAX_HOST_PEER_KEY_BYTES: usize = 512;
/// Maximum byte length of a peer label shown in the approval UI.
pub const MAX_HOST_PEER_LABEL_BYTES: usize = 128;

/// Opaque connection identifier assigned by the control transport adapter.
pub type HostConnectionId = u64;

/// Result of the external whois and allowlist check for an incoming peer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostPeerAuthorization {
    /// Whois succeeded and the stable node id is approved.
    Approved {
        /// Stable Tailscale node id.
        peer_key: String,
    },
    /// Whois succeeded, but owner approval is required and access remains denied.
    Pending {
        /// Stable Tailscale node id.
        peer_key: String,
        /// Short machine label for the approval list.
        label: String,
    },
    /// Whois failed or the peer was explicitly rejected.
    Rejected,
}

/// A stable peer awaiting owner approval.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingHostAuthorization {
    /// Stable Tailscale node id.
    pub peer_key: String,
    /// Bounded display label.
    pub label: String,
}

/// Portable runtime lifecycle phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostRuntimePhase {
    /// Tailscale is unavailable or returned an address outside its prefixes.
    WaitingForTailscale,
    /// A validated Tailscale address is ready for the listener adapter to bind.
    BindReady,
    /// One authorized viewer occupies the session.
    ViewerConnected,
    /// Runtime state has been stopped by its owner.
    Stopped,
}

/// Coarse stream tier inferred from a successful StreamReset action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostQualityTier {
    /// 480p30 or lower.
    P480,
    /// Above 480p30 and up to 720p30.
    P720,
    /// Above 720p30 and up to 1080p30.
    P1080,
}

/// Largest local encoder-lag observation accepted by the host runtime.
pub const MAX_HOST_ENCODER_LAG_MS: u32 = 60_000;

/// One bounded sample from the local host video sender.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostSenderObservation {
    /// Stream epoch described by this sample.
    pub epoch: u16,
    /// One sender queue-overflow pulse since the previous observation.
    pub queue_overflow: bool,
    /// Measured encoder lag in milliseconds, when available.
    pub encoder_lag_ms: Option<u32>,
}

/// Bounded counters for a status panel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HostRuntimeCounters {
    /// Hello messages observed.
    pub hello_received: u64,
    /// Connections rejected by address, whois, or allowlist policy.
    pub rejected_connections: u64,
    /// Additional viewer attempts answered Busy.
    pub busy_connections: u64,
    /// New approval requests rejected because the queue was full.
    pub pending_queue_overflow: u64,
}

/// Metadata-only snapshot of host state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostRuntimeStatus {
    /// Runtime lifecycle phase.
    pub phase: HostRuntimePhase,
    /// Validated address intended for the listener adapter.
    pub bind_address: Option<SocketAddr>,
    /// Whether an authorized control viewer is connected.
    pub viewer_connected: bool,
    /// Active HostSession lifecycle phase.
    pub session_phase: HostPhase,
    /// Selected display, once capture setup succeeds.
    pub current_display: Option<DisplayId>,
    /// Stream tier, once a reset succeeds.
    pub quality_tier: Option<HostQualityTier>,
    /// Short encoder label supplied by the platform adapter.
    pub encoder_name: Option<String>,
    /// Number of pending approvals.
    pub pending_authorizations: usize,
    /// Runtime counters.
    pub counters: HostRuntimeCounters,
}

impl Default for HostRuntimeStatus {
    fn default() -> Self {
        Self {
            phase: HostRuntimePhase::WaitingForTailscale,
            bind_address: None,
            viewer_connected: false,
            session_phase: HostPhase::AwaitingHello,
            current_display: None,
            quality_tier: None,
            encoder_name: None,
            pending_authorizations: 0,
            counters: HostRuntimeCounters::default(),
        }
    }
}

/// Typed metadata or action for the host-agent adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostRuntimeEvent {
    /// Bind this validated address, or close the listener when address is None.
    BindAddressChanged(Option<SocketAddr>),
    /// A peer needs owner review and remains unauthorized.
    PendingAuthorization(PendingHostAuthorization),
    /// Send a protocol control message on one connection.
    SendControl {
        /// Transport adapter connection id.
        connection_id: HostConnectionId,
        /// Typed control message.
        message: ControlMessage,
    },
    /// Execute an action emitted by HostSession.
    SessionAction {
        /// Destination for SendControl actions, if any.
        connection_id: Option<HostConnectionId>,
        /// Typed action.
        action: HostAction,
    },
    /// Forward a viewer quality preference to the host quality policy.
    QualityPreference(SetQuality),
    /// Viewer telemetry feedback supplied to the host quality policy.
    ViewerFeedback(ViewerReport),
    /// Host quality policy decision, paired with any encoder/session actions.
    QualityDecision(QualityEvent),
    /// Close a rejected control connection.
    CloseConnection(HostConnectionId),
    /// The controller is stopped and adapters should release their resources.
    Stopped,
}

/// Configuration or owner-command error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostRuntimeError {
    /// Control port zero cannot be used as a service port.
    InvalidControlPort,
    /// Peer key is empty, oversized, or is not pending.
    InvalidPeerKey,
    /// An encoder-lag observation exceeds the 60-second input bound.
    InvalidQualityObservation,
    /// The loopback-only constructor was not used for a test bind request.
    #[cfg(feature = "loopback-test")]
    LoopbackTestDisabled,
    /// A test bind must use a loopback IP and a nonzero port.
    #[cfg(feature = "loopback-test")]
    InvalidLoopbackTestAddress,
    /// Runtime has already been stopped.
    Stopped,
}

impl std::fmt::Display for HostRuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidControlPort => "control port must be nonzero",
            Self::InvalidPeerKey => "peer key is invalid or not pending",
            Self::InvalidQualityObservation => "host quality observation exceeds its bound",
            #[cfg(feature = "loopback-test")]
            Self::LoopbackTestDisabled => {
                "loopback test binds require the explicit test constructor"
            }
            #[cfg(feature = "loopback-test")]
            Self::InvalidLoopbackTestAddress => {
                "test bind address must be loopback with a nonzero port"
            }
            Self::Stopped => "host runtime is stopped",
        })
    }
}
impl std::error::Error for HostRuntimeError {}

/// Deterministic host controller driven by a host-agent worker.
///
/// The identity adapter must do Tailscale whois and persistent allowlist
/// checks before passing an authorization result to on_hello. The transport,
/// capture, encoder, and sender adapters execute the returned typed events.
pub struct HostRuntime {
    capabilities: HelloAck,
    control_port: u16,
    topology: Topology,
    session_config: HostConfig,
    session: HostSession,
    bind_address: Option<SocketAddr>,
    active_viewer: Option<(HostConnectionId, SocketAddr)>,
    reconnecting_peer_key: Option<String>,
    pending: VecDeque<PendingHostAuthorization>,
    quality_controller: Option<QualityController>,
    quality_preference: QualityPreference,
    quality_rtt_baseline_us: Option<u64>,
    latest_viewer_feedback: Option<(u16, QualityFeedback)>,
    latest_sender_observation: Option<HostSenderObservation>,
    pending_quality_change: Option<(u64, QualityController, QualityPreference)>,
    pending_quality_adjustment: Option<(u64, QualityAdjustment)>,
    last_now_us: u64,
    status: HostRuntimeStatus,
    stopped: bool,
    #[cfg(feature = "loopback-test")]
    loopback_test_enabled: bool,
}

impl HostRuntime {
    /// Creates host state without opening sockets or touching the desktop.
    pub fn new(
        capabilities: HelloAck,
        topology: Topology,
        control_port: u16,
        session_config: HostConfig,
    ) -> Result<Self, HostRuntimeError> {
        if control_port == 0 {
            return Err(HostRuntimeError::InvalidControlPort);
        }
        let session =
            HostSession::with_config(capabilities.clone(), topology.clone(), session_config);
        Ok(Self {
            capabilities,
            control_port,
            topology,
            session_config,
            session,
            bind_address: None,
            active_viewer: None,
            reconnecting_peer_key: None,
            pending: VecDeque::new(),
            quality_controller: None,
            quality_preference: QualityPreference::Auto,
            quality_rtt_baseline_us: None,
            latest_viewer_feedback: None,
            latest_sender_observation: None,
            pending_quality_change: None,
            pending_quality_adjustment: None,
            last_now_us: 0,
            status: HostRuntimeStatus::default(),
            stopped: false,
            #[cfg(feature = "loopback-test")]
            loopback_test_enabled: false,
        })
    }

    /// Creates a host controller that may accept loopback addresses for an
    /// explicitly local test harness. Production callers must use `new`.
    #[cfg(feature = "loopback-test")]
    pub fn new_loopback_test(
        capabilities: HelloAck,
        topology: Topology,
        control_port: u16,
        session_config: HostConfig,
    ) -> Result<Self, HostRuntimeError> {
        let mut runtime = Self::new(capabilities, topology, control_port, session_config)?;
        runtime.loopback_test_enabled = true;
        Ok(runtime)
    }

    /// Returns the current host metadata snapshot.
    pub fn status(&self) -> &HostRuntimeStatus {
        &self.status
    }

    /// Returns the most recent local sender sample for the active stream epoch.
    pub fn latest_sender_observation(&self) -> Option<HostSenderObservation> {
        self.active_viewer?;
        self.latest_sender_observation
            .filter(|sample| sample.epoch == self.session.epoch())
    }

    /// Returns pending approvals in arrival order.
    pub fn pending_authorizations(&self) -> impl Iterator<Item = &PendingHostAuthorization> {
        self.pending.iter()
    }

    /// Validates a local Tailscale address and returns a listener change.
    ///
    /// Invalid, absent, wildcard, LAN, public, and loopback addresses all
    /// produce a None bind target. The host-agent remains responsible for
    /// reporting actual bind success or failure.
    pub fn update_tailscale_address(
        &mut self,
        address: Option<IpAddr>,
    ) -> Result<HostRuntimeEvent, HostRuntimeError> {
        self.ensure_running()?;
        let next = address
            .filter(|ip| is_tailscale_address(*ip))
            .map(|ip| SocketAddr::new(ip, self.control_port));
        self.bind_address = next;
        self.status.bind_address = next;
        self.refresh_status();
        Ok(HostRuntimeEvent::BindAddressChanged(next))
    }

    /// Sets an exact ephemeral loopback bind target for the opt-in test host.
    ///
    /// This method and its authorization path are absent from default builds.
    #[cfg(feature = "loopback-test")]
    pub fn update_test_loopback_address(
        &mut self,
        address: SocketAddr,
    ) -> Result<HostRuntimeEvent, HostRuntimeError> {
        self.ensure_running()?;
        if !self.loopback_test_enabled {
            return Err(HostRuntimeError::LoopbackTestDisabled);
        }
        if !address.ip().is_loopback() || address.port() == 0 {
            return Err(HostRuntimeError::InvalidLoopbackTestAddress);
        }
        self.bind_address = Some(address);
        self.status.bind_address = Some(address);
        self.refresh_status();
        Ok(HostRuntimeEvent::BindAddressChanged(Some(address)))
    }

    /// Supplies the external whois/allowlist result for a newly accepted Hello.
    ///
    /// Unknown peers are answered NotAuthorized immediately and queued for
    /// owner review. If approved later, they must reconnect before streaming.
    pub fn on_hello(
        &mut self,
        connection_id: HostConnectionId,
        remote_addr: SocketAddr,
        hello: &Hello,
        authorization: HostPeerAuthorization,
    ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
        self.ensure_running()?;
        self.status.counters.hello_received = self.status.counters.hello_received.saturating_add(1);
        if !self.peer_address_allowed(remote_addr.ip()) || self.bind_address.is_none() {
            self.status.counters.rejected_connections =
                self.status.counters.rejected_connections.saturating_add(1);
            return Ok(vec![
                self.handshake_event(connection_id, HelloStatus::NotAuthorized),
                HostRuntimeEvent::CloseConnection(connection_id),
            ]);
        }
        match authorization {
            HostPeerAuthorization::Approved { peer_key } if valid_peer_key(&peer_key) => {
                // Device discovery sends Hello with no video UDP endpoint. Answer the
                // capability probe without reserving the single viewer slot or starting
                // capture; otherwise a periodic probe can race a real viewer and make
                // the host report Busy for a session that never existed.
                if hello.video_udp_port == 0 {
                    let status = if hello.protocol_version != self.capabilities.protocol_version {
                        HelloStatus::UnsupportedVersion
                    } else if hello.codecs & self.capabilities.codecs & 1 == 0
                        || self.active_viewer.is_some()
                        || self.session.phase() == HostPhase::ControlDisconnected
                    {
                        HelloStatus::Busy
                    } else {
                        HelloStatus::Ok
                    };
                    return Ok(vec![
                        self.handshake_event(connection_id, status),
                        HostRuntimeEvent::CloseConnection(connection_id),
                    ]);
                }
                if self.active_viewer.is_some() {
                    self.status.counters.busy_connections =
                        self.status.counters.busy_connections.saturating_add(1);
                    return Ok(vec![
                        self.handshake_event(connection_id, HelloStatus::Busy),
                        HostRuntimeEvent::CloseConnection(connection_id),
                    ]);
                }
                if self.session.phase() == HostPhase::ControlDisconnected
                    && self.reconnecting_peer_key.as_deref() != Some(peer_key.as_str())
                {
                    self.status.counters.busy_connections =
                        self.status.counters.busy_connections.saturating_add(1);
                    return Ok(vec![
                        self.handshake_event(connection_id, HelloStatus::Busy),
                        HostRuntimeEvent::CloseConnection(connection_id),
                    ]);
                }
                let actions = self.session.on_hello(hello);
                let accepted = actions.iter().any(|action| {
                    matches!(
                        action,
                        HostAction::SendControl(ControlMessage::HelloAck(ack))
                            if ack.status == HelloStatus::Ok
                    )
                });
                if accepted {
                    self.active_viewer = Some((connection_id, remote_addr));
                    self.reconnecting_peer_key = Some(peer_key);
                    self.quality_rtt_baseline_us = None;
                    self.latest_viewer_feedback = None;
                    self.latest_sender_observation = None;
                }
                self.refresh_status();
                Ok(self.session_events(actions, Some(connection_id)))
            }
            HostPeerAuthorization::Pending { peer_key, label } if valid_peer_key(&peer_key) => {
                let pending_event = self.queue_pending(peer_key, label);
                self.status.counters.rejected_connections =
                    self.status.counters.rejected_connections.saturating_add(1);
                let mut events = vec![
                    self.handshake_event(connection_id, HelloStatus::NotAuthorized),
                    HostRuntimeEvent::CloseConnection(connection_id),
                ];
                events.extend(pending_event);
                Ok(events)
            }
            HostPeerAuthorization::Rejected
            | HostPeerAuthorization::Approved { .. }
            | HostPeerAuthorization::Pending { .. } => {
                self.status.counters.rejected_connections =
                    self.status.counters.rejected_connections.saturating_add(1);
                Ok(vec![
                    self.handshake_event(connection_id, HelloStatus::NotAuthorized),
                    HostRuntimeEvent::CloseConnection(connection_id),
                ])
            }
        }
    }

    /// Removes a pending peer after the identity adapter has persisted its
    /// approval or rejection. That peer must reconnect to use an approval.
    pub fn finish_pending_decision(
        &mut self,
        peer_key: &str,
    ) -> Result<PendingHostAuthorization, HostRuntimeError> {
        self.ensure_running()?;
        if !valid_peer_key(peer_key) {
            return Err(HostRuntimeError::InvalidPeerKey);
        }
        let index = self
            .pending
            .iter()
            .position(|entry| entry.peer_key == peer_key)
            .ok_or(HostRuntimeError::InvalidPeerKey)?;
        let entry = self
            .pending
            .remove(index)
            .ok_or(HostRuntimeError::InvalidPeerKey)?;
        self.refresh_status();
        Ok(entry)
    }

    /// Processes a control message only when both connection id and socket
    /// address match the authorized viewer.
    pub fn on_control(
        &mut self,
        connection_id: HostConnectionId,
        remote_addr: SocketAddr,
        message: ControlMessage,
        now_us: u64,
    ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
        self.ensure_running()?;
        self.last_now_us = self.last_now_us.max(now_us);
        if self.active_viewer != Some((connection_id, remote_addr)) {
            return Ok(vec![HostRuntimeEvent::CloseConnection(connection_id)]);
        }

        let is_goodbye = matches!(&message, ControlMessage::Goodbye(_));
        let actions = match message {
            ControlMessage::SwitchMonitor(request) => {
                self.cancel_pending_quality_change();
                self.session.on_switch_monitor(request)
            }
            ControlMessage::PauseVideo(PauseVideo) => self.session.on_pause(),
            ControlMessage::ResumeVideo(ResumeVideo) => self.session.on_resume(),
            ControlMessage::RequestKeyframe(request) => self.session.on_keyframe_request(request),
            ControlMessage::SetQuality(request) => {
                let Some(preference) = quality_preference(request.max_height) else {
                    return Ok(vec![HostRuntimeEvent::CloseConnection(connection_id)]);
                };
                self.quality_preference = preference;
                let mut events = vec![HostRuntimeEvent::QualityPreference(request)];
                if self.pending_quality_change.is_none() {
                    if let Some(controller) = self.quality_controller.as_mut() {
                        let previous = controller.clone();
                        let decisions = controller.set_preference(preference, now_us);
                        events.extend(self.apply_quality_decisions(decisions, previous));
                    }
                }
                return Ok(events);
            }
            ControlMessage::ViewerReport(report) => {
                let mut events = vec![HostRuntimeEvent::ViewerFeedback(report)];
                if report.epoch == self.session.epoch() {
                    let rtt_us = u64::from(report.rtt_ms).saturating_mul(1_000);
                    if rtt_us > 0 {
                        self.quality_rtt_baseline_us = Some(
                            self.quality_rtt_baseline_us
                                .map_or(rtt_us, |baseline| baseline.min(rtt_us)),
                        );
                    }
                    let encoder_lag_ms = self
                        .latest_sender_observation
                        .filter(|sample| sample.epoch == report.epoch)
                        .and_then(|sample| sample.encoder_lag_ms);
                    let feedback = QualityFeedback {
                        loss_bps: u32::from(report.loss_permille).saturating_mul(10),
                        frame_loss_bps: u32::from(report.frame_loss_permille).saturating_mul(10),
                        rtt_us,
                        rtt_baseline_us: self.quality_rtt_baseline_us.unwrap_or(0),
                        decode_ms_p95: (report.decode_ms_p95 != 0).then_some(report.decode_ms_p95),
                        dropped_frames: report.dropped_frames,
                        encoder_lag_ms,
                        queue_overflow: false,
                    };
                    self.latest_viewer_feedback = Some((report.epoch, feedback));
                    if self.pending_quality_change.is_none() {
                        events.extend(self.apply_quality_feedback(report.epoch, feedback, now_us));
                    }
                }
                return Ok(events);
            }
            ControlMessage::Ping(ping) => {
                return Ok(vec![HostRuntimeEvent::SendControl {
                    connection_id,
                    message: ControlMessage::Pong(Pong {
                        nonce: ping.nonce,
                        echo_ts_us: ping.sender_ts_us,
                    }),
                }]);
            }
            ControlMessage::Goodbye(goodbye) => {
                self.cancel_pending_quality_change();
                self.session.on_goodbye(goodbye)
            }
            ControlMessage::Hello(_)
            | ControlMessage::HelloAck(_)
            | ControlMessage::TopologyAnnounce(_)
            | ControlMessage::StreamReset(_)
            | ControlMessage::InputEvent(_)
            | ControlMessage::ClipboardUpdate(_)
            | ControlMessage::ClipboardSyncControl(_)
            | ControlMessage::StatsReport(_)
            | ControlMessage::Pong(_)
            | ControlMessage::CursorShape(_)
            | ControlMessage::QualityAdjustment(_) => {
                return Ok(vec![HostRuntimeEvent::CloseConnection(connection_id)]);
            }
        };
        if is_goodbye {
            self.active_viewer = None;
            self.reconnecting_peer_key = None;
        }
        Ok(self.session_events(actions, Some(connection_id)))
    }

    /// Applies one bounded local sender sample to the active epoch's quality controller.
    ///
    /// `queue_overflow` is a pulse for one overflow since the prior call. Encoder lag is
    /// retained as an observation but does not have an inferred adaptation threshold.
    pub fn on_local_sender_observation(
        &mut self,
        observation: HostSenderObservation,
        now_us: u64,
    ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
        self.ensure_running()?;
        if observation
            .encoder_lag_ms
            .is_some_and(|lag| lag > MAX_HOST_ENCODER_LAG_MS)
        {
            return Err(HostRuntimeError::InvalidQualityObservation);
        }
        self.last_now_us = self.last_now_us.max(now_us);
        if self.active_viewer.is_none() || observation.epoch != self.session.epoch() {
            return Ok(Vec::new());
        }
        self.latest_sender_observation = Some(observation);
        if self.pending_quality_change.is_some() {
            return Ok(Vec::new());
        }

        let mut feedback = self
            .latest_viewer_feedback
            .filter(|(epoch, _)| *epoch == observation.epoch)
            .map_or_else(QualityFeedback::default, |(_, feedback)| feedback);
        feedback.encoder_lag_ms = observation.encoder_lag_ms;
        feedback.queue_overflow = observation.queue_overflow;
        Ok(self.apply_quality_feedback(observation.epoch, feedback, now_us))
    }

    /// Starts HostSession's reconnect grace period after a control disconnect.
    pub fn on_disconnect(
        &mut self,
        connection_id: HostConnectionId,
        now_us: u64,
    ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
        self.ensure_running()?;
        self.last_now_us = self.last_now_us.max(now_us);
        if self
            .active_viewer
            .is_none_or(|(active_id, _)| active_id != connection_id)
        {
            return Ok(Vec::new());
        }
        self.active_viewer = None;
        let actions = self.session.on_control_disconnected(now_us);
        Ok(self.session_events(actions, None))
    }

    /// Advances session recovery and disconnect timers.
    pub fn tick(&mut self, now_us: u64) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
        self.ensure_running()?;
        self.last_now_us = self.last_now_us.max(now_us);
        let actions = self.session.tick(now_us);
        let events = self.session_events(
            actions,
            self.active_viewer.map(|(connection_id, _)| connection_id),
        );
        if self.session.phase() == HostPhase::Closed {
            self.session = HostSession::with_config(
                self.capabilities.clone(),
                self.topology.clone(),
                self.session_config,
            );
            self.active_viewer = None;
            self.reconnecting_peer_key = None;
            self.status.quality_tier = None;
            self.quality_controller = None;
            self.pending_quality_change = None;
            self.pending_quality_adjustment = None;
            self.quality_rtt_baseline_us = None;
            self.latest_viewer_feedback = None;
            self.latest_sender_observation = None;
        }
        self.refresh_status();
        Ok(events)
    }

    /// Feeds a capture operation result back into HostSession.
    pub fn on_capture_result(
        &mut self,
        operation_id: u64,
        result: Result<(), CaptureFailure>,
        now_us: u64,
    ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
        self.ensure_running()?;
        self.last_now_us = self.last_now_us.max(now_us);
        let actions = self.session.on_capture_result(operation_id, result, now_us);
        Ok(self.session_events(
            actions,
            self.active_viewer.map(|(connection_id, _)| connection_id),
        ))
    }

    /// Reports capture loss and returns the session recovery actions.
    pub fn on_capture_lost(
        &mut self,
        reason: RecoveryReason,
        now_us: u64,
    ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
        self.ensure_running()?;
        self.last_now_us = self.last_now_us.max(now_us);
        let actions = self.session.on_capture_lost(reason, now_us);
        Ok(self.session_events(
            actions,
            self.active_viewer.map(|(connection_id, _)| connection_id),
        ))
    }

    /// Feeds an encoder configuration result back into HostSession.
    pub fn on_encoder_configured(
        &mut self,
        operation_id: u64,
        result: Result<(), EncoderFailure>,
        now_us: u64,
    ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
        self.ensure_running()?;
        self.last_now_us = self.last_now_us.max(now_us);
        let is_quality_operation = self
            .pending_quality_change
            .as_ref()
            .is_some_and(|(pending_id, _, _)| *pending_id == operation_id);
        let actions = self
            .session
            .on_encoder_configured(operation_id, result, now_us);
        let mut events = self.session_events(
            actions,
            self.active_viewer.map(|(connection_id, _)| connection_id),
        );
        let mut quality_adjustment = None;
        if is_quality_operation {
            if result.is_ok() {
                if let Some(controller) = self.quality_controller.as_mut() {
                    controller.reset_feedback_window(now_us);
                }
                quality_adjustment = self
                    .pending_quality_adjustment
                    .take()
                    .filter(|(pending_id, _)| *pending_id == operation_id)
                    .map(|(_, adjustment)| adjustment);
            } else {
                self.pending_quality_adjustment = None;
                if let Some((_, previous, applied_preference)) = self.pending_quality_change.take()
                {
                    if self.quality_preference == applied_preference {
                        self.quality_preference = previous.preference();
                    }
                    if let Some(controller) = self.quality_controller.as_mut() {
                        let current_bitrate = controller.current_bitrate_bps();
                        let previous_bitrate = previous.current_bitrate_bps();
                        *controller = previous;
                        if current_bitrate != previous_bitrate {
                            events.push(HostRuntimeEvent::SessionAction {
                                connection_id: self.active_viewer.map(|(id, _)| id),
                                action: HostAction::Encoder(EncoderAction::SetBitrate(
                                    previous_bitrate,
                                )),
                            });
                        }
                    }
                }
            }
            self.pending_quality_change = None;
            if let Some(controller) = self.quality_controller.as_mut() {
                if controller.preference() != self.quality_preference {
                    let previous = controller.clone();
                    let decisions = controller.set_preference(self.quality_preference, now_us);
                    events.extend(self.apply_quality_decisions(decisions, previous));
                }
            }
        }
        if let (Some(mut adjustment), Some((connection_id, _))) =
            (quality_adjustment, self.active_viewer)
        {
            adjustment.epoch = self.session.epoch();
            events.push(HostRuntimeEvent::SendControl {
                connection_id,
                message: ControlMessage::QualityAdjustment(adjustment),
            });
        }
        Ok(events)
    }

    /// Reports an active encoder failure and starts the host session's bounded rebuild path.
    pub fn on_encoder_failure(&mut self) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
        self.ensure_running()?;
        let actions = self.session.on_encoder_failure();
        Ok(self.session_events(
            actions,
            self.active_viewer.map(|(connection_id, _)| connection_id),
        ))
    }
    /// Feeds an encoder rebuild result back into HostSession.
    pub fn on_encoder_rebuild_result(
        &mut self,
        operation_id: u64,
        success: bool,
        now_us: u64,
    ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
        self.ensure_running()?;
        self.last_now_us = self.last_now_us.max(now_us);
        let actions = self
            .session
            .on_encoder_rebuild_result(operation_id, success);
        Ok(self.session_events(
            actions,
            self.active_viewer.map(|(connection_id, _)| connection_id),
        ))
    }

    /// Applies a topology update and returns any required session actions.
    pub fn on_topology_changed(
        &mut self,
        topology: Topology,
        now_us: u64,
    ) -> Result<Vec<HostRuntimeEvent>, HostRuntimeError> {
        self.ensure_running()?;
        self.last_now_us = self.last_now_us.max(now_us);
        self.cancel_pending_quality_change();
        self.topology = topology.clone();
        let actions = self.session.on_topology_update(topology, now_us);
        Ok(self.session_events(
            actions,
            self.active_viewer.map(|(connection_id, _)| connection_id),
        ))
    }

    /// Updates the short encoder label shown by status.
    pub fn set_encoder_name(&mut self, name: Option<String>) {
        self.status.encoder_name = name.map(|value| bounded_label(&value));
    }

    /// Stops this controller and emits adapter shutdown work.
    pub fn stop(&mut self) -> Vec<HostRuntimeEvent> {
        if self.stopped {
            return Vec::new();
        }
        let actions = self.session.on_goodbye(Goodbye {
            reason: GoodbyeReason::Shutdown,
        });
        self.stopped = true;
        self.active_viewer = None;
        self.reconnecting_peer_key = None;
        self.bind_address = None;
        self.quality_controller = None;
        self.pending_quality_change = None;
        self.pending_quality_adjustment = None;
        self.quality_rtt_baseline_us = None;
        self.latest_viewer_feedback = None;
        self.latest_sender_observation = None;
        self.status.phase = HostRuntimePhase::Stopped;
        self.status.bind_address = None;
        self.status.viewer_connected = false;
        self.status.session_phase = self.session.phase();
        self.status.current_display = self.session.selected_display();
        let mut events = self.session_events(actions, None);
        events.push(HostRuntimeEvent::BindAddressChanged(None));
        events.push(HostRuntimeEvent::Stopped);
        events
    }

    fn handshake_event(
        &self,
        connection_id: HostConnectionId,
        status: HelloStatus,
    ) -> HostRuntimeEvent {
        HostRuntimeEvent::SendControl {
            connection_id,
            message: ControlMessage::HelloAck(HelloAck {
                status,
                ..self.capabilities.clone()
            }),
        }
    }

    fn queue_pending(&mut self, peer_key: String, label: String) -> Vec<HostRuntimeEvent> {
        if self.pending.iter().any(|entry| entry.peer_key == peer_key) {
            return Vec::new();
        }
        if self.pending.len() >= MAX_PENDING_HOST_AUTHORIZATIONS {
            self.status.counters.pending_queue_overflow = self
                .status
                .counters
                .pending_queue_overflow
                .saturating_add(1);
            return Vec::new();
        }
        let entry = PendingHostAuthorization {
            peer_key,
            label: bounded_label(&label),
        };
        self.pending.push_back(entry.clone());
        self.status.pending_authorizations = self.pending.len();
        vec![HostRuntimeEvent::PendingAuthorization(entry)]
    }

    fn session_events(
        &mut self,
        actions: Vec<HostAction>,
        connection_id: Option<HostConnectionId>,
    ) -> Vec<HostRuntimeEvent> {
        let mut events = Vec::new();
        for action in actions {
            let successful_reset = match &action {
                HostAction::SendControl(ControlMessage::StreamReset(reset))
                    if reset.status == racc_proto::StreamStatus::Ok && reset.height > 0 =>
                {
                    self.status.quality_tier = Some(host_quality_tier(reset.height));
                    Some(*reset)
                }
                _ => None,
            };
            events.push(HostRuntimeEvent::SessionAction {
                connection_id,
                action,
            });
            if let Some(reset) = successful_reset {
                if self.pending_quality_change.is_none() {
                    events.extend(self.refresh_quality_controller(reset));
                }
            }
        }
        self.refresh_status();
        events
    }

    fn peer_address_allowed(&self, address: IpAddr) -> bool {
        #[cfg(feature = "loopback-test")]
        {
            match self.bind_address.map(|bind_address| bind_address.ip()) {
                Some(bind_ip) if bind_ip.is_loopback() => {
                    self.loopback_test_enabled && address.is_loopback()
                }
                Some(bind_ip) if is_tailscale_address(bind_ip) => is_tailscale_address(address),
                _ => false,
            }
        }
        #[cfg(not(feature = "loopback-test"))]
        {
            self.bind_address
                .is_some_and(|bind_address| is_tailscale_address(bind_address.ip()))
                && is_tailscale_address(address)
        }
    }

    fn refresh_quality_controller(
        &mut self,
        reset: racc_proto::StreamReset,
    ) -> Vec<HostRuntimeEvent> {
        let display_height = self
            .topology
            .displays()
            .iter()
            .find(|display| display.id().get() == reset.display_id)
            .and_then(|display| {
                let (width, height) = display.size();
                racc_session::bounded_stream_dimensions(width, height)
                    .map(|dimensions| dimensions.height)
            })
            .unwrap_or(reset.height)
            .max(reset.height);
        let initial_tier = quality_tier(reset.height);
        let Ok(mut controller) = QualityController::new(
            self.capabilities.max_height,
            display_height,
            initial_tier,
            QualityPreference::Auto,
        ) else {
            self.quality_controller = None;
            return Vec::new();
        };
        let previous = controller.clone();
        let decisions = controller.set_preference(self.quality_preference, self.last_now_us);
        self.quality_controller = Some(controller);
        self.apply_quality_decisions(decisions, previous)
    }

    fn apply_quality_feedback(
        &mut self,
        epoch: u16,
        feedback: QualityFeedback,
        now_us: u64,
    ) -> Vec<HostRuntimeEvent> {
        if epoch != self.session.epoch() || self.pending_quality_change.is_some() {
            return Vec::new();
        }
        let Some(controller) = self.quality_controller.as_mut() else {
            return Vec::new();
        };
        let previous = controller.clone();
        let decisions = controller.update(now_us, feedback);
        self.apply_quality_decisions(decisions, previous)
    }

    fn apply_quality_decisions(
        &mut self,
        decisions: Vec<QualityEvent>,
        previous: QualityController,
    ) -> Vec<HostRuntimeEvent> {
        let mut events = Vec::new();
        for decision in decisions {
            events.push(HostRuntimeEvent::QualityDecision(decision));
            match decision {
                QualityEvent::BitrateTrim {
                    from_bps, to_bps, ..
                } => {
                    events.push(HostRuntimeEvent::SessionAction {
                        connection_id: self.active_viewer.map(|(id, _)| id),
                        action: HostAction::Encoder(EncoderAction::SetBitrate(to_bps)),
                    });
                    if let (Some((connection_id, _)), Some(controller)) =
                        (self.active_viewer, self.quality_controller.as_ref())
                    {
                        let tier = controller.current_tier();
                        events.push(HostRuntimeEvent::SendControl {
                            connection_id,
                            message: ControlMessage::QualityAdjustment(QualityAdjustment {
                                epoch: self.session.epoch(),
                                reason: QualityAdjustmentReason::BitrateTrim,
                                from_height: tier.height(),
                                to_height: tier.height(),
                                from_bitrate_bps: from_bps,
                                to_bitrate_bps: to_bps,
                            }),
                        });
                    }
                }
                QualityEvent::TierChanged {
                    from,
                    to,
                    reason,
                    target_bitrate_bps,
                } => {
                    events.push(HostRuntimeEvent::SessionAction {
                        connection_id: self.active_viewer.map(|(id, _)| id),
                        action: HostAction::Encoder(EncoderAction::SetBitrate(target_bitrate_bps)),
                    });
                    let actions = self.session.on_quality_tier_changed(to);
                    let operation_id = actions.iter().find_map(|action| match action {
                        HostAction::Encoder(EncoderAction::Configure { operation_id, .. }) => {
                            Some(*operation_id)
                        }
                        _ => None,
                    });
                    if let Some(operation_id) = operation_id {
                        let applied_preference = self
                            .quality_controller
                            .as_ref()
                            .map(QualityController::preference)
                            .unwrap_or_else(|| previous.preference());
                        self.pending_quality_change =
                            Some((operation_id, previous.clone(), applied_preference));
                        self.pending_quality_adjustment = Some((
                            operation_id,
                            QualityAdjustment {
                                epoch: 0,
                                reason: wire_quality_reason(reason),
                                from_height: from.height(),
                                to_height: to.height(),
                                from_bitrate_bps: previous.current_bitrate_bps(),
                                to_bitrate_bps: target_bitrate_bps,
                            },
                        ));
                    } else if let Some(controller) = self.quality_controller.as_mut() {
                        self.pending_quality_adjustment = None;
                        let current_bitrate = controller.current_bitrate_bps();
                        let previous_bitrate = previous.current_bitrate_bps();
                        *controller = previous.clone();
                        if current_bitrate != previous_bitrate {
                            events.push(HostRuntimeEvent::SessionAction {
                                connection_id: self.active_viewer.map(|(id, _)| id),
                                action: HostAction::Encoder(EncoderAction::SetBitrate(
                                    previous_bitrate,
                                )),
                            });
                        }
                    }
                    events
                        .extend(self.session_events(actions, self.active_viewer.map(|(id, _)| id)));
                }
            }
        }
        events
    }

    fn cancel_pending_quality_change(&mut self) {
        self.pending_quality_adjustment = None;
        if let Some((_, previous, _)) = self.pending_quality_change.take() {
            self.quality_controller = Some(previous);
        }
    }

    fn refresh_status(&mut self) {
        self.status.viewer_connected = self.active_viewer.is_some();
        self.status.session_phase = self.session.phase();
        self.status.current_display = self.session.selected_display();
        self.status.pending_authorizations = self.pending.len();
        if self.stopped {
            self.status.phase = HostRuntimePhase::Stopped;
        } else if self.active_viewer.is_some() {
            self.status.phase = HostRuntimePhase::ViewerConnected;
        } else if self.bind_address.is_some() {
            self.status.phase = HostRuntimePhase::BindReady;
        } else {
            self.status.phase = HostRuntimePhase::WaitingForTailscale;
        }
    }

    fn ensure_running(&self) -> Result<(), HostRuntimeError> {
        if self.stopped {
            Err(HostRuntimeError::Stopped)
        } else {
            Ok(())
        }
    }
}

fn wire_quality_reason(reason: racc_session::QualityChangeReason) -> QualityAdjustmentReason {
    match reason {
        racc_session::QualityChangeReason::Loss => QualityAdjustmentReason::Loss,
        racc_session::QualityChangeReason::RttInflation => QualityAdjustmentReason::RttInflation,
        racc_session::QualityChangeReason::QueueOverflow => QualityAdjustmentReason::QueueOverflow,
        racc_session::QualityChangeReason::Stable => QualityAdjustmentReason::Stable,
        racc_session::QualityChangeReason::Preference => QualityAdjustmentReason::Preference,
    }
}

fn quality_preference(max_height: u16) -> Option<QualityPreference> {
    match max_height {
        0 => Some(QualityPreference::Auto),
        480 => Some(QualityPreference::Fixed(QualityTier::P480)),
        720 => Some(QualityPreference::Fixed(QualityTier::P720)),
        1080 => Some(QualityPreference::Fixed(QualityTier::P1080)),
        _ => None,
    }
}

fn quality_tier(height: u16) -> QualityTier {
    if height >= 1080 {
        QualityTier::P1080
    } else if height >= 720 {
        QualityTier::P720
    } else {
        QualityTier::P480
    }
}

fn host_quality_tier(height: u16) -> HostQualityTier {
    match quality_tier(height) {
        QualityTier::P480 => HostQualityTier::P480,
        QualityTier::P720 => HostQualityTier::P720,
        QualityTier::P1080 => HostQualityTier::P1080,
    }
}

/// Checks Tailscale's IPv4 CGNAT and IPv6 ULA prefixes.
pub fn is_tailscale_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let address = u32::from(address);
            let network = u32::from(Ipv4Addr::new(100, 64, 0, 0));
            let mask = u32::MAX << 22;
            address & mask == network & mask
        }
        IpAddr::V6(address) => address.octets()[..6] == [0xfd, 0x7a, 0x11, 0x5c, 0xa1, 0xe0],
    }
}

fn valid_peer_key(peer_key: &str) -> bool {
    !peer_key.trim().is_empty() && peer_key.len() <= MAX_HOST_PEER_KEY_BYTES
}

fn bounded_label(label: &str) -> String {
    let label = label.trim();
    if label.is_empty() {
        return "Unknown device".to_owned();
    }
    let mut end = label.len().min(MAX_HOST_PEER_LABEL_BYTES);
    while !label.is_char_boundary(end) {
        end -= 1;
    }
    label[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_proto::OsType;
    use racc_topology::{Display, DisplayFlags};

    fn topology() -> Topology {
        Topology::new(
            1,
            vec![Display::new(
                DisplayId::new(1).unwrap_or_else(|| unreachable!()),
                "Main",
                0,
                0,
                1280,
                720,
                1000,
                60_000,
                DisplayFlags::new(true, true, true, false),
            )],
            None,
        )
        .unwrap_or_else(|error| panic!("valid topology: {error}"))
    }

    fn capabilities() -> HelloAck {
        HelloAck {
            protocol_version: 1,
            status: HelloStatus::Ok,
            device_name: "test-host".to_owned(),
            os: OsType::Windows,
            app_version: "test".to_owned(),
            codecs: 1,
            max_height: 1080,
            features: 0,
            host_cpu_cores: 8,
        }
    }

    fn hello() -> Hello {
        Hello {
            protocol_version: 1,
            device_name: "test-viewer".to_owned(),
            os: OsType::Windows,
            app_version: "test".to_owned(),
            video_udp_port: 47_474,
            codecs: 1,
            max_height: 720,
            features: 0,
        }
    }

    fn address(value: &str) -> IpAddr {
        value.parse().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    fn socket(value: &str, port: u16) -> SocketAddr {
        SocketAddr::new(address(value), port)
    }

    fn runtime() -> HostRuntime {
        HostRuntime::new(capabilities(), topology(), 47_473, HostConfig::default())
            .unwrap_or_else(|error| panic!("runtime: {error}"))
    }

    fn approved() -> HostPeerAuthorization {
        HostPeerAuthorization::Approved {
            peer_key: "node-one".to_owned(),
        }
    }

    fn start_stream(runtime: &mut HostRuntime, peer: SocketAddr) {
        runtime
            .update_tailscale_address(Some(address("100.100.10.1")))
            .unwrap_or_else(|error| panic!("address update: {error}"));
        let hello = runtime
            .on_hello(1, peer, &hello(), approved())
            .unwrap_or_else(|error| panic!("hello: {error}"));
        let capture_id = hello
            .iter()
            .find_map(|event| match event {
                HostRuntimeEvent::SessionAction {
                    action:
                        HostAction::Capture(racc_session::CaptureAction::SwitchDisplay {
                            operation_id,
                            ..
                        }),
                    ..
                } => Some(*operation_id),
                _ => None,
            })
            .expect("initial capture action");
        let configure = runtime
            .on_capture_result(capture_id, Ok(()), 10)
            .unwrap_or_else(|error| panic!("capture result: {error}"));
        let encoder_id = configure
            .iter()
            .find_map(|event| match event {
                HostRuntimeEvent::SessionAction {
                    action: HostAction::Encoder(EncoderAction::Configure { operation_id, .. }),
                    ..
                } => Some(*operation_id),
                _ => None,
            })
            .expect("initial encoder configure action");
        runtime
            .on_encoder_configured(encoder_id, Ok(()), 20)
            .unwrap_or_else(|error| panic!("encoder result: {error}"));
    }

    fn viewer_report(
        epoch: u16,
        loss_permille: u16,
        frame_loss_permille: u16,
        rtt_ms: u32,
    ) -> ViewerReport {
        ViewerReport {
            epoch,
            loss_permille,
            frame_loss_permille,
            rtt_ms,
            decode_ms_p95: 8,
            dropped_frames: 1,
        }
    }

    #[test]
    fn only_tailnet_addresses_become_bind_targets() {
        let mut runtime = runtime();
        assert_eq!(
            runtime
                .update_tailscale_address(None)
                .unwrap_or_else(|error| panic!("address update: {error}")),
            HostRuntimeEvent::BindAddressChanged(None)
        );
        for text in [
            "127.0.0.1",
            "192.168.1.4",
            "8.8.8.8",
            "100.63.255.255",
            "100.128.0.1",
        ] {
            runtime
                .update_tailscale_address(Some(address(text)))
                .unwrap_or_else(|error| panic!("address update: {error}"));
            assert_eq!(
                runtime.status().phase,
                HostRuntimePhase::WaitingForTailscale
            );
            assert_eq!(runtime.status().bind_address, None);
        }
        runtime
            .update_tailscale_address(Some(address("100.100.10.1")))
            .unwrap_or_else(|error| panic!("address update: {error}"));
        assert_eq!(
            runtime.status().bind_address,
            Some(socket("100.100.10.1", 47_473))
        );
        assert_eq!(runtime.status().phase, HostRuntimePhase::BindReady);
        assert!(is_tailscale_address(address("fd7a:115c:a1e0::42")));
        assert!(!is_tailscale_address(address("fd7a:115c:a1e1::42")));
    }

    #[cfg(feature = "loopback-test")]
    #[test]
    fn default_constructor_does_not_enable_loopback_even_when_feature_is_compiled() {
        let mut runtime = runtime();
        assert_eq!(
            runtime.update_test_loopback_address(socket("127.0.0.1", 47_473)),
            Err(HostRuntimeError::LoopbackTestDisabled)
        );
        let events = runtime
            .on_hello(1, socket("127.0.0.1", 50_000), &hello(), approved())
            .unwrap_or_else(|error| panic!("loopback hello: {error}"));
        assert!(events.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SendControl {
                message: ControlMessage::HelloAck(ack),
                ..
            } if ack.status == HelloStatus::NotAuthorized
        )));
    }

    #[cfg(feature = "loopback-test")]
    #[test]
    fn explicit_test_constructor_accepts_only_a_loopback_test_bind() {
        let mut runtime = HostRuntime::new_loopback_test(
            capabilities(),
            topology(),
            47_473,
            HostConfig::default(),
        )
        .unwrap_or_else(|error| panic!("test runtime: {error}"));
        assert_eq!(
            runtime.update_test_loopback_address(socket("127.0.0.1", 0)),
            Err(HostRuntimeError::InvalidLoopbackTestAddress)
        );
        runtime
            .update_test_loopback_address(socket("127.0.0.1", 47_473))
            .unwrap_or_else(|error| panic!("loopback bind: {error}"));
        let events = runtime
            .on_hello(1, socket("127.0.0.1", 50_000), &hello(), approved())
            .unwrap_or_else(|error| panic!("loopback hello: {error}"));
        assert!(events.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SessionAction {
                action: HostAction::SendControl(ControlMessage::HelloAck(ack)),
                ..
            } if ack.status == HelloStatus::Ok
        )));
        assert!(runtime.status().viewer_connected);

        let mut local_runtime = HostRuntime::new_loopback_test(
            capabilities(),
            topology(),
            47_473,
            HostConfig::default(),
        )
        .unwrap_or_else(|error| panic!("test runtime: {error}"));
        local_runtime
            .update_test_loopback_address(socket("127.0.0.1", 47_473))
            .unwrap_or_else(|error| panic!("loopback bind: {error}"));
        let tailnet_peer = local_runtime
            .on_hello(2, socket("100.100.10.2", 50_000), &hello(), approved())
            .unwrap_or_else(|error| panic!("tailnet hello: {error}"));
        assert!(tailnet_peer.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SendControl {
                message: ControlMessage::HelloAck(ack),
                ..
            } if ack.status == HelloStatus::NotAuthorized
        )));
    }

    #[test]
    fn pending_identity_is_denied_and_queue_is_bounded() {
        let mut runtime = runtime();
        runtime
            .update_tailscale_address(Some(address("100.100.10.1")))
            .unwrap_or_else(|error| panic!("address update: {error}"));
        let remote = socket("100.100.10.2", 50_000);
        let events = runtime
            .on_hello(
                3,
                remote,
                &hello(),
                HostPeerAuthorization::Pending {
                    peer_key: "node-pending".to_owned(),
                    label: "New PC".to_owned(),
                },
            )
            .unwrap_or_else(|error| panic!("hello: {error}"));
        assert!(events.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SendControl {
                message: ControlMessage::HelloAck(ack),
                ..
            } if ack.status == HelloStatus::NotAuthorized
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::PendingAuthorization(peer)
                if peer.peer_key == "node-pending" && peer.label == "New PC"
        )));
        assert!(!runtime.status().viewer_connected);

        let removed = runtime
            .finish_pending_decision("node-pending")
            .unwrap_or_else(|error| panic!("finish decision: {error}"));
        assert_eq!(removed.label, "New PC");
        assert_eq!(runtime.status().pending_authorizations, 0);

        for index in 0..MAX_PENDING_HOST_AUTHORIZATIONS + 1 {
            let _ = runtime.on_hello(
                10 + index as u64,
                remote,
                &hello(),
                HostPeerAuthorization::Pending {
                    peer_key: format!("node-{index}"),
                    label: "pending".to_owned(),
                },
            );
        }
        assert_eq!(
            runtime.status().pending_authorizations,
            MAX_PENDING_HOST_AUTHORIZATIONS
        );
        assert_eq!(runtime.status().counters.pending_queue_overflow, 1);
    }

    #[test]
    fn authorized_viewer_gets_session_actions_and_second_viewer_gets_busy() {
        let mut runtime = runtime();
        runtime
            .update_tailscale_address(Some(address("100.100.10.1")))
            .unwrap_or_else(|error| panic!("address update: {error}"));
        let first_addr = socket("100.100.10.2", 50_001);
        let first = runtime
            .on_hello(1, first_addr, &hello(), approved())
            .unwrap_or_else(|error| panic!("first hello: {error}"));
        assert!(runtime.status().viewer_connected);
        assert!(first.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SessionAction {
                action: HostAction::Capture(_),
                ..
            }
        )));

        let second = runtime
            .on_hello(
                2,
                socket("100.100.10.3", 50_002),
                &hello(),
                HostPeerAuthorization::Approved {
                    peer_key: "node-two".to_owned(),
                },
            )
            .unwrap_or_else(|error| panic!("second hello: {error}"));
        assert!(second.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SendControl {
                message: ControlMessage::HelloAck(ack),
                ..
            } if ack.status == HelloStatus::Busy
        )));
        assert_eq!(runtime.status().counters.busy_connections, 1);

        let pause = runtime
            .on_control(1, first_addr, ControlMessage::PauseVideo(PauseVideo), 1_000)
            .unwrap_or_else(|error| panic!("pause: {error}"));
        assert!(pause.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SessionAction {
                action: HostAction::Encoder(racc_session::EncoderAction::SetPaused(true)),
                ..
            }
        )));
    }

    #[test]
    fn capability_probe_does_not_reserve_the_viewer_slot_or_start_capture() {
        let mut runtime = runtime();
        runtime
            .update_tailscale_address(Some(address("100.100.10.1")))
            .unwrap_or_else(|error| panic!("address update: {error}"));
        let probe_peer = socket("100.100.10.2", 50_001);
        let mut probe = hello();
        probe.video_udp_port = 0;

        let probe_events = runtime
            .on_hello(1, probe_peer, &probe, approved())
            .unwrap_or_else(|error| panic!("capability probe: {error}"));

        assert!(probe_events.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SendControl {
                message: ControlMessage::HelloAck(HelloAck {
                    status: HelloStatus::Ok,
                    ..
                }),
                ..
            }
        )));
        assert!(probe_events
            .iter()
            .any(|event| matches!(event, HostRuntimeEvent::CloseConnection(1))));
        assert!(!probe_events.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SessionAction {
                action: HostAction::Capture(_),
                ..
            }
        )));
        assert!(!runtime.status().viewer_connected);

        let viewer_events = runtime
            .on_hello(2, probe_peer, &hello(), approved())
            .unwrap_or_else(|error| panic!("viewer hello after probe: {error}"));
        assert!(viewer_events.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SessionAction {
                action: HostAction::Capture(_),
                ..
            }
        )));
        assert!(runtime.status().viewer_connected);
    }

    #[test]
    fn capability_probe_while_streaming_does_not_displace_the_active_viewer() {
        let mut runtime = runtime();
        let active_peer = socket("100.100.10.2", 50_001);
        start_stream(&mut runtime, active_peer);
        let probe_peer = socket("100.100.10.3", 50_002);
        let mut probe = hello();
        probe.video_udp_port = 0;

        let probe_events = runtime
            .on_hello(2, probe_peer, &probe, approved())
            .unwrap_or_else(|error| panic!("capability probe during stream: {error}"));

        assert!(probe_events.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SendControl {
                message: ControlMessage::HelloAck(HelloAck {
                    status: HelloStatus::Busy,
                    ..
                }),
                ..
            }
        )));
        assert!(probe_events
            .iter()
            .any(|event| matches!(event, HostRuntimeEvent::CloseConnection(2))));
        assert!(runtime.status().viewer_connected);
        assert_eq!(runtime.status().counters.busy_connections, 0);
    }

    #[test]
    fn reconnect_grace_is_limited_to_the_original_approved_peer() {
        let mut runtime = runtime();
        let original_peer = socket("100.100.10.2", 50_001);
        start_stream(&mut runtime, original_peer);
        runtime
            .on_disconnect(1, 100_000)
            .unwrap_or_else(|error| panic!("original disconnect: {error}"));
        assert_eq!(
            runtime.status().session_phase,
            HostPhase::ControlDisconnected
        );

        let other_peer = socket("100.100.10.3", 50_002);
        let other = runtime
            .on_hello(
                2,
                other_peer,
                &hello(),
                HostPeerAuthorization::Approved {
                    peer_key: "node-two".to_owned(),
                },
            )
            .unwrap_or_else(|error| panic!("other peer hello: {error}"));
        assert!(other.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SendControl {
                message: ControlMessage::HelloAck(HelloAck {
                    status: HelloStatus::Busy,
                    ..
                }),
                ..
            }
        )));
        assert!(!runtime.status().viewer_connected);

        let original_reconnect = runtime
            .on_hello(3, socket("100.100.10.2", 50_003), &hello(), approved())
            .unwrap_or_else(|error| panic!("original peer reconnect: {error}"));
        assert!(original_reconnect.iter().any(|event| {
            matches!(
                event,
                HostRuntimeEvent::SendControl {
                    message: ControlMessage::HelloAck(HelloAck {
                        status: HelloStatus::Ok,
                        ..
                    }),
                    ..
                }
            ) || matches!(
                event,
                HostRuntimeEvent::SessionAction {
                    action: HostAction::SendControl(ControlMessage::HelloAck(HelloAck {
                        status: HelloStatus::Ok,
                        ..
                    })),
                    ..
                }
            )
        }));
        assert!(runtime.status().viewer_connected);
    }

    #[test]
    fn sustained_viewer_loss_changes_bitrate_then_reconfigures_and_resets_stream() {
        let mut runtime = runtime();
        let peer = socket("100.100.10.2", 50_001);
        start_stream(&mut runtime, peer);
        assert_eq!(runtime.status().quality_tier, Some(HostQualityTier::P720));
        let epoch = runtime.session.epoch();

        let baseline = runtime
            .on_control(
                1,
                peer,
                ControlMessage::ViewerReport(viewer_report(epoch, 0, 0, 20)),
                1_000,
            )
            .unwrap_or_else(|error| panic!("baseline report: {error}"));
        assert!(
            baseline.contains(&HostRuntimeEvent::ViewerFeedback(viewer_report(
                epoch, 0, 0, 20
            )))
        );

        for now_us in [1_000_000, 2_000_000, 4_000_000] {
            let events = runtime
                .on_control(
                    1,
                    peer,
                    ControlMessage::ViewerReport(viewer_report(epoch, 30, 0, 20)),
                    now_us,
                )
                .unwrap_or_else(|error| panic!("loss report: {error}"));
            if now_us == 2_000_000 {
                assert!(events.iter().any(|event| matches!(
                    event,
                    HostRuntimeEvent::QualityDecision(QualityEvent::BitrateTrim {
                        to_bps: 3_150_000,
                        percent: 90,
                        ..
                    })
                )));
                assert!(events.iter().any(|event| matches!(
                    event,
                    HostRuntimeEvent::SessionAction {
                        action: HostAction::Encoder(EncoderAction::SetBitrate(3_150_000)),
                        ..
                    }
                )));
            }
        }

        let downshift = runtime
            .on_control(
                1,
                peer,
                ControlMessage::ViewerReport(viewer_report(epoch, 30, 0, 20)),
                4_100_000,
            )
            .unwrap_or_else(|error| panic!("downshift report: {error}"));
        assert!(downshift.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::QualityDecision(QualityEvent::TierChanged {
                to: QualityTier::P480,
                reason: racc_session::QualityChangeReason::Loss,
                ..
            })
        )));
        let operation_id = downshift
            .iter()
            .find_map(|event| match event {
                HostRuntimeEvent::SessionAction {
                    action:
                        HostAction::Encoder(EncoderAction::Configure {
                            operation_id,
                            width: 854,
                            height: 480,
                            ..
                        }),
                    ..
                } => Some(*operation_id),
                _ => None,
            })
            .expect("tier change reconfigures encoder");

        let completed = runtime
            .on_encoder_configured(operation_id, Ok(()), 4_200_000)
            .unwrap_or_else(|error| panic!("quality configure result: {error}"));
        let reset_epoch = completed.iter().find_map(|event| match event {
            HostRuntimeEvent::SessionAction {
                action: HostAction::SendControl(ControlMessage::StreamReset(reset)),
                ..
            } if reset.status == racc_proto::StreamStatus::Ok => {
                assert_eq!((reset.width, reset.height), (854, 480));
                Some(reset.epoch)
            }
            _ => None,
        });
        let reset_epoch = reset_epoch.expect("successful quality stream reset");
        assert_eq!(reset_epoch, epoch.wrapping_add(1));
        assert!(completed.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SendControl {
                message: ControlMessage::QualityAdjustment(adjustment),
                ..
            } if adjustment.epoch == reset_epoch
                && adjustment.reason == QualityAdjustmentReason::Loss
                && adjustment.from_height == 720
                && adjustment.to_height == 480
        )));
        assert!(completed.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SessionAction {
                action: HostAction::Encoder(EncoderAction::ForceKeyframe { epoch: id }),
                ..
            } if *id == reset_epoch
        )));
        assert_eq!(runtime.status().quality_tier, Some(HostQualityTier::P480));
    }

    #[test]
    fn stale_viewer_report_cannot_change_quality_and_fixed_preference_reconfigures() {
        let mut runtime = runtime();
        let peer = socket("100.100.10.2", 50_001);
        start_stream(&mut runtime, peer);
        let epoch = runtime.session.epoch();

        let stale = runtime
            .on_control(
                1,
                peer,
                ControlMessage::ViewerReport(viewer_report(epoch.wrapping_sub(1), 1000, 1000, 500)),
                100_000,
            )
            .unwrap_or_else(|error| panic!("stale report: {error}"));
        assert_eq!(stale.len(), 1);
        assert!(matches!(stale[0], HostRuntimeEvent::ViewerFeedback(_)));

        let preference = runtime
            .on_control(
                1,
                peer,
                ControlMessage::SetQuality(SetQuality {
                    max_height: 480,
                    bitrate_hint_kbps: 0,
                }),
                200_000,
            )
            .unwrap_or_else(|error| panic!("quality preference: {error}"));
        assert!(preference.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::QualityDecision(QualityEvent::TierChanged {
                to: QualityTier::P480,
                reason: racc_session::QualityChangeReason::Preference,
                ..
            })
        )));
        assert!(preference.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SessionAction {
                action: HostAction::Encoder(EncoderAction::Configure { height: 480, .. }),
                ..
            }
        )));
        assert_eq!(
            runtime
                .on_control(
                    1,
                    peer,
                    ControlMessage::SetQuality(SetQuality {
                        max_height: 600,
                        bitrate_hint_kbps: 0,
                    }),
                    300_000,
                )
                .unwrap_or_else(|error| panic!("invalid quality request: {error}")),
            [HostRuntimeEvent::CloseConnection(1)]
        );
    }

    #[test]
    fn stale_local_sender_observation_cannot_adapt_or_replace_current_sample() {
        let mut runtime = runtime();
        let peer = socket("100.100.10.2", 50_001);
        start_stream(&mut runtime, peer);
        let epoch = runtime.session.epoch();
        let initial_bitrate = runtime
            .quality_controller
            .as_ref()
            .map(QualityController::current_bitrate_bps);

        let events = runtime
            .on_local_sender_observation(
                HostSenderObservation {
                    epoch: epoch.wrapping_sub(1),
                    queue_overflow: true,
                    encoder_lag_ms: Some(59_000),
                },
                100_000,
            )
            .unwrap_or_else(|error| panic!("stale sender observation: {error}"));

        assert!(events.is_empty());
        assert_eq!(runtime.latest_sender_observation(), None);
        assert_eq!(
            runtime
                .quality_controller
                .as_ref()
                .map(QualityController::current_bitrate_bps),
            initial_bitrate
        );
    }

    #[test]
    fn local_sender_queue_overflow_triggers_host_quality_policy() {
        let mut runtime = runtime();
        let peer = socket("100.100.10.2", 50_001);
        start_stream(&mut runtime, peer);
        let epoch = runtime.session.epoch();
        let sample = |queue_overflow| HostSenderObservation {
            epoch,
            queue_overflow,
            encoder_lag_ms: Some(17),
        };

        let first = runtime
            .on_local_sender_observation(sample(true), 1_000_000)
            .unwrap_or_else(|error| panic!("first sender sample: {error}"));
        assert!(!first
            .iter()
            .any(|event| matches!(event, HostRuntimeEvent::QualityDecision(_))));
        let second = runtime
            .on_local_sender_observation(sample(true), 1_500_000)
            .unwrap_or_else(|error| panic!("second sender sample: {error}"));
        assert!(second.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::QualityDecision(QualityEvent::BitrateTrim { .. })
        )));
        let downshift = runtime
            .on_local_sender_observation(sample(false), 1_600_000)
            .unwrap_or_else(|error| panic!("sender follow-up sample: {error}"));
        assert!(downshift.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::QualityDecision(QualityEvent::TierChanged {
                to: QualityTier::P480,
                reason: racc_session::QualityChangeReason::QueueOverflow,
                ..
            })
        )));
        assert!(downshift.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SessionAction {
                action: HostAction::Encoder(EncoderAction::Configure { height: 480, .. }),
                ..
            }
        )));
    }

    #[test]
    fn encoder_lag_is_retained_without_an_inferred_adaptation_threshold() {
        let mut runtime = runtime();
        let peer = socket("100.100.10.2", 50_001);
        start_stream(&mut runtime, peer);
        let epoch = runtime.session.epoch();
        let observation = HostSenderObservation {
            epoch,
            queue_overflow: false,
            encoder_lag_ms: Some(MAX_HOST_ENCODER_LAG_MS),
        };

        let events = runtime
            .on_local_sender_observation(observation, 1_000)
            .unwrap_or_else(|error| panic!("encoder lag observation: {error}"));

        assert!(events.is_empty());
        assert_eq!(runtime.latest_sender_observation(), Some(observation));
        assert_eq!(
            runtime.on_local_sender_observation(
                HostSenderObservation {
                    encoder_lag_ms: Some(MAX_HOST_ENCODER_LAG_MS + 1),
                    ..observation
                },
                2_000,
            ),
            Err(HostRuntimeError::InvalidQualityObservation)
        );
    }

    #[test]
    fn local_observation_updates_controller_using_current_viewer_feedback() {
        let mut runtime = runtime();
        let peer = socket("100.100.10.2", 50_001);
        start_stream(&mut runtime, peer);
        let epoch = runtime.session.epoch();
        runtime
            .on_control(
                1,
                peer,
                ControlMessage::ViewerReport(viewer_report(epoch, 0, 0, 20)),
                1_000,
            )
            .unwrap_or_else(|error| panic!("baseline report: {error}"));
        runtime
            .on_control(
                1,
                peer,
                ControlMessage::ViewerReport(viewer_report(epoch, 30, 0, 20)),
                1_000_000,
            )
            .unwrap_or_else(|error| panic!("loss report: {error}"));

        let events = runtime
            .on_local_sender_observation(
                HostSenderObservation {
                    epoch,
                    queue_overflow: false,
                    encoder_lag_ms: Some(40),
                },
                2_000_000,
            )
            .unwrap_or_else(|error| panic!("local feedback update: {error}"));

        assert!(events.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::QualityDecision(QualityEvent::BitrateTrim {
                to_bps: 3_150_000,
                percent: 90,
                ..
            })
        )));
        assert_eq!(
            runtime.latest_sender_observation().unwrap().encoder_lag_ms,
            Some(40)
        );
    }

    #[test]
    fn failed_quality_configuration_restores_the_prior_policy_and_bitrate() {
        let mut runtime = runtime();
        let peer = socket("100.100.10.2", 50_001);
        start_stream(&mut runtime, peer);
        let pending = runtime
            .on_control(
                1,
                peer,
                ControlMessage::SetQuality(SetQuality {
                    max_height: 480,
                    bitrate_hint_kbps: 0,
                }),
                100_000,
            )
            .unwrap_or_else(|error| panic!("quality preference: {error}"));
        let operation_id = pending
            .iter()
            .find_map(|event| match event {
                HostRuntimeEvent::SessionAction {
                    action: HostAction::Encoder(EncoderAction::Configure { operation_id, .. }),
                    ..
                } => Some(*operation_id),
                _ => None,
            })
            .expect("quality configure operation");

        let failed = runtime
            .on_encoder_configured(operation_id, Err(EncoderFailure::ConfigureFailed), 200_000)
            .unwrap_or_else(|error| panic!("failed quality configure: {error}"));
        let controller = runtime
            .quality_controller
            .as_ref()
            .expect("prior quality controller restored");
        assert_eq!(controller.current_tier(), QualityTier::P720);
        assert_eq!(controller.preference(), QualityPreference::Auto);
        assert!(failed.iter().any(|event| matches!(
            event,
            HostRuntimeEvent::SessionAction {
                action: HostAction::Encoder(EncoderAction::SetBitrate(3_500_000)),
                ..
            }
        )));
    }

    #[test]
    fn viewer_report_is_forwarded_only_from_the_authorized_connection() {
        let mut runtime = runtime();
        runtime
            .update_tailscale_address(Some(address("100.100.10.1")))
            .unwrap_or_else(|error| panic!("address update: {error}"));
        let peer = socket("100.100.10.2", 50_001);
        let report = ViewerReport {
            epoch: 7,
            loss_permille: 12,
            frame_loss_permille: 5,
            rtt_ms: 24,
            decode_ms_p95: 9,
            dropped_frames: 2,
        };

        assert_eq!(
            runtime
                .on_control(99, peer, ControlMessage::ViewerReport(report), 1_000)
                .unwrap_or_else(|error| panic!("unauthorized report: {error}")),
            [HostRuntimeEvent::CloseConnection(99)]
        );

        runtime
            .on_hello(1, peer, &hello(), approved())
            .unwrap_or_else(|error| panic!("hello: {error}"));
        assert_eq!(
            runtime
                .on_control(1, peer, ControlMessage::ViewerReport(report), 2_000)
                .unwrap_or_else(|error| panic!("authorized report: {error}")),
            [HostRuntimeEvent::ViewerFeedback(report)]
        );
    }

    #[test]
    fn nonmatching_control_source_is_closed_and_stop_is_final() {
        let mut runtime = runtime();
        runtime
            .update_tailscale_address(Some(address("100.100.10.1")))
            .unwrap_or_else(|error| panic!("address update: {error}"));
        let peer = socket("100.100.10.2", 50_001);
        let _ = runtime
            .on_hello(4, peer, &hello(), approved())
            .unwrap_or_else(|error| panic!("hello: {error}"));
        assert_eq!(
            runtime
                .on_control(
                    4,
                    socket("100.100.10.3", 50_001),
                    ControlMessage::PauseVideo(PauseVideo),
                    1_000,
                )
                .unwrap_or_else(|error| panic!("wrong source: {error}")),
            vec![HostRuntimeEvent::CloseConnection(4)]
        );
        let events = runtime.stop();
        assert!(events.contains(&HostRuntimeEvent::BindAddressChanged(None)));
        assert_eq!(runtime.status().phase, HostRuntimePhase::Stopped);
        assert_eq!(
            runtime.update_tailscale_address(None),
            Err(HostRuntimeError::Stopped)
        );
    }
}
