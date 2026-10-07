use crate::codec::{Reader, Writer};
use crate::{
    ProtoError, ProtoResult, MAX_CLIPBOARD_BYTES, MAX_CONTROL_FRAME_BYTES, MAX_CURSOR_BYTES,
    MAX_CURSOR_DIM, MAX_DISPLAYS,
};

/// Identifies the operating system represented in a handshake.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum OsType {
    /// Unknown or unreported operating system.
    Unknown = 0,
    /// Microsoft Windows.
    Windows = 1,
    /// macOS.
    MacOs = 2,
    /// Linux.
    Linux = 3,
}

/// Result of the host's Hello handshake response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum HelloStatus {
    /// The protocol version and peer are accepted.
    Ok = 0,
    /// The protocol version is unsupported.
    UnsupportedVersion = 1,
    /// The peer is not authorized.
    NotAuthorized = 2,
    /// The host is busy.
    Busy = 3,
}

/// Viewer-to-host handshake capabilities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    /// Protocol version requested by the viewer.
    pub protocol_version: u8,
    /// Human-readable device name.
    pub device_name: String,
    /// Operating system of the viewer.
    pub os: OsType,
    /// Application version string.
    pub app_version: String,
    /// UDP port announced by the viewer for video.
    pub video_udp_port: u16,
    /// Codec capability bits; bit zero indicates H.264.
    pub codecs: u32,
    /// Maximum requested display height.
    pub max_height: u16,
    /// Feature bits; bit zero indicates text clipboard support.
    pub features: u32,
}

/// Host-to-viewer handshake response and capabilities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelloAck {
    /// Protocol version selected by the host.
    pub protocol_version: u8,
    /// Handshake result.
    pub status: HelloStatus,
    /// Human-readable host device name.
    pub device_name: String,
    /// Operating system of the host.
    pub os: OsType,
    /// Application version string.
    pub app_version: String,
    /// Codec capability bits; bit zero indicates H.264.
    pub codecs: u32,
    /// Maximum stream height supported by the host.
    pub max_height: u16,
    /// Feature bits; bit zero indicates text clipboard support.
    pub features: u32,
    /// Number of host CPU cores.
    pub host_cpu_cores: u8,
}

/// One display in a topology announcement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayInfo {
    /// Stable display identifier.
    pub display_id: u32,
    /// Display name.
    pub name: String,
    /// Virtual desktop x origin in physical pixels.
    pub x: i32,
    /// Virtual desktop y origin in physical pixels.
    pub y: i32,
    /// Display width in physical pixels; must be nonzero.
    pub width_px: u32,
    /// Display height in physical pixels; must be nonzero.
    pub height_px: u32,
    /// Scale factor in thousandths (1500 means 150%).
    pub scale_milli: u16,
    /// Display refresh rate in thousandths of a hertz.
    pub refresh_mhz: u32,
    /// Display flags: bit 0 primary, bit 1 active, bit 2 available, bit 3 HDR label.
    pub flags: u8,
}

/// Complete display topology sent by the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopologyAnnounce {
    /// Monotonically increasing topology revision.
    pub topology_rev: u32,
    /// Active display identifier, or zero if there is no active display.
    pub active_display_id: u32,
    /// Display list, with unique identifiers and at most MAX_DISPLAYS entries.
    pub displays: Vec<DisplayInfo>,
}

/// Viewer request to switch the host's selected display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwitchMonitor {
    /// Request identifier echoed by the host.
    pub req_id: u32,
    /// Stable display identifier to select.
    pub display_id: u32,
}

/// Video codec carried by a stream reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StreamCodec {
    /// H.264/AVC.
    H264 = 1,
}

/// Result of a host stream reset or monitor switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StreamStatus {
    /// The requested stream is ready.
    Ok = 0,
    /// The requested display does not exist.
    DisplayNotFound = 1,
    /// Capture could not be started.
    CaptureFailed = 2,
    /// Encoding could not be started.
    EncoderFailed = 3,
    /// Video is paused.
    Paused = 4,
    /// The host cannot process the request yet.
    Busy = 5,
}

/// Host stream configuration and switch result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamReset {
    /// Request identifier, or zero when initiated by the host.
    pub req_id: u32,
    /// New stream epoch.
    pub epoch: u16,
    /// Selected video codec.
    pub codec: StreamCodec,
    /// Encoded stream width.
    pub width: u16,
    /// Encoded stream height.
    pub height: u16,
    /// Stream frame rate.
    pub fps: u8,
    /// Topology revision used for this stream.
    pub topology_rev: u32,
    /// Selected stable display identifier.
    pub display_id: u32,
    /// Reset outcome.
    pub status: StreamStatus,
}

/// Viewer preference for host-selected stream quality.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetQuality {
    /// Maximum requested height: zero means automatic; otherwise 480, 720, or 1080.
    pub max_height: u16,
    /// Bitrate hint in kilobits per second; zero means automatic.
    pub bitrate_hint_kbps: u32,
}

