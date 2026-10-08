//! Cursor metadata forwarding stays separate from encoded video frames.
use crate::control_server::{ControlSendError, ControlSendHandle};
use racc_capture::{
    CursorBlendMode as CaptureBlendMode, CursorPosition, CursorShape as CaptureShape,
};
use racc_core::HostConnectionId;
use racc_net::{validate_bind_addr, BindPolicy};
use racc_proto::{
    encode_cursor_datagram, ControlMessage, CursorBlendMode, CursorShape, CursorUpdate,
    MAX_CURSOR_BYTES, MAX_CURSOR_DIM,
};
use racc_topology::DisplayId;
use std::fmt;
use std::io::{self, ErrorKind};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::Duration;

const SHAPE_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const CURSOR_DATAGRAM_LEN: usize = 19;

#[derive(Debug)]
pub enum CursorTransportError {
    Control(ControlSendError),
    Io(io::Error),
    ShortDatagram,
}
impl fmt::Display for CursorTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Control(error) => write!(formatter, "cursor control write failed: {error}"),
            Self::Io(error) => write!(formatter, "cursor UDP write failed: {error}"),
            Self::ShortDatagram => formatter.write_str("cursor UDP write was short"),
        }
    }
}
impl std::error::Error for CursorTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Control(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::ShortDatagram => None,
        }
    }
}
pub trait CursorTransport {
    fn send_shape_confirmed(
        &mut self,
        connection_id: HostConnectionId,
        shape: CursorShape,
    ) -> Result<(), CursorTransportError>;
    fn send_cursor_datagram(&mut self, update: CursorUpdate) -> Result<bool, CursorTransportError>;
}

/// Production shape-control and nonblocking cursor-UDP transport.
pub struct CursorNetworkTransport {
    control: ControlSendHandle,
    socket: UdpSocket,
    datagram: Vec<u8>,
}
impl CursorNetworkTransport {
    pub fn bind(
        local_ip: IpAddr,
        target: SocketAddr,
        control: ControlSendHandle,
    ) -> io::Result<Self> {
        validate_bind_addr(local_ip, BindPolicy::Tailscale).map_err(io::Error::other)?;
        validate_bind_addr(target.ip(), BindPolicy::Tailscale).map_err(io::Error::other)?;
        if target.port() == 0 {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "cursor target port must be nonzero",
            ));
        }
        let socket = UdpSocket::bind(SocketAddr::new(local_ip, 0))?;
        socket.set_nonblocking(true)?;
        socket.connect(target)?;
        Ok(Self {
            control,
            socket,
            datagram: Vec::with_capacity(CURSOR_DATAGRAM_LEN),
        })
    }
}
impl CursorTransport for CursorNetworkTransport {
    fn send_shape_confirmed(
        &mut self,
        connection_id: HostConnectionId,
        shape: CursorShape,
    ) -> Result<(), CursorTransportError> {
        self.control
            .send_confirmed(
                connection_id,
                ControlMessage::CursorShape(shape),
                SHAPE_WRITE_TIMEOUT,
            )
            .map_err(CursorTransportError::Control)
    }
    fn send_cursor_datagram(&mut self, update: CursorUpdate) -> Result<bool, CursorTransportError> {
        self.datagram.clear();
        encode_cursor_datagram(update, &mut self.datagram);
        match self.socket.send(&self.datagram) {
            Ok(n) if n == CURSOR_DATAGRAM_LEN => Ok(true),
            Ok(_) => Err(CursorTransportError::ShortDatagram),
            Err(e) if e.kind() == ErrorKind::WouldBlock => Ok(false),
            Err(e) => Err(CursorTransportError::Io(e)),
        }
    }
}

#[derive(Clone, Copy)]
struct CursorSession {
    connection_id: HostConnectionId,
    epoch: u16,
    display_id: DisplayId,
    origin: (i32, i32),
}

