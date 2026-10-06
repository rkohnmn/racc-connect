use crate::{
    EventKind, EventLog, EventLogError, EventLogSnapshot, PingRecordOutcome, PingTracker,
    RateWindow, RttEstimator, RttEstimatorError,
};
use core::fmt;
use racc_proto::{CaptureBackend, Encoder, StatsReport};

/// Control-connection lifecycle shown by telemetry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConnectionState {
    /// No connection is active.
    #[default]
    Disconnected,
    /// Transport connection is being established.
    Connecting,
    /// Protocol handshake is in progress.
    Handshaking,
    /// Session is connected.
    Connected,
    /// Reconnection is in progress.
    Reconnecting,
}

/// Current Tailscale path classification.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PathKind {
    /// Path has not been reported.
    #[default]
    Unknown,
    /// Direct peer-to-peer path.
    Direct,
    /// DERP relay path.
    Derp,
}

/// Video codec selected for the stream.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CodecKind {
    /// Codec has not yet been reported.
    #[default]
    Unknown,
    /// H.264 / AVC.
    H264,
}

/// Decoder implementation. Current protocol StatsReport does not transmit decoder kind.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DecoderKind {
    /// Decoder has not been reported.
    #[default]
    Unknown,
    /// Windows Media Foundation / DXVA decoder.
    MediaFoundation,
    /// macOS VideoToolbox decoder.
    VideoToolbox,
    /// Software H.264 decoder.
    Software,
}

impl DecoderKind {
    /// Maps the local decoder code space to a decoder kind; unknown values are tolerated.
    ///
    /// These codes are reserved for a future StatsReport field; v0 StatsReport has none.
    pub const fn from_wire_code(code: u8) -> Self {
        match code {
            1 => Self::MediaFoundation,
            2 => Self::VideoToolbox,
            3 => Self::Software,
            _ => Self::Unknown,
        }
    }

    /// Returns the local numeric code, with zero representing unknown.
    pub const fn to_wire_code(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::MediaFoundation => 1,
            Self::VideoToolbox => 2,
            Self::Software => 3,
        }
    }
}

/// Host capture backend, normalized from StatsReport numeric codes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CaptureBackendKind {
    /// Backend has not been reported or is not recognized.
    #[default]
    Unknown,
    /// DXGI Desktop Duplication.
    Dxgi,
    /// Windows Graphics Capture.
    Wgc,
    /// macOS ScreenCaptureKit.
    ScreenCaptureKit,
    /// Compatibility CGDisplayStream.
    CgDisplayStream,
}

impl CaptureBackendKind {
    /// Converts the protocol numeric code to a local kind without panicking.
    pub const fn from_wire_code(code: u8) -> Self {
        match code {
            1 => Self::Dxgi,
            2 => Self::Wgc,
            3 => Self::ScreenCaptureKit,
            4 => Self::CgDisplayStream,
            _ => Self::Unknown,
        }
    }

    /// Converts a local kind to its protocol numeric code.
    pub const fn to_wire_code(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Dxgi => 1,
            Self::Wgc => 2,
            Self::ScreenCaptureKit => 3,
            Self::CgDisplayStream => 4,
        }
    }
}

impl From<CaptureBackend> for CaptureBackendKind {
    fn from(value: CaptureBackend) -> Self {
        Self::from_wire_code(value as u8)
    }
}

/// Host encoder, normalized from StatsReport numeric codes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EncoderKind {
    /// Encoder has not been reported or is not recognized.
    #[default]
    Unknown,
    /// Windows Media Foundation hardware encoder.
    MediaFoundationHw,
    /// OpenH264 software encoder.
    OpenH264,
    /// macOS VideoToolbox.
    VideoToolbox,
    /// NVIDIA NVENC.
    Nvenc,
    /// AMD AMF.
    Amf,
    /// Intel QSV / oneVPL.
    Qsv,
}

impl EncoderKind {
    /// Converts the protocol numeric code to a local kind without panicking.
    pub const fn from_wire_code(code: u8) -> Self {
        match code {
            1 => Self::MediaFoundationHw,
            2 => Self::OpenH264,
            3 => Self::VideoToolbox,
            4 => Self::Nvenc,
            5 => Self::Amf,
            6 => Self::Qsv,
            _ => Self::Unknown,
        }
    }

    /// Converts a local kind to its protocol numeric code.
    pub const fn to_wire_code(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::MediaFoundationHw => 1,
            Self::OpenH264 => 2,
            Self::VideoToolbox => 3,
            Self::Nvenc => 4,
            Self::Amf => 5,
            Self::Qsv => 6,
        }
    }
}