/// Request for an IDR/keyframe in the current epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestKeyframe {
    /// Epoch for which a keyframe is requested.
    pub epoch: u16,
}

/// Pause-video control message with no fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PauseVideo;

/// Resume-video control message with no fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResumeVideo;

/// Mouse or keyboard event kind sent to the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEventKind {
    /// Absolute pointer position in normalized 16-bit coordinates.
    MouseMoveAbs {
        /// Horizontal position from zero through 65535.
        u: u16,
        /// Vertical position from zero through 65535.
        v: u16,
    },
    /// Relative pointer movement in raw device units.
    MouseMoveRel {
        /// Horizontal movement.
        dx: i16,
        /// Vertical movement.
        dy: i16,
    },
    /// Mouse button transition.
    MouseButton {
        /// Button number: 1 left, 2 right, 3 middle, 4 back, 5 forward.
        button: u8,
        /// Whether the button is pressed.
        pressed: bool,
    },
    /// Mouse wheel movement in units of 1/120 notch.
    Wheel {
        /// Horizontal wheel delta.
        dx: i16,
        /// Vertical wheel delta.
        dy: i16,
    },
    /// Keyboard transition using USB HID usage page 0x07.
    Key {
        /// HID usage code.
        hid_usage: u16,
        /// Whether the key is pressed.
        pressed: bool,
        /// Modifier bits: shift, control, alt, and meta occupy bits zero through three.
        modifiers: u8,
    },
}

/// Epoch-bound input event for a selected display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputEvent {
    /// Stream epoch.
    pub epoch: u16,
    /// Selected stable display identifier.
    pub display_id: u32,
    /// Input action.
    pub event: InputEventKind,
}

/// Direction from which clipboard text originated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ClipboardOrigin {
    /// Text originated at the viewer.
    Viewer = 0,
    /// Text originated at the host.
    Host = 1,
}

/// UTF-8 text clipboard update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardUpdate {
    /// Monotonic clipboard sequence number.
    pub seq: u32,
    /// Side that originated the update.
    pub origin: ClipboardOrigin,
    /// Clipboard UTF-8 text, at most MAX_CLIPBOARD_BYTES bytes.
    pub text: String,
}

/// Host display-capture backend reported in telemetry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CaptureBackend {
    /// Backend not reported.
    Unknown = 0,
    /// DXGI Desktop Duplication.
    Dxgi = 1,
    /// Windows Graphics Capture.
    Wgc = 2,
    /// macOS ScreenCaptureKit.
    ScreenCaptureKit = 3,
    /// Compatibility fallback CGDisplayStream.
    CgDisplayStream = 4,
}

/// Host encoder reported in telemetry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Encoder {
    /// Encoder not reported.
    Unknown = 0,
    /// Windows Media Foundation hardware encoder.
    MediaFoundationHw = 1,
    /// OpenH264 software encoder.
    OpenH264 = 2,
    /// macOS VideoToolbox.
    VideoToolbox = 3,
    /// NVIDIA NVENC.
    Nvenc = 4,
    /// AMD AMF.
    Amf = 5,
    /// Intel oneVPL/QSV.
    Qsv = 6,
}

/// Host CPU and active-stream telemetry report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatsReport {
    /// Host CPU percentage multiplied by ten, from 0 through 1000.
    pub host_cpu_pct_x10: u16,
    /// Active capture backend.
    pub capture_backend: CaptureBackend,
    /// Active video encoder.
    pub encoder: Encoder,
    /// Encoded stream width.
    pub width: u16,
    /// Encoded stream height.
    pub height: u16,
    /// Display refresh rate in thousandths of a hertz.
    pub display_refresh_mhz: u32,
    /// Target bitrate in kilobits per second.
    pub target_bitrate_kbps: u32,
    /// Measured bitrate in kilobits per second.
    pub actual_bitrate_kbps: u32,
}

/// Ping request used to measure round-trip time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ping {
    /// Opaque request identifier.
    pub nonce: u64,
    /// Sender timestamp in microseconds.
    pub sender_ts_us: u64,
}

/// Ping response used to measure round-trip time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pong {
    /// Opaque request identifier echoed from Ping.
    pub nonce: u64,
    /// Sender timestamp echoed from Ping in microseconds.
    pub echo_ts_us: u64,
}

/// BGRA cursor bitmap transported over the reliable control channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorShape {
    /// Cursor shape identifier referenced by cursor datagrams.
    pub shape_id: u32,
    /// Bitmap width in pixels, from one through MAX_CURSOR_DIM.
    pub width: u16,
    /// Bitmap height in pixels, from one through MAX_CURSOR_DIM.
    pub height: u16,
    /// Horizontal hotspot coordinate, less than width.
    pub hotspot_x: u16,
    /// Vertical hotspot coordinate, less than height.
    pub hotspot_y: u16,
    /// BGRA bitmap bytes, exactly width times height times four.
    pub bgra: Vec<u8>,
}