/// Validates shapes and scopes cursor metadata to one authenticated stream epoch/display.
#[derive(Default)]
pub struct HostCursorDispatcher {
    last_shape_id: u32,
    current_shape: Option<CursorShape>,
    active: Option<CursorSession>,
    announced: Option<(HostConnectionId, u16)>,
}
impl HostCursorDispatcher {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn deactivate(&mut self) {
        self.active = None;
        self.announced = None;
    }
    /// Starts a confirmed stream epoch and writes any cached shape before enabling UDP updates.
    pub fn activate<T: CursorTransport + ?Sized>(
        &mut self,
        connection_id: HostConnectionId,
        epoch: u16,
        display_id: DisplayId,
        origin: (i32, i32),
        transport: &mut T,
    ) -> Result<(), CursorTransportError> {
        self.active = Some(CursorSession {
            connection_id,
            epoch,
            display_id,
            origin,
        });
        self.announced = None;
        self.announce_cached(transport)
    }
    /// Caches a bounded OS shape and sends it over control when the stream is active.
    pub fn shape_changed<T: CursorTransport + ?Sized>(
        &mut self,
        shape: CaptureShape,
        transport: Option<&mut T>,
    ) -> Result<(), CursorShapeError> {
        let id = self
            .last_shape_id
            .checked_add(1)
            .ok_or(CursorShapeError::ShapeIdsExhausted)?;
        let shape = convert_shape(shape, id)?;
        self.last_shape_id = id;
        self.current_shape = Some(shape);
        self.announced = None;
        if let (Some(session), Some(transport)) = (self.active, transport) {
            self.announce(session, transport)
                .map_err(CursorShapeError::Transport)?;
        }
        Ok(())
    }
    /// Sends one position only if connection, epoch, display, and shape announcement match.
    pub fn position<T: CursorTransport + ?Sized>(
        &mut self,
        connection_id: HostConnectionId,
        epoch: u16,
        display_id: DisplayId,
        position: CursorPosition,
        transport: &mut T,
    ) -> Result<CursorDispatch, CursorTransportError> {
        let Some(session) = self.active.filter(|s| {
            s.connection_id == connection_id && s.epoch == epoch && s.display_id == display_id
        }) else {
            return Ok(CursorDispatch::Suppressed);
        };
        let Some(shape) = self
            .current_shape
            .as_ref()
            .filter(|_| self.announced == Some((connection_id, epoch)))
        else {
            return Ok(CursorDispatch::Suppressed);
        };
        let update = CursorUpdate {
            epoch,
            shape_id: shape.shape_id,
            x: position.x.saturating_sub(session.origin.0),
            y: position.y.saturating_sub(session.origin.1),
            visible: position.visible,
        };
        if transport.send_cursor_datagram(update)? {
            Ok(CursorDispatch::Sent(update))
        } else {
            Ok(CursorDispatch::DroppedWouldBlock)
        }
    }
    fn announce_cached<T: CursorTransport + ?Sized>(
        &mut self,
        transport: &mut T,
    ) -> Result<(), CursorTransportError> {
        if let (Some(session), Some(_)) = (self.active, self.current_shape.as_ref()) {
            self.announce(session, transport)?;
        }
        Ok(())
    }
    fn announce<T: CursorTransport + ?Sized>(
        &mut self,
        session: CursorSession,
        transport: &mut T,
    ) -> Result<(), CursorTransportError> {
        let Some(shape) = self.current_shape.as_ref() else {
            return Ok(());
        };
        transport.send_shape_confirmed(session.connection_id, shape.clone())?;
        self.announced = Some((session.connection_id, session.epoch));
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorDispatch {
    Sent(CursorUpdate),
    DroppedWouldBlock,
    Suppressed,
}
#[derive(Debug)]
pub enum CursorShapeError {
    Dimensions,
    PayloadLength,
    PixelSemantics,
    Hotspot,
    ShapeIdsExhausted,
    Transport(CursorTransportError),
}
impl fmt::Display for CursorShapeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dimensions => {
                formatter.write_str("cursor dimensions are outside protocol bounds")
            }
            Self::PayloadLength => formatter.write_str("cursor BGRA payload length is invalid"),
            Self::PixelSemantics => {
                formatter.write_str("cursor pixels do not match their blend mode")
            }
            Self::Hotspot => formatter.write_str("cursor hotspot is outside the shape"),
            Self::ShapeIdsExhausted => formatter.write_str("cursor shape IDs are exhausted"),
            Self::Transport(error) => write!(formatter, "{error}"),
        }
    }
}
impl std::error::Error for CursorShapeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            _ => None,
        }
    }
}
fn convert_shape(capture: CaptureShape, shape_id: u32) -> Result<CursorShape, CursorShapeError> {
    let width = usize::try_from(capture.width).map_err(|_| CursorShapeError::Dimensions)?;
    let height = usize::try_from(capture.height).map_err(|_| CursorShapeError::Dimensions)?;
    if width == 0 || height == 0 || width > MAX_CURSOR_DIM || height > MAX_CURSOR_DIM {
        return Err(CursorShapeError::Dimensions);
    }
    if capture.hotspot_x >= capture.width || capture.hotspot_y >= capture.height {
        return Err(CursorShapeError::Hotspot);
    }
    let expected = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(4))
        .filter(|n| *n <= MAX_CURSOR_BYTES)
        .ok_or(CursorShapeError::PayloadLength)?;
    if capture.bgra8.len() != expected {
        return Err(CursorShapeError::PayloadLength);
    }
    if !valid_pixels(capture.blend_mode, &capture.bgra8) {
        return Err(CursorShapeError::PixelSemantics);
    }
    let blend_mode = match capture.blend_mode {
        CaptureBlendMode::PremultipliedAlpha => CursorBlendMode::PremultipliedAlpha,
        CaptureBlendMode::WindowsMaskedColor => CursorBlendMode::WindowsMaskedColor,
        CaptureBlendMode::WindowsAndXor => CursorBlendMode::WindowsAndXor,
    };
    Ok(CursorShape {
        shape_id,
        width: u16::try_from(capture.width).map_err(|_| CursorShapeError::Dimensions)?,
        height: u16::try_from(capture.height).map_err(|_| CursorShapeError::Dimensions)?,
        hotspot_x: u16::try_from(capture.hotspot_x).map_err(|_| CursorShapeError::Hotspot)?,
        hotspot_y: u16::try_from(capture.hotspot_y).map_err(|_| CursorShapeError::Hotspot)?,
        bgra: capture.bgra8.as_ref().to_vec(),
        blend_mode,
    })
}

