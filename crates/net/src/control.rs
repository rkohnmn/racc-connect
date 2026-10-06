use crate::{validate_bind_addr, BindError, BindPolicy};
use racc_proto::{ControlMessage, FrameDecoder, ProtoError};
use socket2::{SockRef, TcpKeepalive};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

/// Default idle period before the operating system sends TCP keepalive probes.
pub const DEFAULT_KEEPALIVE_IDLE: Duration = Duration::from_secs(10);
/// Default write timeout for control messages.
pub const DEFAULT_CONTROL_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// Default read timeout for a blocking control receive.
pub const DEFAULT_CONTROL_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Typed errors from control stream validation and I/O.
#[derive(Debug)]
pub enum ControlError {
    /// An operating-system operation failed.
    Io(io::Error),
    /// A bounded control frame was malformed.
    Proto(ProtoError),
    /// A read or write timed out.
    Timeout,
    /// The peer closed the control stream.
    Closed,
    /// A local or remote IP violated the selected bind policy.
    Bind(BindError),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "control I/O error: {error}"),
            Self::Proto(error) => write!(f, "control protocol error: {error}"),
            Self::Timeout => f.write_str("control stream timed out"),
            Self::Closed => f.write_str("control peer closed the stream"),
            Self::Bind(error) => write!(f, "control bind policy rejected address: {error}"),
        }
    }
}

impl std::error::Error for ControlError {}

impl From<ProtoError> for ControlError {
    fn from(error: ProtoError) -> Self {
        Self::Proto(error)
    }
}

impl From<BindError> for ControlError {
    fn from(error: BindError) -> Self {
        Self::Bind(error)
    }
}

impl From<io::Error> for ControlError {
    fn from(error: io::Error) -> Self {
        if matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ) {
            Self::Timeout
        } else {
            Self::Io(error)
        }
    }
}

/// Configurable timeouts used when establishing or accepting control connections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlSettings {
    /// Idle duration before TCP keepalive probes begin.
    pub keepalive_idle: Duration,
    /// Maximum time for a full control-frame write.
    pub write_timeout: Duration,
    /// Timeout for each blocking receive attempt.
    pub read_timeout: Option<Duration>,
    /// Maximum time a TCP connect helper waits for connection establishment.
    pub connect_timeout: Duration,
}

impl Default for ControlSettings {
    fn default() -> Self {
        Self {
            keepalive_idle: DEFAULT_KEEPALIVE_IDLE,
            write_timeout: DEFAULT_CONTROL_WRITE_TIMEOUT,
            read_timeout: Some(DEFAULT_CONTROL_READ_TIMEOUT),
            connect_timeout: DEFAULT_CONTROL_WRITE_TIMEOUT,
        }
    }
}

/// Applies the low-latency and keepalive settings required by the control channel.
pub fn configure_control_stream(
    stream: &TcpStream,
    settings: ControlSettings,
) -> Result<(), ControlError> {
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(settings.write_timeout))?;
    stream.set_read_timeout(settings.read_timeout)?;
    let keepalive = TcpKeepalive::new().with_time(settings.keepalive_idle);
    SockRef::from(stream).set_tcp_keepalive(&keepalive)?;
    Ok(())
}

/// TCP listener bound to a validated interface address.
#[derive(Debug)]
pub struct ControlListener {
    listener: TcpListener,
    policy: BindPolicy,
    settings: ControlSettings,
}

impl ControlListener {
    /// Binds a TCP listener after validating the interface address.
    pub fn bind(
        address: SocketAddr,
        policy: BindPolicy,
        settings: ControlSettings,
    ) -> Result<Self, ControlError> {
        validate_bind_addr(address.ip(), policy)?;
        Ok(Self {
            listener: TcpListener::bind(address)?,
            policy,
            settings,
        })
    }

    /// Returns the actual local address, including an assigned ephemeral port.
    pub fn local_addr(&self) -> Result<SocketAddr, ControlError> {
        Ok(self.listener.local_addr()?)
    }