/// Reason a peer is closing the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum GoodbyeReason {
    /// Normal close.
    Normal = 0,
    /// Close after an error.
    Error = 1,
    /// A newer session superseded this one.
    Superseded = 2,
    /// Peer is not authorized.
    NotAuthorized = 3,
    /// Host is shutting down.
    Shutdown = 4,
}

/// Connection-close control message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Goodbye {
    /// Reason for closing.
    pub reason: GoodbyeReason,
}

/// Typed v0 control message. Variant order is not its wire type number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlMessage {
    /// Type 1, viewer handshake.
    Hello(Hello),
    /// Type 2, host handshake response.
    HelloAck(HelloAck),
    /// Type 3, host display topology.
    TopologyAnnounce(TopologyAnnounce),
    /// Type 4, viewer display-switch request.
    SwitchMonitor(SwitchMonitor),
    /// Type 5, host stream reset.
    StreamReset(StreamReset),
    /// Type 6, viewer quality preference.
    SetQuality(SetQuality),
    /// Type 7, viewer keyframe request.
    RequestKeyframe(RequestKeyframe),
    /// Type 8, pause video.
    PauseVideo(PauseVideo),
    /// Type 9, resume video.
    ResumeVideo(ResumeVideo),
    /// Type 10, viewer input event.
    InputEvent(InputEvent),
    /// Type 11, text clipboard update.
    ClipboardUpdate(ClipboardUpdate),
    /// Type 12, host performance report.
    StatsReport(StatsReport),
    /// Type 13, ping.
    Ping(Ping),
    /// Type 14, pong.
    Pong(Pong),
    /// Type 15, cursor bitmap.
    CursorShape(CursorShape),
    /// Type 16, connection close.
    Goodbye(Goodbye),
}

/// Common payload codec implemented by each typed control-message structure.
pub trait ControlPayload: Sized {
    /// The fixed control message type number.
    const TYPE: u8;

    /// Encodes this message's payload, excluding the type byte and frame prefix.
    fn encode_payload(&self, output: &mut Vec<u8>) -> ProtoResult<()>;

    /// Decodes one payload from the start of a byte slice and reports bytes consumed.
    fn decode_payload(input: &[u8]) -> ProtoResult<(Self, usize)>;
}

impl ControlMessage {
    /// Encodes a complete control frame, including its four-byte length prefix.
    pub fn encode_frame(&self, output: &mut Vec<u8>) -> ProtoResult<()> {
        let (message_type, payload) = self.encode_payload_inner()?;
        let frame_body_len = payload.len().checked_add(1).ok_or(ProtoError::TooLarge)?;
        if frame_body_len > MAX_CONTROL_FRAME_BYTES {
            return Err(ProtoError::TooLarge);
        }
        let frame_length = u32::try_from(frame_body_len).map_err(|_| ProtoError::TooLarge)?;
        output.extend_from_slice(&frame_length.to_le_bytes());
        output.push(message_type);
        output.extend_from_slice(&payload);
        Ok(())
    }

    /// Decodes a complete control body (type byte followed by payload).
    pub fn decode_body(body: &[u8]) -> ProtoResult<Self> {
        if body.len() > MAX_CONTROL_FRAME_BYTES {
            return Err(ProtoError::TooLarge);
        }
        let (message_type, payload) = body.split_first().ok_or(ProtoError::Truncated)?;
        let (message, consumed) = decode_payload_for_type(*message_type, payload)?;
        if consumed != payload.len() {
            return Err(ProtoError::TrailingBytes);
        }
        Ok(message)
    }

    fn encode_payload_inner(&self) -> ProtoResult<(u8, Vec<u8>)> {
        let mut payload = Vec::new();
        let message_type = match self {
            Self::Hello(value) => encode_typed(value, &mut payload)?,
            Self::HelloAck(value) => encode_typed(value, &mut payload)?,
            Self::TopologyAnnounce(value) => encode_typed(value, &mut payload)?,
            Self::SwitchMonitor(value) => encode_typed(value, &mut payload)?,
            Self::StreamReset(value) => encode_typed(value, &mut payload)?,
            Self::SetQuality(value) => encode_typed(value, &mut payload)?,
            Self::RequestKeyframe(value) => encode_typed(value, &mut payload)?,
            Self::PauseVideo(value) => encode_typed(value, &mut payload)?,
            Self::ResumeVideo(value) => encode_typed(value, &mut payload)?,
            Self::InputEvent(value) => encode_typed(value, &mut payload)?,
            Self::ClipboardUpdate(value) => encode_typed(value, &mut payload)?,
            Self::StatsReport(value) => encode_typed(value, &mut payload)?,
            Self::Ping(value) => encode_typed(value, &mut payload)?,
            Self::Pong(value) => encode_typed(value, &mut payload)?,
            Self::CursorShape(value) => encode_typed(value, &mut payload)?,
            Self::Goodbye(value) => encode_typed(value, &mut payload)?,
        };
        Ok((message_type, payload))
    }
}