fn valid_pixels(mode: CaptureBlendMode, bgra: &[u8]) -> bool {
    bgra.chunks_exact(4).all(|pixel| match pixel {
        [blue, green, red, alpha] => match mode {
            CaptureBlendMode::PremultipliedAlpha => blue <= alpha && green <= alpha && red <= alpha,
            CaptureBlendMode::WindowsMaskedColor => *alpha == 0 || *alpha == u8::MAX,
            CaptureBlendMode::WindowsAndXor => {
                (*alpha == 0 || *alpha == u8::MAX)
                    && (*blue == 0 || *blue == u8::MAX)
                    && blue == green
                    && green == red
            }
        },
        _ => false,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use racc_capture::CursorBlendMode as CaptureBlendMode;

    #[derive(Default)]
    struct FakeTransport {
        shapes: Vec<(HostConnectionId, CursorShape)>,
        updates: Vec<CursorUpdate>,
        fail_shape: bool,
        drop_updates: bool,
    }
    impl CursorTransport for FakeTransport {
        fn send_shape_confirmed(
            &mut self,
            id: HostConnectionId,
            shape: CursorShape,
        ) -> Result<(), CursorTransportError> {
            if self.fail_shape {
                return Err(CursorTransportError::Control(ControlSendError::QueueFull));
            }
            self.shapes.push((id, shape));
            Ok(())
        }
        fn send_cursor_datagram(
            &mut self,
            update: CursorUpdate,
        ) -> Result<bool, CursorTransportError> {
            if self.drop_updates {
                return Ok(false);
            }
            self.updates.push(update);
            Ok(true)
        }
    }
    fn shape(mode: CaptureBlendMode) -> CaptureShape {
        let bgra8 = match mode {
            CaptureBlendMode::PremultipliedAlpha => vec![
                16, 24, 32, 128, 16, 24, 32, 128, 16, 24, 32, 128, 16, 24, 32, 128,
            ],
            CaptureBlendMode::WindowsMaskedColor => vec![
                10, 20, 30, 0, 10, 20, 30, 255, 40, 50, 60, 0, 40, 50, 60, 255,
            ],
            CaptureBlendMode::WindowsAndXor => vec![
                255, 255, 255, 0, 0, 0, 0, 255, 0, 0, 0, 0, 255, 255, 255, 255,
            ],
        };
        CaptureShape {
            width: 2,
            height: 2,
            hotspot_x: 1,
            hotspot_y: 0,
            bgra8: bgra8.into(),
            blend_mode: mode,
        }
    }
    fn display(id: u32) -> DisplayId {
        DisplayId::new(id).expect("valid test display")
    }

    #[test]
    fn shape_precedes_udp_and_keeps_bgra_hotspot_and_blend_mode() {
        let mut d = HostCursorDispatcher::new();
        let mut t = FakeTransport::default();
        d.shape_changed(
            shape(CaptureBlendMode::WindowsAndXor),
            None::<&mut FakeTransport>,
        )
        .expect("shape valid");
        d.activate(4, 9, display(7), (-1920, 80), &mut t)
            .expect("cached shape announced");
        let sent = d
            .position(
                4,
                9,
                display(7),
                CursorPosition {
                    x: -1900,
                    y: 125,
                    visible: true,
                },
                &mut t,
            )
            .expect("UDP send");
        assert_eq!(t.shapes.len(), 1);
        assert_eq!(t.shapes[0].1.blend_mode, CursorBlendMode::WindowsAndXor);
        assert_eq!(
            t.shapes[0].1.bgra.as_slice(),
            shape(CaptureBlendMode::WindowsAndXor).bgra8.as_ref()
        );
        assert_eq!((t.shapes[0].1.hotspot_x, t.shapes[0].1.hotspot_y), (1, 0));
        assert_eq!(
            sent,
            CursorDispatch::Sent(CursorUpdate {
                epoch: 9,
                shape_id: t.shapes[0].1.shape_id,
                x: 20,
                y: 45,
                visible: true
            })
        );
        assert_eq!(t.updates.len(), 1);
    }
    #[test]
    fn stale_connection_epoch_display_and_deactivated_session_are_suppressed() {
        let mut d = HostCursorDispatcher::new();
        let mut t = FakeTransport::default();
        d.shape_changed(
            shape(CaptureBlendMode::PremultipliedAlpha),
            None::<&mut FakeTransport>,
        )
        .expect("shape valid");
        d.activate(4, 9, display(7), (0, 0), &mut t)
            .expect("shape announced");
        let p = CursorPosition {
            x: 5,
            y: 6,
            visible: true,
        };
        for args in [(5, 9, 7), (4, 8, 7), (4, 9, 8)] {
            assert!(matches!(
                d.position(args.0, args.1, display(args.2), p, &mut t),
                Ok(CursorDispatch::Suppressed)
            ));
        }
        d.deactivate();
        assert!(matches!(
            d.position(4, 9, display(7), p, &mut t),
            Ok(CursorDispatch::Suppressed)
        ));
        assert!(t.updates.is_empty());
    }
    #[test]
    fn invalid_dimension_payload_and_hotspot_are_rejected() {
        let mut d = HostCursorDispatcher::new();
        let mut s = shape(CaptureBlendMode::WindowsMaskedColor);
        s.width = 0;
        assert!(matches!(
            d.shape_changed(s, None::<&mut FakeTransport>),
            Err(CursorShapeError::Dimensions)
        ));
        let mut s = shape(CaptureBlendMode::PremultipliedAlpha);
        s.width = u32::try_from(MAX_CURSOR_DIM + 1).expect("fits");
        assert!(matches!(
            d.shape_changed(s, None::<&mut FakeTransport>),
            Err(CursorShapeError::Dimensions)
        ));
        let mut s = shape(CaptureBlendMode::PremultipliedAlpha);
        s.hotspot_x = s.width;
        assert!(matches!(
            d.shape_changed(s, None::<&mut FakeTransport>),
            Err(CursorShapeError::Hotspot)
        ));
        let mut invalid = shape(CaptureBlendMode::PremultipliedAlpha);
        invalid.bgra8 = vec![255, 0, 0, 1, 255, 0, 0, 1, 255, 0, 0, 1, 255, 0, 0, 1].into();
        assert!(matches!(
            d.shape_changed(invalid, None::<&mut FakeTransport>),
            Err(CursorShapeError::PixelSemantics)
        ));
        let mut s = shape(CaptureBlendMode::PremultipliedAlpha);
        s.bgra8 = vec![0; 15].into();
        assert!(matches!(
            d.shape_changed(s, None::<&mut FakeTransport>),
            Err(CursorShapeError::PayloadLength)
        ));
    }
    #[test]
    fn shape_write_failure_gates_udp_until_new_epoch_retry() {
        let mut d = HostCursorDispatcher::new();
        let mut t = FakeTransport {
            fail_shape: true,
            ..FakeTransport::default()
        };
        d.shape_changed(
            shape(CaptureBlendMode::PremultipliedAlpha),
            None::<&mut FakeTransport>,
        )
        .expect("shape valid");
        assert!(matches!(
            d.activate(1, 2, display(7), (0, 0), &mut t),
            Err(CursorTransportError::Control(ControlSendError::QueueFull))
        ));
        assert!(matches!(
            d.position(
                1,
                2,
                display(7),
                CursorPosition {
                    x: 1,
                    y: 1,
                    visible: true
                },
                &mut t
            ),
            Ok(CursorDispatch::Suppressed)
        ));
        t.fail_shape = false;
        d.activate(1, 3, display(7), (0, 0), &mut t)
            .expect("retry at reset");
        assert_eq!(t.shapes.len(), 1);
    }
    #[test]
    fn socket_backpressure_drops_latest_update_without_queueing() {
        let mut d = HostCursorDispatcher::new();
        let mut t = FakeTransport {
            drop_updates: true,
            ..FakeTransport::default()
        };
        d.shape_changed(
            shape(CaptureBlendMode::PremultipliedAlpha),
            None::<&mut FakeTransport>,
        )
        .expect("shape valid");
        d.activate(1, 2, display(7), (0, 0), &mut t)
            .expect("shape announced");
        assert!(matches!(
            d.position(
                1,
                2,
                display(7),
                CursorPosition {
                    x: 9,
                    y: 10,
                    visible: true
                },
                &mut t
            ),
            Ok(CursorDispatch::DroppedWouldBlock)
        ));
        assert!(t.updates.is_empty());
    }
}