impl From<Encoder> for EncoderKind {
    fn from(value: Encoder) -> Self {
        Self::from_wire_code(value as u8)
    }
}

/// Session telemetry snapshot suitable for UI display.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SessionSnapshot {
    /// Control connection state.
    pub connection_state: ConnectionState,
    /// Direct or DERP path classification.
    pub path: PathKind,
    /// Latest measured RTT in microseconds.
    pub last_rtt_us: Option<u64>,
    /// Minimum RTT in the recent estimator window in microseconds.
    pub min_rtt_us: Option<u64>,
    /// Smoothed RTT in microseconds.
    pub srtt_us: Option<f64>,
    /// RTT variation in microseconds.
    pub rttvar_us: Option<f64>,
    /// Jitter estimate in microseconds.
    pub jitter_us: Option<f64>,
    /// Packet loss fraction, normalized to the range 0 through 1.
    pub loss_fraction: f64,
    /// Frame loss fraction, normalized to the range 0 through 1.
    pub frame_loss_fraction: f64,
    /// Approximate measured bitrate in bits per second.
    pub bitrate_bps: u64,
    /// Observed frame rate from the recent window.
    pub fps: f64,
    /// Stream codec.
    pub codec: CodecKind,
    /// Active decoder.
    pub decoder: DecoderKind,
    /// Current stream epoch.
    pub epoch: u16,
}

/// Host telemetry snapshot, excluding dropped GPU usage.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HostSnapshot {
    /// Host CPU percent multiplied by ten.
    pub cpu_pct_x10: u16,
    /// Active capture backend.
    pub capture_backend: CaptureBackendKind,
    /// Active encoder.
    pub encoder: EncoderKind,
    /// Encoded stream width.
    pub width: u16,
    /// Encoded stream height.
    pub height: u16,
    /// Display refresh rate in thousandths of a hertz.
    pub refresh_mhz: u32,
    /// Target bitrate in kilobits per second.
    pub target_bitrate_kbps: u32,
    /// Measured bitrate in kilobits per second.
    pub actual_bitrate_kbps: u32,
}

impl HostSnapshot {
    /// Converts one received protocol host-statistics report.
    pub fn from_stats_report(report: StatsReport) -> Self {
        Self {
            cpu_pct_x10: report.host_cpu_pct_x10,
            capture_backend: report.capture_backend.into(),
            encoder: report.encoder.into(),
            width: report.width,
            height: report.height,
            refresh_mhz: report.display_refresh_mhz,
            target_bitrate_kbps: report.target_bitrate_kbps,
            actual_bitrate_kbps: report.actual_bitrate_kbps,
        }
    }
}

/// Immutable cheaply cloned aggregate telemetry snapshot.
#[derive(Clone, Debug, Default)]
pub struct TelemetrySnapshot {
    /// Session counters and status.
    pub session: SessionSnapshot,
    /// Host-reported capture and encoder status.
    pub host: HostSnapshot,
    /// Retained event log snapshot.
    pub events: EventLogSnapshot,
}

/// Typed telemetry configuration or event-log error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryError {
    /// RTT minimum-window configuration is invalid.
    Rtt(RttEstimatorError),
    /// Event ID allocation failed.
    EventLog(EventLogError),
}