fn encode_typed<T: ControlPayload>(value: &T, output: &mut Vec<u8>) -> ProtoResult<u8> {
    value.encode_payload(output)?;
    Ok(T::TYPE)
}

fn decode_payload_for_type(message_type: u8, input: &[u8]) -> ProtoResult<(ControlMessage, usize)> {
    macro_rules! decode {
        ($ty:ty, $variant:ident) => {{
            let (value, consumed) = <$ty as ControlPayload>::decode_payload(input)?;
            Ok((ControlMessage::$variant(value), consumed))
        }};
    }

    match message_type {
        1 => decode!(Hello, Hello),
        2 => decode!(HelloAck, HelloAck),
        3 => decode!(TopologyAnnounce, TopologyAnnounce),
        4 => decode!(SwitchMonitor, SwitchMonitor),
        5 => decode!(StreamReset, StreamReset),
        6 => decode!(SetQuality, SetQuality),
        7 => decode!(RequestKeyframe, RequestKeyframe),
        8 => decode!(PauseVideo, PauseVideo),
        9 => decode!(ResumeVideo, ResumeVideo),
        10 => decode!(InputEvent, InputEvent),
        11 => decode!(ClipboardUpdate, ClipboardUpdate),
        12 => decode!(StatsReport, StatsReport),
        13 => decode!(Ping, Ping),
        14 => decode!(Pong, Pong),
        15 => decode!(CursorShape, CursorShape),
        16 => decode!(Goodbye, Goodbye),
        _ => Err(ProtoError::UnknownMessageType),
    }
}

fn decode_os(value: u8) -> ProtoResult<OsType> {
    match value {
        0 => Ok(OsType::Unknown),
        1 => Ok(OsType::Windows),
        2 => Ok(OsType::MacOs),
        3 => Ok(OsType::Linux),
        _ => Err(ProtoError::InvalidValue),
    }
}

fn decode_hello_status(value: u8) -> ProtoResult<HelloStatus> {
    match value {
        0 => Ok(HelloStatus::Ok),
        1 => Ok(HelloStatus::UnsupportedVersion),
        2 => Ok(HelloStatus::NotAuthorized),
        3 => Ok(HelloStatus::Busy),
        _ => Err(ProtoError::InvalidValue),
    }
}

fn decode_stream_codec(value: u8) -> ProtoResult<StreamCodec> {
    match value {
        1 => Ok(StreamCodec::H264),
        _ => Err(ProtoError::InvalidValue),
    }
}

fn decode_stream_status(value: u8) -> ProtoResult<StreamStatus> {
    match value {
        0 => Ok(StreamStatus::Ok),
        1 => Ok(StreamStatus::DisplayNotFound),
        2 => Ok(StreamStatus::CaptureFailed),
        3 => Ok(StreamStatus::EncoderFailed),
        4 => Ok(StreamStatus::Paused),
        5 => Ok(StreamStatus::Busy),
        _ => Err(ProtoError::InvalidValue),
    }
}

fn decode_clipboard_origin(value: u8) -> ProtoResult<ClipboardOrigin> {
    match value {
        0 => Ok(ClipboardOrigin::Viewer),
        1 => Ok(ClipboardOrigin::Host),
        _ => Err(ProtoError::InvalidValue),
    }
}

fn decode_capture_backend(value: u8) -> ProtoResult<CaptureBackend> {
    match value {
        0 => Ok(CaptureBackend::Unknown),
        1 => Ok(CaptureBackend::Dxgi),
        2 => Ok(CaptureBackend::Wgc),
        3 => Ok(CaptureBackend::ScreenCaptureKit),
        4 => Ok(CaptureBackend::CgDisplayStream),
        _ => Err(ProtoError::InvalidValue),
    }
}

fn decode_encoder(value: u8) -> ProtoResult<Encoder> {
    match value {
        0 => Ok(Encoder::Unknown),
        1 => Ok(Encoder::MediaFoundationHw),
        2 => Ok(Encoder::OpenH264),
        3 => Ok(Encoder::VideoToolbox),
        4 => Ok(Encoder::Nvenc),
        5 => Ok(Encoder::Amf),
        6 => Ok(Encoder::Qsv),
        _ => Err(ProtoError::InvalidValue),
    }
}

fn decode_goodbye_reason(value: u8) -> ProtoResult<GoodbyeReason> {
    match value {
        0 => Ok(GoodbyeReason::Normal),
        1 => Ok(GoodbyeReason::Error),
        2 => Ok(GoodbyeReason::Superseded),
        3 => Ok(GoodbyeReason::NotAuthorized),
        4 => Ok(GoodbyeReason::Shutdown),
        _ => Err(ProtoError::InvalidValue),
    }
}

