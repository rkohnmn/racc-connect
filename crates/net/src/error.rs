use crate::BindError;
use racc_proto::ProtoError;
use std::fmt;
use std::io;

/// Error returned by transport validation and socket wrappers.
#[derive(Debug)]
pub enum NetError {
    /// An operating-system socket operation failed.
    Io(io::Error),
    /// A bounded protocol decoder rejected input.
    Proto(ProtoError),
    /// The requested local or remote address violates the bind policy.
    Bind(BindError),
    /// A socket read or write timed out.
    Timeout,
    /// The peer closed the connection.
    Closed,
    /// A video frame is empty or exceeds the protocol fragment limit.
    InvalidFrameSize,
    /// Caller-provided datagram storage is too small.
    BufferCount,
    /// An allocation could not be reserved.
    AllocationFailed,
    /// The worker thread panicked.
    ThreadPanicked,
    /// A worker queue has been closed.
    QueueClosed,
    /// A frame was evicted before sending because the waiting queue was full.
    QueueDropped,
    /// Internal synchronization state was poisoned.
    Poisoned,
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "socket I/O error: {error}"),
            Self::Proto(error) => write!(f, "protocol error: {error}"),
            Self::Bind(error) => write!(f, "bind policy rejected address: {error}"),
            Self::Timeout => f.write_str("socket operation timed out"),
            Self::Closed => f.write_str("peer or worker closed"),
            Self::InvalidFrameSize => f.write_str("video frame size is outside protocol bounds"),
            Self::BufferCount => f.write_str("caller-provided datagram storage is too small"),
            Self::AllocationFailed => f.write_str("memory reservation failed"),
            Self::ThreadPanicked => f.write_str("transport worker thread panicked"),
            Self::QueueClosed => f.write_str("transport send queue is closed"),
            Self::QueueDropped => f.write_str("frame was evicted before transmission"),
            Self::Poisoned => f.write_str("transport synchronization state was poisoned"),
        }
    }
}

impl std::error::Error for NetError {}

impl From<io::Error> for NetError {
    fn from(error: io::Error) -> Self {
        if matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ) {
            Self::Timeout
        } else {
            Self::Io(error)
        }
    }
}

impl From<ProtoError> for NetError {
    fn from(error: ProtoError) -> Self {
        Self::Proto(error)
    }
}

impl From<BindError> for NetError {
    fn from(error: BindError) -> Self {
        Self::Bind(error)
    }
}
