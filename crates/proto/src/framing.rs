use crate::messages::ControlMessage;
use crate::{ProtoError, ProtoResult, MAX_CONTROL_FRAME_BYTES};

/// Decodes one length-prefixed control frame and returns bytes consumed.
pub fn decode_control_frame(input: &[u8]) -> ProtoResult<(ControlMessage, usize)> {
    if input.len() < 4 {
        return Err(ProtoError::Truncated);
    }
    let mut prefix = [0; 4];
    prefix.copy_from_slice(input.get(..4).ok_or(ProtoError::Truncated)?);
    let body_length =
        usize::try_from(u32::from_le_bytes(prefix)).map_err(|_| ProtoError::TooLarge)?;
    if body_length == 0 {
        return Err(ProtoError::InvalidValue);
    }
    if body_length > MAX_CONTROL_FRAME_BYTES {
        return Err(ProtoError::TooLarge);
    }
    let total_length = body_length.checked_add(4).ok_or(ProtoError::TooLarge)?;
    let body = input.get(4..total_length).ok_or(ProtoError::Truncated)?;
    let message = ControlMessage::decode_body(body)?;
    Ok((message, total_length))
}

/// Incremental decoder for length-prefixed TCP control messages.
///
/// Its internal body buffer never grows beyond MAX_CONTROL_FRAME_BYTES. It
/// invokes the supplied callback for each complete message instead of
/// accumulating an unbounded batch of decoded messages.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    prefix: [u8; 4],
    prefix_len: usize,
    expected_body_len: Option<usize>,
    body: Vec<u8>,
}

impl FrameDecoder {
    /// Creates an empty decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds arbitrary bytes and calls the callback for each complete message.
    ///
    /// An invalid or oversized frame resets the partial frame state and returns
    /// the error immediately. Bytes after that malformed frame in this chunk
    /// are not processed.
    pub fn feed(
        &mut self,
        chunk: &[u8],
        mut on_message: impl FnMut(ControlMessage),
    ) -> ProtoResult<()> {
        let mut offset = 0usize;
        while offset < chunk.len() {
            if self.expected_body_len.is_none() {
                let needed = 4usize.saturating_sub(self.prefix_len);
                let available = chunk.len().saturating_sub(offset);
                let take = needed.min(available);
                let end = offset.checked_add(take).ok_or(ProtoError::TooLarge)?;
                let destination_end = self
                    .prefix_len
                    .checked_add(take)
                    .ok_or(ProtoError::TooLarge)?;
                let source = chunk.get(offset..end).ok_or(ProtoError::Truncated)?;
                let destination = self
                    .prefix
                    .get_mut(self.prefix_len..destination_end)
                    .ok_or(ProtoError::InvalidValue)?;
                destination.copy_from_slice(source);
                self.prefix_len = destination_end;
                offset = end;

                if self.prefix_len == self.prefix.len() {
                    let length = usize::try_from(u32::from_le_bytes(self.prefix))
                        .map_err(|_| ProtoError::TooLarge)?;
                    if length == 0 {
                        self.reset();
                        return Err(ProtoError::InvalidValue);
                    }
                    if length > MAX_CONTROL_FRAME_BYTES {
                        self.reset();
                        return Err(ProtoError::TooLarge);
                    }
                    self.expected_body_len = Some(length);
                    self.body.clear();
                    if self.body.capacity() < length {
                        self.body.reserve(length);
                    }
                }
            } else {
                let expected = self.expected_body_len.ok_or(ProtoError::InvalidValue)?;
                let needed = expected.saturating_sub(self.body.len());
                let available = chunk.len().saturating_sub(offset);
                let take = needed.min(available);
                let end = offset.checked_add(take).ok_or(ProtoError::TooLarge)?;
                let source = chunk.get(offset..end).ok_or(ProtoError::Truncated)?;
                self.body.extend_from_slice(source);
                offset = end;

                if self.body.len() == expected {
                    match ControlMessage::decode_body(&self.body) {
                        Ok(message) => on_message(message),
                        Err(error) => {
                            self.reset();
                            return Err(error);
                        }
                    }
                    self.reset();
                }
            }
        }
        Ok(())
    }

    /// Returns the number of prefix and body bytes currently buffered.
    pub fn buffered_len(&self) -> usize {
        self.prefix_len.saturating_add(self.body.len())
    }

    /// Returns the current internal buffer capacity in bytes, including prefix.
    pub fn buffer_capacity(&self) -> usize {
        self.body.capacity().saturating_add(self.prefix.len())
    }

    fn reset(&mut self) {
        self.prefix = [0; 4];
        self.prefix_len = 0;
        self.expected_body_len = None;
        self.body.clear();
    }
}