fn write_hello(value: &Hello, writer: &mut Writer) -> ProtoResult<()> {
    if value.codecs & !1 != 0 || value.features & !1 != 0 {
        return Err(ProtoError::InvalidValue);
    }
    writer.u8(value.protocol_version);
    writer.string(&value.device_name)?;
    writer.u8(value.os as u8);
    writer.string(&value.app_version)?;
    writer.u16(value.video_udp_port);
    writer.u32(value.codecs);
    writer.u16(value.max_height);
    writer.u32(value.features);
    Ok(())
}

fn read_hello(reader: &mut Reader<'_>) -> ProtoResult<Hello> {
    let protocol_version = reader.u8()?;
    let device_name = reader.string()?;
    let os = decode_os(reader.u8()?)?;
    let app_version = reader.string()?;
    let video_udp_port = reader.u16()?;
    let codecs = reader.u32()?;
    let max_height = reader.u16()?;
    let features = reader.u32()?;
    if codecs & !1 != 0 || features & !1 != 0 {
        return Err(ProtoError::InvalidValue);
    }
    Ok(Hello {
        protocol_version,
        device_name,
        os,
        app_version,
        video_udp_port,
        codecs,
        max_height,
        features,
    })
}

fn write_hello_ack(value: &HelloAck, writer: &mut Writer) -> ProtoResult<()> {
    if value.codecs & !1 != 0 || value.features & !1 != 0 {
        return Err(ProtoError::InvalidValue);
    }
    writer.u8(value.protocol_version);
    writer.u8(value.status as u8);
    writer.string(&value.device_name)?;
    writer.u8(value.os as u8);
    writer.string(&value.app_version)?;
    writer.u32(value.codecs);
    writer.u16(value.max_height);
    writer.u32(value.features);
    writer.u8(value.host_cpu_cores);
    Ok(())
}

fn read_hello_ack(reader: &mut Reader<'_>) -> ProtoResult<HelloAck> {
    let protocol_version = reader.u8()?;
    let status = decode_hello_status(reader.u8()?)?;
    let device_name = reader.string()?;
    let os = decode_os(reader.u8()?)?;
    let app_version = reader.string()?;
    let codecs = reader.u32()?;
    let max_height = reader.u16()?;
    let features = reader.u32()?;
    let host_cpu_cores = reader.u8()?;
    if codecs & !1 != 0 || features & !1 != 0 {
        return Err(ProtoError::InvalidValue);
    }
    Ok(HelloAck {
        protocol_version,
        status,
        device_name,
        os,
        app_version,
        codecs,
        max_height,
        features,
        host_cpu_cores,
    })
}

fn validate_topology(value: &TopologyAnnounce) -> ProtoResult<()> {
    if value.displays.len() > MAX_DISPLAYS {
        return Err(ProtoError::TooLarge);
    }
    for (index, display) in value.displays.iter().enumerate() {
        if display.width_px == 0 || display.height_px == 0 || display.flags & !0x0f != 0 {
            return Err(ProtoError::InvalidValue);
        }
        if value
            .displays
            .iter()
            .take(index)
            .any(|earlier| earlier.display_id == display.display_id)
        {
            return Err(ProtoError::InvalidValue);
        }
    }
    Ok(())
}

fn write_topology(value: &TopologyAnnounce, writer: &mut Writer) -> ProtoResult<()> {
    validate_topology(value)?;
    writer.u32(value.topology_rev);
    writer.u32(value.active_display_id);
    writer.u8(u8::try_from(value.displays.len()).map_err(|_| ProtoError::TooLarge)?);
    for display in &value.displays {
        writer.u32(display.display_id);
        writer.string(&display.name)?;
        writer.i32(display.x);
        writer.i32(display.y);
        writer.u32(display.width_px);
        writer.u32(display.height_px);
        writer.u16(display.scale_milli);
        writer.u32(display.refresh_mhz);
        writer.u8(display.flags);
    }
    Ok(())
}

fn read_topology(reader: &mut Reader<'_>) -> ProtoResult<TopologyAnnounce> {
    let topology_rev = reader.u32()?;
    let active_display_id = reader.u32()?;
    let count = usize::from(reader.u8()?);
    if count > MAX_DISPLAYS {
        return Err(ProtoError::TooLarge);
    }
    let mut displays = Vec::with_capacity(count);
    for _ in 0..count {
        let display = DisplayInfo {
            display_id: reader.u32()?,
            name: reader.string()?,
            x: reader.i32()?,
            y: reader.i32()?,
            width_px: reader.u32()?,
            height_px: reader.u32()?,
            scale_milli: reader.u16()?,
            refresh_mhz: reader.u32()?,
            flags: reader.u8()?,
        };
        if display.width_px == 0 || display.height_px == 0 || display.flags & !0x0f != 0 {
            return Err(ProtoError::InvalidValue);
        }
        if displays
            .iter()
            .any(|prior: &DisplayInfo| prior.display_id == display.display_id)
        {
            return Err(ProtoError::InvalidValue);
        }
        displays.push(display);
    }
    Ok(TopologyAnnounce {
        topology_rev,
        active_display_id,
        displays,
    })
}