    /// Accepts one connection, validates its peer address, and configures TCP_NODELAY/keepalive.
    pub fn accept(&self) -> Result<(ControlConn, SocketAddr), ControlError> {
        let (stream, peer) = self.listener.accept()?;
        validate_bind_addr(peer.ip(), self.policy)?;
        let conn = ControlConn::from_stream(stream, self.settings)?;
        Ok((conn, peer))
    }
}

/// Connects to a Tailscale or explicitly test-only peer address.
pub fn connect_control(
    address: SocketAddr,
    policy: BindPolicy,
    settings: ControlSettings,
) -> Result<ControlConn, ControlError> {
    validate_bind_addr(address.ip(), policy)?;
    let stream = TcpStream::connect_timeout(&address, settings.connect_timeout)?;
    ControlConn::from_stream(stream, settings)
}

/// Reliable, bounded TCP control connection.
#[derive(Debug)]
pub struct ControlConn {
    stream: TcpStream,
    decoder: FrameDecoder,
    ready: VecDeque<ControlMessage>,
    read_buffer: [u8; 4096],
}

impl ControlConn {
    /// Wraps an established stream and applies TCP_NODELAY, keepalive and timeouts.
    fn from_stream(stream: TcpStream, settings: ControlSettings) -> Result<Self, ControlError> {
        configure_control_stream(&stream, settings)?;
        Ok(Self {
            stream,
            decoder: FrameDecoder::new(),
            ready: VecDeque::new(),
            read_buffer: [0; 4096],
        })
    }

    /// Encodes and writes one length-prefixed control message.
    pub fn send(&mut self, message: &ControlMessage) -> Result<(), ControlError> {
        write_message(&mut self.stream, message)
    }

    /// Reads the next complete length-prefixed control message.
    pub fn recv(&mut self) -> Result<ControlMessage, ControlError> {
        receive_message(
            &mut self.stream,
            &mut self.decoder,
            &mut self.ready,
            &mut self.read_buffer,
        )
    }

    /// Clones the socket into independent read and write halves for separate threads.
    pub fn split(&self) -> Result<(ControlReadHalf, ControlWriteHalf), ControlError> {
        let read_stream = self.stream.try_clone()?;
        let write_stream = self.stream.try_clone()?;
        Ok((
            ControlReadHalf {
                stream: read_stream,
                decoder: FrameDecoder::new(),
                ready: VecDeque::new(),
                read_buffer: [0; 4096],
            },
            ControlWriteHalf {
                stream: write_stream,
            },
        ))
    }

    /// Returns the connected peer address.
    pub fn peer_addr(&self) -> Result<SocketAddr, ControlError> {
        Ok(self.stream.peer_addr()?)
    }

    /// Returns the local socket address.
    pub fn local_addr(&self) -> Result<SocketAddr, ControlError> {
        Ok(self.stream.local_addr()?)
    }

    /// Returns whether TCP_NODELAY is enabled.
    pub fn nodelay(&self) -> Result<bool, ControlError> {
        Ok(self.stream.nodelay()?)
    }
}

/// Read half of a control connection.
#[derive(Debug)]
pub struct ControlReadHalf {
    stream: TcpStream,
    decoder: FrameDecoder,
    ready: VecDeque<ControlMessage>,
    read_buffer: [u8; 4096],
}

impl ControlReadHalf {
    /// Reads the next complete control message.
    pub fn recv(&mut self) -> Result<ControlMessage, ControlError> {
        receive_message(
            &mut self.stream,
            &mut self.decoder,
            &mut self.ready,
            &mut self.read_buffer,
        )
    }
}

/// Write half of a control connection.
#[derive(Debug)]
pub struct ControlWriteHalf {
    stream: TcpStream,
}

impl ControlWriteHalf {
    /// Writes one complete control message.
    pub fn send(&mut self, message: &ControlMessage) -> Result<(), ControlError> {
        write_message(&mut self.stream, message)
    }
}

fn write_message(stream: &mut TcpStream, message: &ControlMessage) -> Result<(), ControlError> {
    let mut frame = Vec::new();
    message.encode_frame(&mut frame)?;
    stream.write_all(&frame)?;
    Ok(())
}

