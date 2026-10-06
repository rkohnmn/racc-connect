use crate::{ProtoError, ProtoResult, MAX_NAME_BYTES};

/// Bounds-checked reader for untrusted wire bytes.
#[derive(Debug)]
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    /// Creates a reader over a borrowed byte slice.
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    /// Returns the number of bytes consumed.
    pub(crate) fn position(&self) -> usize {
        self.position
    }

    /// Returns the number of bytes remaining.
    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    /// Reads a checked byte range.
    pub(crate) fn take(&mut self, length: usize) -> ProtoResult<&'a [u8]> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(ProtoError::TooLarge)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(ProtoError::Truncated)?;
        self.position = end;
        Ok(value)
    }

    pub(crate) fn u8(&mut self) -> ProtoResult<u8> {
        self.take(1)?.first().copied().ok_or(ProtoError::Truncated)
    }

    pub(crate) fn u16(&mut self) -> ProtoResult<u16> {
        let bytes = self.take(2)?;
        let mut array = [0; 2];
        array.copy_from_slice(bytes);
        Ok(u16::from_le_bytes(array))
    }

    pub(crate) fn i16(&mut self) -> ProtoResult<i16> {
        let bytes = self.take(2)?;
        let mut array = [0; 2];
        array.copy_from_slice(bytes);
        Ok(i16::from_le_bytes(array))
    }

    pub(crate) fn u32(&mut self) -> ProtoResult<u32> {
        let bytes = self.take(4)?;
        let mut array = [0; 4];
        array.copy_from_slice(bytes);
        Ok(u32::from_le_bytes(array))
    }

    pub(crate) fn i32(&mut self) -> ProtoResult<i32> {
        let bytes = self.take(4)?;
        let mut array = [0; 4];
        array.copy_from_slice(bytes);
        Ok(i32::from_le_bytes(array))
    }

    pub(crate) fn u64(&mut self) -> ProtoResult<u64> {
        let bytes = self.take(8)?;
        let mut array = [0; 8];
        array.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(array))
    }

    pub(crate) fn string(&mut self) -> ProtoResult<String> {
        let length = usize::from(self.u8()?);
        if length > MAX_NAME_BYTES {
            return Err(ProtoError::TooLarge);
        }
        let bytes = self.take(length)?;
        let value = core::str::from_utf8(bytes).map_err(|_| ProtoError::InvalidUtf8)?;
        Ok(value.to_owned())
    }

    pub(crate) fn finish(&self) -> ProtoResult<()> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(ProtoError::TrailingBytes)
        }
    }
}

/// Writes primitive values in the protocol's little-endian encoding.
#[derive(Debug, Default)]
pub(crate) struct Writer {
    pub(crate) bytes: Vec<u8>,
}

impl Writer {
    pub(crate) fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub(crate) fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn i16(&mut self, value: i16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn i32(&mut self, value: i32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn string(&mut self, value: &str) -> ProtoResult<()> {
        if value.len() > MAX_NAME_BYTES || value.len() > usize::from(u8::MAX) {
            return Err(ProtoError::TooLarge);
        }
        self.u8(value.len() as u8);
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }
}