fn write_switch_monitor(value: &SwitchMonitor, writer: &mut Writer) -> ProtoResult<()> {
    writer.u32(value.req_id);
    writer.u32(value.display_id);
    Ok(())
}

fn read_switch_monitor(reader: &mut Reader<'_>) -> ProtoResult<SwitchMonitor> {
    Ok(SwitchMonitor {
        req_id: reader.u32()?,
        display_id: reader.u32()?,
    })
}

fn write_stream_reset(value: &StreamReset, writer: &mut Writer) -> ProtoResult<()> {
    if value.status == StreamStatus::Ok && (value.width == 0 || value.height == 0 || value.fps == 0)
    {
        return Err(ProtoError::InvalidValue);
    }
    writer.u32(value.req_id);
    writer.u16(value.epoch);
    writer.u8(value.codec as u8);
    writer.u16(value.width);
    writer.u16(value.height);
    writer.u8(value.fps);
    writer.u32(value.topology_rev);
    writer.u32(value.display_id);
    writer.u8(value.status as u8);
    Ok(())
}

fn read_stream_reset(reader: &mut Reader<'_>) -> ProtoResult<StreamReset> {
    let req_id = reader.u32()?;
    let epoch = reader.u16()?;
    let codec = decode_stream_codec(reader.u8()?)?;
    let width = reader.u16()?;
    let height = reader.u16()?;
    let fps = reader.u8()?;
    let topology_rev = reader.u32()?;
    let display_id = reader.u32()?;
    let status = decode_stream_status(reader.u8()?)?;
    if status == StreamStatus::Ok && (width == 0 || height == 0 || fps == 0) {
        return Err(ProtoError::InvalidValue);
    }
    Ok(StreamReset {
        req_id,
        epoch,
        codec,
        width,
        height,
        fps,
        topology_rev,
        display_id,
        status,
    })
}

fn write_set_quality(value: &SetQuality, writer: &mut Writer) -> ProtoResult<()> {
    if value.max_height != 0 && !matches!(value.max_height, 480 | 720 | 1080) {
        return Err(ProtoError::InvalidValue);
    }
    writer.u16(value.max_height);
    writer.u32(value.bitrate_hint_kbps);
    Ok(())
}

fn read_set_quality(reader: &mut Reader<'_>) -> ProtoResult<SetQuality> {
    let max_height = reader.u16()?;
    if max_height != 0 && !matches!(max_height, 480 | 720 | 1080) {
        return Err(ProtoError::InvalidValue);
    }
    Ok(SetQuality {
        max_height,
        bitrate_hint_kbps: reader.u32()?,
    })
}

fn write_request_keyframe(value: &RequestKeyframe, writer: &mut Writer) -> ProtoResult<()> {
    writer.u16(value.epoch);
    Ok(())
}

fn read_request_keyframe(reader: &mut Reader<'_>) -> ProtoResult<RequestKeyframe> {
    Ok(RequestKeyframe {
        epoch: reader.u16()?,
    })
}

fn write_pause(_: &PauseVideo, _: &mut Writer) -> ProtoResult<()> {
    Ok(())
}

fn read_pause(_: &mut Reader<'_>) -> ProtoResult<PauseVideo> {
    Ok(PauseVideo)
}

fn write_resume(_: &ResumeVideo, _: &mut Writer) -> ProtoResult<()> {
    Ok(())
}

fn read_resume(_: &mut Reader<'_>) -> ProtoResult<ResumeVideo> {
    Ok(ResumeVideo)
}

fn read_bool(reader: &mut Reader<'_>) -> ProtoResult<bool> {
    match reader.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ProtoError::InvalidValue),
    }
}

fn write_input_event(value: &InputEvent, writer: &mut Writer) -> ProtoResult<()> {
    writer.u16(value.epoch);
    writer.u32(value.display_id);
    match value.event {
        InputEventKind::MouseMoveAbs { u, v } => {
            writer.u8(1);
            writer.u16(u);
            writer.u16(v);
        }
        InputEventKind::MouseMoveRel { dx, dy } => {
            writer.u8(2);
            writer.i16(dx);
            writer.i16(dy);
        }
        InputEventKind::MouseButton { button, pressed } => {
            if !(1..=5).contains(&button) {
                return Err(ProtoError::InvalidValue);
            }
            writer.u8(3);
            writer.u8(button);
            writer.u8(u8::from(pressed));
        }
        InputEventKind::Wheel { dx, dy } => {
            writer.u8(4);
            writer.i16(dx);
            writer.i16(dy);
        }
        InputEventKind::Key {
            hid_usage,
            pressed,
            modifiers,
        } => {
            if modifiers & !0x0f != 0 {
                return Err(ProtoError::InvalidValue);
            }
            writer.u8(5);
            writer.u16(hid_usage);
            writer.u8(u8::from(pressed));
            writer.u8(modifiers);
        }
    }
    Ok(())
}