fn receive_message(
    stream: &mut TcpStream,
    decoder: &mut FrameDecoder,
    ready: &mut VecDeque<ControlMessage>,
    read_buffer: &mut [u8; 4096],
) -> Result<ControlMessage, ControlError> {
    if let Some(message) = ready.pop_front() {
        return Ok(message);
    }
    loop {
        let bytes_read = match stream.read(read_buffer) {
            Ok(0) => {
                if decoder.buffered_len() > 0 {
                    return Err(ControlError::Proto(ProtoError::Truncated));
                }
                return Err(ControlError::Closed);
            }
            Ok(length) => length,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(ControlError::Timeout)
            }
            Err(error) => return Err(ControlError::Io(error)),
        };
        let chunk = read_buffer.get(..bytes_read).ok_or(ControlError::Closed)?;
        decoder.feed(chunk, |message| ready.push_back(message))?;
        if let Some(message) = ready.pop_front() {
            return Ok(message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_proto::{
        CaptureBackend, ClipboardOrigin, ClipboardUpdate, CursorShape, DisplayInfo, Encoder,
        Goodbye, GoodbyeReason, Hello, HelloAck, HelloStatus, InputEvent, InputEventKind, OsType,
        PauseVideo, Ping, Pong, RequestKeyframe, ResumeVideo, SetQuality, StatsReport, StreamCodec,
        StreamReset, StreamStatus, SwitchMonitor, TopologyAnnounce,
    };
    use std::net::TcpListener;
    use std::thread;

    fn settings(read_timeout: Option<Duration>) -> ControlSettings {
        ControlSettings {
            read_timeout,
            ..ControlSettings::default()
        }
    }

    fn messages() -> Vec<ControlMessage> {
        vec![
            ControlMessage::Hello(Hello {
                protocol_version: 0,
                device_name: "viewer".to_owned(),
                os: OsType::Windows,
                app_version: "0.1".to_owned(),
                video_udp_port: 42123,
                codecs: 1,
                max_height: 1080,
                features: 1,
            }),
            ControlMessage::HelloAck(HelloAck {
                protocol_version: 0,
                status: HelloStatus::Ok,
                device_name: "host".to_owned(),
                os: OsType::Windows,
                app_version: "0.1".to_owned(),
                codecs: 1,
                max_height: 1080,
                features: 1,
                host_cpu_cores: 8,
            }),
            ControlMessage::TopologyAnnounce(TopologyAnnounce {
                topology_rev: 1,
                active_display_id: 9,
                displays: vec![DisplayInfo {
                    display_id: 9,
                    name: "Display 1".to_owned(),
                    x: 0,
                    y: 0,
                    width_px: 1920,
                    height_px: 1080,
                    scale_milli: 1000,
                    refresh_mhz: 60000,
                    flags: 7,
                }],
            }),
            ControlMessage::SwitchMonitor(SwitchMonitor {
                req_id: 1,
                display_id: 9,
            }),
            ControlMessage::StreamReset(StreamReset {
                req_id: 1,
                epoch: 2,
                codec: StreamCodec::H264,
                width: 1280,
                height: 720,
                fps: 30,
                topology_rev: 1,
                display_id: 9,
                status: StreamStatus::Ok,
            }),
            ControlMessage::SetQuality(SetQuality {
                max_height: 720,
                bitrate_hint_kbps: 3500,
            }),
            ControlMessage::RequestKeyframe(RequestKeyframe { epoch: 2 }),
            ControlMessage::PauseVideo(PauseVideo),
            ControlMessage::ResumeVideo(ResumeVideo),
            ControlMessage::InputEvent(InputEvent {
                epoch: 2,
                display_id: 9,
                event: InputEventKind::MouseMoveAbs { u: 1, v: 2 },
            }),
            ControlMessage::ClipboardUpdate(ClipboardUpdate {
                seq: 5,
                origin: ClipboardOrigin::Viewer,
                text: "hello".to_owned(),
            }),
            ControlMessage::StatsReport(StatsReport {
                host_cpu_pct_x10: 250,
                capture_backend: CaptureBackend::Dxgi,
                encoder: Encoder::MediaFoundationHw,
                width: 1280,
                height: 720,
                display_refresh_mhz: 60000,
                target_bitrate_kbps: 3500,
                actual_bitrate_kbps: 3400,
            }),
            ControlMessage::Ping(Ping {
                nonce: 1,
                sender_ts_us: 2,
            }),
            ControlMessage::Pong(Pong {
                nonce: 1,
                echo_ts_us: 2,
            }),
            ControlMessage::CursorShape(CursorShape {
                shape_id: 1,
                width: 1,
                height: 1,
                hotspot_x: 0,
                hotspot_y: 0,
                bgra: vec![0, 0, 0, 255],
            }),
            ControlMessage::Goodbye(Goodbye {
                reason: GoodbyeReason::Normal,
            }),
        ]
    }

    fn connected_pair(
        timeout: Option<Duration>,
    ) -> Result<(ControlConn, ControlConn), ControlError> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let client = thread::spawn(move || TcpStream::connect(address));
        let (server, _) = listener.accept()?;
        let client = client.join().map_err(|_| ControlError::Closed)??;
        Ok((
            ControlConn::from_stream(server, settings(timeout))?,
            ControlConn::from_stream(client, settings(timeout))?,
        ))
    }

    #[test]
    fn real_tcp_loopback_round_trips_all_sixteen_control_types() {
        let result = connected_pair(Some(Duration::from_secs(2)));
        assert!(result.is_ok());
        let (mut server, mut client) = match result {
            Ok(pair) => pair,
            Err(error) => panic!("loopback connect failed: {error}"),
        };
        assert!(matches!(client.nodelay(), Ok(true)));
        for message in messages() {
            let sent = client.send(&message);
            assert!(sent.is_ok());
            let received = server.recv();
            assert_eq!(received.ok(), Some(message));
        }
    }

    #[test]
    fn decoder_accepts_one_byte_at_a_time_writes() {
        let result = connected_pair(Some(Duration::from_secs(2)));
        assert!(result.is_ok());
        let (mut receiver, sender) = match result {
            Ok(pair) => pair,
            Err(error) => panic!("loopback connect failed: {error}"),
        };
        let writer = thread::spawn(move || {
            let mut sender = sender;
            let mut bytes = Vec::new();
            if let Err(error) = ControlMessage::PauseVideo(PauseVideo).encode_frame(&mut bytes) {
                return Err(error.to_string());
            }
            for byte in bytes {
                if let Err(error) = sender.stream.write_all(&[byte]) {
                    return Err(error.to_string());
                }
            }
            Ok::<(), String>(())
        });
        let received = receiver.recv();
        assert_eq!(received.ok(), Some(ControlMessage::PauseVideo(PauseVideo)));
        assert!(writer.join().is_ok());
    }

    #[test]
    fn oversized_prefix_is_rejected_before_a_body_is_buffered() {
        let result = connected_pair(Some(Duration::from_secs(1)));
        assert!(result.is_ok());
        let (mut receiver, mut sender) = match result {
            Ok(pair) => pair,
            Err(error) => panic!("loopback connect failed: {error}"),
        };
        let written = sender.stream.write_all(&(u32::MAX).to_le_bytes());
        assert!(written.is_ok());
        assert!(matches!(
            receiver.recv(),
            Err(ControlError::Proto(ProtoError::TooLarge))
        ));
        assert_eq!(receiver.decoder.buffered_len(), 0);
    }

    #[test]
    fn closing_mid_frame_returns_truncated_protocol_error() {
        let listener = TcpListener::bind("127.0.0.1:0");
        assert!(listener.is_ok());
        let listener = match listener {
            Ok(value) => value,
            Err(_) => return,
        };
        let address = listener.local_addr();
        assert!(address.is_ok());
        let address = match address {
            Ok(value) => value,
            Err(_) => return,
        };
        let client = thread::spawn(move || {
            let mut stream = TcpStream::connect(address);
            if let Ok(ref mut stream) = stream {
                let _ = stream.write_all(&[5, 0, 0, 0, 1]);
            }
            drop(stream);
        });
        let accepted = listener.accept();
        assert!(accepted.is_ok());
        let (stream, _) = match accepted {
            Ok(value) => value,
            Err(_) => return,
        };
        let mut receiver = ControlConn::from_stream(stream, settings(Some(Duration::from_secs(1))));
        assert!(receiver.is_ok());
        if let Ok(ref mut receiver) = receiver {
            assert!(matches!(
                receiver.recv(),
                Err(ControlError::Proto(ProtoError::Truncated))
            ));
        }
        let _ = client.join();
    }

    #[test]
    fn m2_control_defaults_match_the_transport_contract() {
        assert_eq!(DEFAULT_KEEPALIVE_IDLE, Duration::from_secs(10));
        assert_eq!(DEFAULT_CONTROL_WRITE_TIMEOUT, Duration::from_secs(2));
        assert_eq!(DEFAULT_CONTROL_READ_TIMEOUT, Duration::from_secs(5));
        let defaults = ControlSettings::default();
        assert_eq!(defaults.keepalive_idle, Duration::from_secs(10));
        assert_eq!(defaults.write_timeout, Duration::from_secs(2));
        assert_eq!(defaults.read_timeout, Some(Duration::from_secs(5)));
        assert_eq!(defaults.connect_timeout, Duration::from_secs(2));
    }

    #[test]
    fn read_timeout_is_typed_and_socket_options_are_applied() {
        let result = connected_pair(Some(Duration::from_millis(20)));
        assert!(result.is_ok());
        let (mut receiver, sender) = match result {
            Ok(pair) => pair,
            Err(error) => panic!("loopback connect failed: {error}"),
        };
        assert!(matches!(receiver.recv(), Err(ControlError::Timeout)));
        assert!(matches!(sender.nodelay(), Ok(true)));
    }
}

#[cfg(all(test, feature = "test-bind"))]
mod helper_tests {
    use super::*;
    use racc_proto::{ControlMessage, PauseVideo};
    use std::thread;

    fn settings(read_timeout: Option<Duration>) -> ControlSettings {
        ControlSettings {
            read_timeout,
            ..ControlSettings::default()
        }
    }

    #[test]
    fn listener_and_connect_helpers_validate_loopback_and_exchange_messages() {
        let listener = ControlListener::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            BindPolicy::TestOnlyLoopback,
            settings(Some(Duration::from_secs(2))),
        );
        assert!(listener.is_ok());
        let listener = match listener {
            Ok(value) => value,
            Err(error) => panic!("control listener failed: {error}"),
        };
        let address = listener.local_addr();
        assert!(address.is_ok());
        let address = match address {
            Ok(value) => value,
            Err(error) => panic!("listener address failed: {error}"),
        };
        let connector = thread::spawn(move || {
            connect_control(
                address,
                BindPolicy::TestOnlyLoopback,
                settings(Some(Duration::from_secs(2))),
            )
        });
        let accepted = listener.accept();
        assert!(accepted.is_ok());
        let (mut server, peer) = match accepted {
            Ok(value) => value,
            Err(error) => panic!("control accept failed: {error}"),
        };
        assert_eq!(
            peer.ip(),
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        );
        let client = connector.join();
        assert!(client.is_ok());
        let client = match client {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => panic!("control connect failed: {error}"),
            Err(_) => panic!("control connector thread failed"),
        };
        let split = client.split();
        assert!(split.is_ok());
        let (mut reader, mut writer) = match split {
            Ok(value) => value,
            Err(error) => panic!("control split failed: {error}"),
        };
        let writer_thread =
            thread::spawn(move || writer.send(&ControlMessage::PauseVideo(PauseVideo)));
        let received = server.recv();
        assert_eq!(received.ok(), Some(ControlMessage::PauseVideo(PauseVideo)));
        assert!(matches!(reader.recv(), Err(ControlError::Timeout)));
        assert!(writer_thread.join().is_ok());
    }
}
