use crate::codec::{Reader, Writer};
use crate::{
    ProtoError, ProtoResult, MAX_DATAGRAM, MAX_FRAGMENTS_PER_FRAME, MAX_VIDEO_PAYLOAD,
    PROTOCOL_VERSION, VIDEO_HEADER_LEN,
};

/// Video datagram flag marking an H.264 keyframe.
pub const VIDEO_FLAG_KEY: u8 = 1;
/// Video datagram flag marking the final fragment of a frame.
pub const VIDEO_FLAG_LAST_FRAGMENT: u8 = 2;
/// Video datagram flag indicating that the frame contains codec configuration.
pub const VIDEO_FLAG_CONFIG: u8 = 4;
const VIDEO_KNOWN_FLAGS: u8 = VIDEO_FLAG_KEY | VIDEO_FLAG_LAST_FRAGMENT | VIDEO_FLAG_CONFIG;
const VIDEO_KIND: u8 = 1;
const CURSOR_KIND: u8 = 2;
const CURSOR_DATAGRAM_LEN: usize = 19;

/// Metadata in the fixed 18-byte video-slice datagram header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoHeader {
    /// Protocol version.
    pub version: u8,
    /// Video flags; use the VIDEO_FLAG constants.
    pub flags: u8,
    /// Stream epoch. Receivers discard fragments from other epochs.
    pub epoch: u16,
    /// Monotonic frame identifier within the epoch.
    pub frame_id: u32,
    /// Zero-based fragment index.
    pub frag_idx: u16,
    /// Number of fragments in this frame.
    pub frag_cnt: u16,
    /// Host capture time in microseconds, wrapping at u32.
    pub capture_ts_us: u32,
}

/// Cursor position and visibility carried in a fixed-size UDP datagram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorUpdate {
    /// Stream epoch.
    pub epoch: u16,
    /// Identifier of the cursor shape sent over the control channel.
    pub shape_id: u32,
    /// Host physical x pixel relative to the streamed display's top-left.
    pub x: i32,
    /// Host physical y pixel relative to the streamed display's top-left.
    pub y: i32,
    /// Whether the cursor is visible.
    pub visible: bool,
}

/// A parsed UDP datagram; video payloads borrow the input buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoDatagram<'a> {
    /// One Annex B H.264 access-unit fragment.
    Video {
        /// Parsed fixed header.
        header: VideoHeader,
        /// Borrowed encoded fragment bytes.
        payload: &'a [u8],
    },
    /// Cursor metadata update.
    Cursor(CursorUpdate),
}

/// Encodes one video header and nonempty payload into a caller-owned buffer.
pub fn encode_video_datagram(
    header: VideoHeader,
    payload: &[u8],
    output: &mut Vec<u8>,
) -> ProtoResult<()> {
    validate_video_header(&header, payload.len())?;
    let total = VIDEO_HEADER_LEN
        .checked_add(payload.len())
        .ok_or(ProtoError::TooLarge)?;
    if total > MAX_DATAGRAM {
        return Err(ProtoError::TooLarge);
    }

    let mut writer = Writer::default();
    writer.u8(header.version);
    writer.u8(VIDEO_KIND);
    writer.u8(header.flags);
    writer.u8(0);
    writer.u16(header.epoch);
    writer.u32(header.frame_id);
    writer.u16(header.frag_idx);
    writer.u16(header.frag_cnt);
    writer.u32(header.capture_ts_us);
    output.extend_from_slice(&writer.bytes);
    output.extend_from_slice(payload);
    Ok(())
}