fn read_input_event(reader: &mut Reader<'_>) -> ProtoResult<InputEvent> {
    let epoch = reader.u16()?;
    let display_id = reader.u32()?;
    let event = match reader.u8()? {
        1 => InputEventKind::MouseMoveAbs {
            u: reader.u16()?,
            v: reader.u16()?,
        },
        2 => InputEventKind::MouseMoveRel {
            dx: reader.i16()?,
            dy: reader.i16()?,
        },
        3 => {
            let button = reader.u8()?;
            if !(1..=5).contains(&button) {
                return Err(ProtoError::InvalidValue);
            }
            InputEventKind::MouseButton {
                button,
                pressed: read_bool(reader)?,
            }
        }
        4 => InputEventKind::Wheel {
            dx: reader.i16()?,
            dy: reader.i16()?,
        },
        5 => {
            let hid_usage = reader.u16()?;
            let pressed = read_bool(reader)?;
            let modifiers = reader.u8()?;
            if modifiers & !0x0f != 0 {
                return Err(ProtoError::InvalidValue);
            }
            InputEventKind::Key {
                hid_usage,
                pressed,
                modifiers,
            }
        }
        _ => return Err(ProtoError::InvalidValue),
    };
    Ok(InputEvent {
        epoch,
        display_id,
        event,
    })
}

fn write_clipboard(value: &ClipboardUpdate, writer: &mut Writer) -> ProtoResult<()> {
    if value.text.len() > MAX_CLIPBOARD_BYTES {
        return Err(ProtoError::TooLarge);
    }
    writer.u32(value.seq);
    writer.u8(value.origin as u8);
    writer.u8(1);
    writer.u32(u32::try_from(value.text.len()).map_err(|_| ProtoError::TooLarge)?);
    writer.bytes.extend_from_slice(value.text.as_bytes());
    Ok(())
}

fn read_clipboard(reader: &mut Reader<'_>) -> ProtoResult<ClipboardUpdate> {
    let seq = reader.u32()?;
    let origin = decode_clipboard_origin(reader.u8()?)?;
    if reader.u8()? != 1 {
        return Err(ProtoError::InvalidValue);
    }
    let length = usize::try_from(reader.u32()?).map_err(|_| ProtoError::TooLarge)?;
    if length > MAX_CLIPBOARD_BYTES {
        return Err(ProtoError::TooLarge);
    }
    let bytes = reader.take(length)?;
    let text = core::str::from_utf8(bytes)
        .map_err(|_| ProtoError::InvalidUtf8)?
        .to_owned();
    Ok(ClipboardUpdate { seq, origin, text })
}

fn write_stats(value: &StatsReport, writer: &mut Writer) -> ProtoResult<()> {
    if value.host_cpu_pct_x10 > 1000 {
        return Err(ProtoError::InvalidValue);
    }
    writer.u16(value.host_cpu_pct_x10);
    writer.u8(value.capture_backend as u8);
    writer.u8(value.encoder as u8);
    writer.u16(value.width);
    writer.u16(value.height);
    writer.u32(value.display_refresh_mhz);
    writer.u32(value.target_bitrate_kbps);
    writer.u32(value.actual_bitrate_kbps);
    Ok(())
}

fn read_stats(reader: &mut Reader<'_>) -> ProtoResult<StatsReport> {
    let host_cpu_pct_x10 = reader.u16()?;
    if host_cpu_pct_x10 > 1000 {
        return Err(ProtoError::InvalidValue);
    }
    Ok(StatsReport {
        host_cpu_pct_x10,
        capture_backend: decode_capture_backend(reader.u8()?)?,
        encoder: decode_encoder(reader.u8()?)?,
        width: reader.u16()?,
        height: reader.u16()?,
        display_refresh_mhz: reader.u32()?,
        target_bitrate_kbps: reader.u32()?,
        actual_bitrate_kbps: reader.u32()?,
    })
}

fn write_ping(value: &Ping, writer: &mut Writer) -> ProtoResult<()> {
    writer.u64(value.nonce);
    writer.u64(value.sender_ts_us);
    Ok(())
}

fn read_ping(reader: &mut Reader<'_>) -> ProtoResult<Ping> {
    Ok(Ping {
        nonce: reader.u64()?,
        sender_ts_us: reader.u64()?,
    })
}