impl fmt::Display for TelemetryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rtt(error) => error.fmt(formatter),
            Self::EventLog(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TelemetryError {}

impl From<RttEstimatorError> for TelemetryError {
    fn from(value: RttEstimatorError) -> Self {
        Self::Rtt(value)
    }
}

impl From<EventLogError> for TelemetryError {
    fn from(value: EventLogError) -> Self {
        Self::EventLog(value)
    }
}

/// Sans-I/O telemetry accumulator; the core can publish snapshots at about 4 Hz.
///
/// Snapshot collection only copies small scalar state and clones the event snapshot.
/// The UI can poll snapshots independently of video; this type owns no locks, I/O,
/// threads, or pixel data.
#[derive(Clone, Debug)]
pub struct TelemetryHub {
    session: SessionSnapshot,
    host: HostSnapshot,
    rtt: RttEstimator,
    frame_rates: RateWindow,
    byte_rates: RateWindow,
    events: EventLog,
    ping_tracker: PingTracker,
    frame_loss_override: Option<f64>,
}

impl Default for TelemetryHub {
    fn default() -> Self {
        Self {
            session: SessionSnapshot::default(),
            host: HostSnapshot::default(),
            rtt: RttEstimator::default(),
            frame_rates: RateWindow::default(),
            byte_rates: RateWindow::default(),
            events: EventLog::new(),
            ping_tracker: PingTracker::new(),
            frame_loss_override: None,
        }
    }
}

impl TelemetryHub {
    /// Creates a hub with default one-second rates and ten-second RTT minimum window.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a hub with a caller-selected RTT minimum window.
    pub fn with_rtt_window(window_us: u64) -> Result<Self, TelemetryError> {
        Ok(Self {
            rtt: RttEstimator::new(window_us)?,
            ..Self::default()
        })
    }

    /// Records an RTT sample at an injected timestamp.
    pub fn record_rtt(&mut self, now_us: u64, rtt_us: u64) {
        self.rtt.record(now_us, rtt_us);
    }

    /// Records one received or transmitted frame and whether it was lost.
    pub fn record_frame(&mut self, now_us: u64, lost: bool) -> bool {
        let accepted = self.frame_rates.record(now_us, 1, u64::from(lost));
        if accepted {
            self.frame_loss_override = None;
        }
        accepted
    }

    /// Records payload bytes at an injected timestamp.
    pub fn record_bytes(&mut self, now_us: u64, bytes: u64) -> bool {
        self.byte_rates.record(now_us, 0, bytes)
    }

    /// Supplies the latest packet and frame loss fractions.
    pub fn record_loss(&mut self, _now_us: u64, loss_fraction: f64, frame_loss_fraction: f64) {
        self.session.loss_fraction = finite_fraction(loss_fraction);
        self.session.frame_loss_fraction = finite_fraction(frame_loss_fraction);
        self.frame_loss_override = Some(self.session.frame_loss_fraction);
        self.frame_loss_override = Some(self.session.frame_loss_fraction);
    }

    /// Updates direct/DERP path classification.
    pub fn set_path(&mut self, _now_us: u64, path: PathKind) {
        self.session.path = path;
    }

    /// Updates connection lifecycle state.
    pub fn set_connection_state(&mut self, _now_us: u64, state: ConnectionState) {
        self.session.connection_state = state;
    }

    /// Updates codec, decoder and current stream epoch.
    pub fn set_stream(&mut self, _now_us: u64, codec: CodecKind, decoder: DecoderKind, epoch: u16) {
        self.session.codec = codec;
        self.session.decoder = decoder;
        self.session.epoch = epoch;
    }

    /// Adds a timestamped event to the bounded ring.
    pub fn push_event(
        &mut self,
        now_us: u64,
        kind: EventKind,
        detail: impl Into<String>,
    ) -> Result<(), TelemetryError> {
        self.events.push(now_us, kind, detail)?;
        Ok(())
    }

    /// Replaces host telemetry from one protocol StatsReport.
    pub fn update_host_stats(&mut self, _now_us: u64, report: StatsReport) {
        self.host = HostSnapshot::from_stats_report(report);
    }

    /// Records a ping request, evicting and counting the oldest at capacity.
    pub fn record_ping(&mut self, nonce: u64, echo_ts_us: u64, now_us: u64) -> PingRecordOutcome {
        self.ping_tracker.record_ping(nonce, echo_ts_us, now_us)
    }

    /// Matches a pong, returning its RTT and feeding a matched sample to the estimator.
    pub fn match_pong(&mut self, nonce: u64, echo_ts_us: u64, now_us: u64) -> Option<u64> {
        let rtt = self.ping_tracker.match_pong(nonce, echo_ts_us, now_us)?;
        self.record_rtt(now_us, rtt);
        Some(rtt)
    }

    /// Expires outstanding pings and returns their count.
    pub fn expire_pings(&mut self, now_us: u64, timeout_us: u64) -> usize {
        self.ping_tracker.expire(now_us, timeout_us)
    }

    /// Returns a cheap immutable snapshot, advancing rolling windows to `now_us`.
    pub fn snapshot(&mut self, now_us: u64) -> TelemetrySnapshot {
        let rtt = self.rtt.snapshot(now_us);
        let frames = self.frame_rates.snapshot(now_us);
        let bytes = self.byte_rates.snapshot(now_us);
        self.session.last_rtt_us = rtt.last_rtt_us;
        self.session.min_rtt_us = rtt.min_rtt_us;
        self.session.srtt_us = rtt.srtt_us;
        self.session.rttvar_us = rtt.rttvar_us;
        self.session.jitter_us = rtt.jitter_us;
        self.session.fps = frames.events_per_second;
        if let Some(override_fraction) = self.frame_loss_override {
            self.session.frame_loss_fraction = override_fraction;
        } else if frames.events > 0 {
            self.session.frame_loss_fraction =
                (frames.bytes as f64 / frames.events as f64).clamp(0.0, 1.0);
        }
        if let Some(override_fraction) = self.frame_loss_override {
            self.session.frame_loss_fraction = override_fraction;
        } else if frames.events > 0 {
            self.session.frame_loss_fraction =
                (frames.bytes as f64 / frames.events as f64).clamp(0.0, 1.0);
        }
        self.session.bitrate_bps = (bytes.bytes_per_second * 8.0).min(u64::MAX as f64) as u64;
        TelemetrySnapshot {
            session: self.session,
            host: self.host,
            events: self.events.snapshot(),
        }
    }

    /// Returns an event snapshot containing only IDs after the supplied cursor.
    pub fn events_since(&self, id: u64) -> EventLogSnapshot {
        self.events.events_since(id)
    }

    /// Returns ping tracker diagnostics.
    pub fn ping_snapshot(&self) -> crate::PingTrackerSnapshot {
        self.ping_tracker.snapshot()
    }
}

fn finite_fraction(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_wire_code_mappings_round_trip_and_unknown_codes_are_safe() {
        for code in 0..=u8::MAX {
            let capture = CaptureBackendKind::from_wire_code(code);
            assert_eq!(
                CaptureBackendKind::from_wire_code(capture.to_wire_code()),
                capture
            );
            let encoder = EncoderKind::from_wire_code(code);
            assert_eq!(EncoderKind::from_wire_code(encoder.to_wire_code()), encoder);
            let decoder = DecoderKind::from_wire_code(code);
            assert_eq!(DecoderKind::from_wire_code(decoder.to_wire_code()), decoder);
        }
        assert_eq!(
            CaptureBackendKind::from_wire_code(255),
            CaptureBackendKind::Unknown
        );
        assert_eq!(EncoderKind::from_wire_code(255), EncoderKind::Unknown);
        assert_eq!(DecoderKind::from_wire_code(255), DecoderKind::Unknown);
    }

    #[test]
    fn stats_report_conversion_excludes_gpu_and_maps_host_fields() {
        let report = StatsReport {
            host_cpu_pct_x10: 225,
            capture_backend: CaptureBackend::ScreenCaptureKit,
            encoder: Encoder::VideoToolbox,
            width: 1280,
            height: 720,
            display_refresh_mhz: 60_000,
            target_bitrate_kbps: 3_500,
            actual_bitrate_kbps: 3_200,
        };
        let host = HostSnapshot::from_stats_report(report);
        assert_eq!(host.cpu_pct_x10, 225);
        assert_eq!(host.capture_backend, CaptureBackendKind::ScreenCaptureKit);
        assert_eq!(host.encoder, EncoderKind::VideoToolbox);
        assert_eq!((host.width, host.height), (1280, 720));
    }

    #[test]
    fn hub_updates_and_snapshot_keep_ui_data_small_and_bounded() {
        let mut hub = TelemetryHub::new();
        hub.set_connection_state(1, ConnectionState::Connected);
        hub.set_path(1, PathKind::Direct);
        hub.set_stream(1, CodecKind::H264, DecoderKind::Software, 7);
        hub.record_rtt(10, 1_000);
        hub.record_frame(10, false);
        hub.record_frame(510_000, false);
        hub.record_bytes(10, 100_000);
        hub.record_loss(10, 0.02, 0.01);
        hub.push_event(11, EventKind::ConnectionEstablished, "ready")
            .expect("event id");
        let snapshot = hub.snapshot(999_999);
        assert_eq!(
            snapshot.session.connection_state,
            ConnectionState::Connected
        );
        assert_eq!(snapshot.session.path, PathKind::Direct);
        assert_eq!(snapshot.session.epoch, 7);
        assert_eq!(snapshot.session.fps, 2.0);
        assert_eq!(snapshot.session.bitrate_bps, 800_000);
        assert_eq!(snapshot.session.loss_fraction, 0.02);
        assert_eq!(snapshot.events.events().len(), 1);
        assert_eq!(hub.events_since(0).events().len(), 1);
    }

    #[test]
    fn arbitrary_updates_with_backwards_times_stay_finite_and_bounded() {
        let mut hub = TelemetryHub::new();
        for step in 0..10_000u64 {
            let now = if step % 7 == 0 {
                u64::MAX - step
            } else {
                step / 2
            };
            hub.record_rtt(now, step.wrapping_mul(31));
            hub.record_frame(now, step % 11 == 0);
            hub.record_bytes(now, step);
            hub.record_loss(now, f64::NAN, 1.5);
        }
        let snapshot = hub.snapshot(u64::MAX);
        assert!(snapshot.session.fps.is_finite());
        assert!(snapshot.session.loss_fraction.is_finite());
        assert_eq!(snapshot.session.frame_loss_fraction, 1.0);
        assert!(hub.ping_snapshot().outstanding <= crate::MAX_OUTSTANDING_PINGS);
    }
}