/// Parses a video or cursor UDP datagram without allocating.
pub fn parse_video_datagram(bytes: &[u8]) -> ProtoResult<VideoDatagram<'_>> {
    if bytes.len() > MAX_DATAGRAM {
        return Err(ProtoError::TooLarge);
    }
    let mut reader = Reader::new(bytes);
    let version = reader.u8()?;
    if version != PROTOCOL_VERSION {
        return Err(ProtoError::UnsupportedVersion);
    }
    let kind = reader.u8()?;
    match kind {
        VIDEO_KIND => {
            let flags = reader.u8()?;
            let reserved = reader.u8()?;
            if reserved != 0 {
                return Err(ProtoError::InvalidValue);
            }
            if flags & !VIDEO_KNOWN_FLAGS != 0 {
                return Err(ProtoError::InvalidValue);
            }
            let header = VideoHeader {
                version,
                flags,
                epoch: reader.u16()?,
                frame_id: reader.u32()?,
                frag_idx: reader.u16()?,
                frag_cnt: reader.u16()?,
                capture_ts_us: reader.u32()?,
            };
            let payload = reader.take(reader.remaining())?;
            validate_video_header(&header, payload.len())?;
            Ok(VideoDatagram::Video { header, payload })
        }
        CURSOR_KIND => {
            let update = parse_cursor_after_prefix(&mut reader)?;
            reader.finish()?;
            Ok(VideoDatagram::Cursor(update))
        }
        _ => Err(ProtoError::UnknownKind),
    }
}

/// Encodes one fixed-size cursor datagram into a caller-owned buffer.
pub fn encode_cursor_datagram(update: CursorUpdate, output: &mut Vec<u8>) {
    let mut writer = Writer::default();
    writer.u8(PROTOCOL_VERSION);
    writer.u8(CURSOR_KIND);
    writer.u8(0);
    writer.u8(0);
    writer.u16(update.epoch);
    writer.u32(update.shape_id);
    writer.i32(update.x);
    writer.i32(update.y);
    writer.u8(u8::from(update.visible));
    output.extend_from_slice(&writer.bytes);
}

/// Parses exactly one fixed-size cursor datagram.
pub fn parse_cursor_datagram(bytes: &[u8]) -> ProtoResult<CursorUpdate> {
    if bytes.len() < CURSOR_DATAGRAM_LEN {
        return Err(ProtoError::Truncated);
    }
    if bytes.len() > CURSOR_DATAGRAM_LEN {
        return Err(ProtoError::TrailingBytes);
    }
    let mut reader = Reader::new(bytes);
    let version = reader.u8()?;
    if version != PROTOCOL_VERSION {
        return Err(ProtoError::UnsupportedVersion);
    }
    if reader.u8()? != CURSOR_KIND {
        return Err(ProtoError::UnknownKind);
    }
    let update = parse_cursor_after_prefix(&mut reader)?;
    reader.finish()?;
    Ok(update)
}

fn parse_cursor_after_prefix(reader: &mut Reader<'_>) -> ProtoResult<CursorUpdate> {
    let flags = reader.u8()?;
    let reserved = reader.u8()?;
    if flags != 0 || reserved != 0 {
        return Err(ProtoError::InvalidValue);
    }
    let epoch = reader.u16()?;
    let shape_id = reader.u32()?;
    let x = reader.i32()?;
    let y = reader.i32()?;
    let visible = match reader.u8()? {
        0 => false,
        1 => true,
        _ => return Err(ProtoError::InvalidValue),
    };
    Ok(CursorUpdate {
        epoch,
        shape_id,
        x,
        y,
        visible,
    })
}

fn validate_video_header(header: &VideoHeader, payload_len: usize) -> ProtoResult<()> {
    if header.version != PROTOCOL_VERSION {
        return Err(ProtoError::UnsupportedVersion);
    }
    if header.flags & !VIDEO_KNOWN_FLAGS != 0 {
        return Err(ProtoError::InvalidValue);
    }
    if usize::from(header.frag_cnt) == 0
        || usize::from(header.frag_cnt) > MAX_FRAGMENTS_PER_FRAME
        || header.frag_idx >= header.frag_cnt
    {
        return Err(ProtoError::InvalidValue);
    }
    let is_last = header.flags & VIDEO_FLAG_LAST_FRAGMENT != 0;
    if is_last != (header.frag_idx.checked_add(1) == Some(header.frag_cnt)) {
        return Err(ProtoError::InvalidValue);
    }
    if payload_len == 0 || payload_len > MAX_VIDEO_PAYLOAD {
        return Err(if payload_len == 0 {
            ProtoError::InvalidValue
        } else {
            ProtoError::TooLarge
        });
    }
    Ok(())
}