fn write_pong(value: &Pong, writer: &mut Writer) -> ProtoResult<()> {
    writer.u64(value.nonce);
    writer.u64(value.echo_ts_us);
    Ok(())
}

fn read_pong(reader: &mut Reader<'_>) -> ProtoResult<Pong> {
    Ok(Pong {
        nonce: reader.u64()?,
        echo_ts_us: reader.u64()?,
    })
}

fn cursor_byte_length(width: u16, height: u16) -> ProtoResult<usize> {
    let width = usize::from(width);
    let height = usize::from(height);
    if width == 0 || height == 0 || width > MAX_CURSOR_DIM || height > MAX_CURSOR_DIM {
        return Err(ProtoError::InvalidValue);
    }
    let length = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(ProtoError::TooLarge)?;
    if length > MAX_CURSOR_BYTES {
        return Err(ProtoError::TooLarge);
    }
    Ok(length)
}

fn validate_cursor_shape(value: &CursorShape) -> ProtoResult<()> {
    let expected = cursor_byte_length(value.width, value.height)?;
    if value.hotspot_x >= value.width || value.hotspot_y >= value.height {
        return Err(ProtoError::InvalidValue);
    }
    if value.bgra.len() != expected {
        return Err(ProtoError::InvalidValue);
    }
    Ok(())
}

fn write_cursor_shape(value: &CursorShape, writer: &mut Writer) -> ProtoResult<()> {
    validate_cursor_shape(value)?;
    writer.u32(value.shape_id);
    writer.u16(value.width);
    writer.u16(value.height);
    writer.u16(value.hotspot_x);
    writer.u16(value.hotspot_y);
    writer.bytes.extend_from_slice(&value.bgra);
    Ok(())
}

fn read_cursor_shape(reader: &mut Reader<'_>) -> ProtoResult<CursorShape> {
    let shape_id = reader.u32()?;
    let width = reader.u16()?;
    let height = reader.u16()?;
    let hotspot_x = reader.u16()?;
    let hotspot_y = reader.u16()?;
    let length = cursor_byte_length(width, height)?;
    if hotspot_x >= width || hotspot_y >= height {
        return Err(ProtoError::InvalidValue);
    }
    let bgra = reader.take(length)?.to_vec();
    Ok(CursorShape {
        shape_id,
        width,
        height,
        hotspot_x,
        hotspot_y,
        bgra,
    })
}

fn write_goodbye(value: &Goodbye, writer: &mut Writer) -> ProtoResult<()> {
    writer.u8(value.reason as u8);
    Ok(())
}

fn read_goodbye(reader: &mut Reader<'_>) -> ProtoResult<Goodbye> {
    Ok(Goodbye {
        reason: decode_goodbye_reason(reader.u8()?)?,
    })
}

macro_rules! impl_payload {
    ($ty:ty, $number:expr, $write:path, $read:path) => {
        impl ControlPayload for $ty {
            const TYPE: u8 = $number;

            fn encode_payload(&self, output: &mut Vec<u8>) -> ProtoResult<()> {
                let mut writer = Writer::default();
                $write(self, &mut writer)?;
                if writer
                    .bytes
                    .len()
                    .checked_add(1)
                    .ok_or(ProtoError::TooLarge)?
                    > MAX_CONTROL_FRAME_BYTES
                {
                    return Err(ProtoError::TooLarge);
                }
                output.extend_from_slice(&writer.bytes);
                Ok(())
            }

            fn decode_payload(input: &[u8]) -> ProtoResult<(Self, usize)> {
                let mut reader = Reader::new(input);
                let value = $read(&mut reader)?;
                Ok((value, reader.position()))
            }
        }
    };
}

impl_payload!(Hello, 1, write_hello, read_hello);
impl_payload!(HelloAck, 2, write_hello_ack, read_hello_ack);
impl_payload!(TopologyAnnounce, 3, write_topology, read_topology);
impl_payload!(SwitchMonitor, 4, write_switch_monitor, read_switch_monitor);
impl_payload!(StreamReset, 5, write_stream_reset, read_stream_reset);
impl_payload!(SetQuality, 6, write_set_quality, read_set_quality);
impl_payload!(
    RequestKeyframe,
    7,
    write_request_keyframe,
    read_request_keyframe
);
impl_payload!(PauseVideo, 8, write_pause, read_pause);
impl_payload!(ResumeVideo, 9, write_resume, read_resume);
impl_payload!(InputEvent, 10, write_input_event, read_input_event);
impl_payload!(ClipboardUpdate, 11, write_clipboard, read_clipboard);
impl_payload!(StatsReport, 12, write_stats, read_stats);
impl_payload!(Ping, 13, write_ping, read_ping);
impl_payload!(Pong, 14, write_pong, read_pong);
impl_payload!(CursorShape, 15, write_cursor_shape, read_cursor_shape);
impl_payload!(Goodbye, 16, write_goodbye, read_goodbye);
