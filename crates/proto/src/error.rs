use core::fmt;

/// Errors produced while validating or decoding protocol data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtoError {
    /// The input ended before a complete field was available.
    Truncated,
    /// A length or value exceeded a protocol-defined bound.
    TooLarge,
    /// A field value is outside the protocol's allowed set.
    InvalidValue,
    /// A datagram kind byte is not recognized.
    UnknownKind,
    /// A control message type byte is not recognized.
    UnknownMessageType,
    /// The protocol version is not supported.
    UnsupportedVersion,
    /// A protocol string is not valid UTF-8.
    InvalidUtf8,
    /// A complete control message contains bytes after its payload.
    TrailingBytes,
}

impl fmt::Display for ProtoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Truncated => "truncated protocol data",
            Self::TooLarge => "protocol value exceeds a bound",
            Self::InvalidValue => "invalid protocol value",
            Self::UnknownKind => "unknown datagram kind",
            Self::UnknownMessageType => "unknown control message type",
            Self::UnsupportedVersion => "unsupported protocol version",
            Self::InvalidUtf8 => "invalid UTF-8",
            Self::TrailingBytes => "trailing bytes after protocol value",
        };
        formatter.write_str(text)
    }
}

impl std::error::Error for ProtoError {}

/// The result type used by protocol encoders and decoders.
pub type ProtoResult<T> = Result<T, ProtoError>;
