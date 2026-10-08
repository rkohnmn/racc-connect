//! Change-count polling policy used by the NSPasteboard adapter.
//!
//! The trait keeps native pasteboard calls outside the session/clipboard policy. It is portable
//! so polling cadence, size bounds, and remote-write echo suppression can be tested on any host.

use crate::MAX_CLIPBOARD_BYTES;

/// Minimum poll spacing used while a clipboard session is active.
pub const MACOS_PASTEBOARD_POLL_INTERVAL_MS: u64 = 250;

/// Bounded text-only operations required from a native pasteboard wrapper.
pub trait PasteboardText {
    /// Native API error type.
    type Error;

    /// Returns NSPasteboard's change count or the platform wrapper's equivalent.
    fn change_count(&mut self) -> Result<i64, Self::Error>;
    /// Reads UTF-8 text and must avoid allocating beyond `maximum_bytes`.
    fn read_text_bounded(&mut self, maximum_bytes: usize) -> Result<Option<Vec<u8>>, Self::Error>;
    /// Replaces the local pasteboard text with validated UTF-8 bytes.
    fn write_text(&mut self, text: &str) -> Result<(), Self::Error>;
}

/// Error returned by the pasteboard polling policy.
#[derive(Debug, PartialEq, Eq)]
pub enum PasteboardPollError<E> {
    /// Native pasteboard adapter failed.
    Adapter(E),
    /// Text exceeded the shared 512 KiB limit.
    TooLarge {
        /// Actual UTF-8 byte count.
        bytes: usize,
        /// Maximum accepted byte count.
        maximum: usize,
    },
    /// Text was not valid UTF-8.
    InvalidUtf8,
}

/// Polls a pasteboard at 4 Hz and suppresses the change count caused by remote writes.
pub struct MacPasteboardPoller<P: PasteboardText> {
    pasteboard: P,
    active: bool,
    last_change_count: Option<i64>,
    next_poll_ms: u64,
}

impl<P: PasteboardText> MacPasteboardPoller<P> {
    /// Creates an inactive poller; no pasteboard operation occurs during construction.
    pub fn new(pasteboard: P) -> Self {
        Self {
            pasteboard,
            active: false,
            last_change_count: None,
            next_poll_ms: 0,
        }
    }

    /// Starts polling and snapshots the current pasteboard state without reading its contents.
    pub fn start_session(&mut self, now_ms: u64) -> Result<(), PasteboardPollError<P::Error>> {
        self.last_change_count = Some(
            self.pasteboard
                .change_count()
                .map_err(PasteboardPollError::Adapter)?,
        );
        self.next_poll_ms = now_ms.saturating_add(MACOS_PASTEBOARD_POLL_INTERVAL_MS);
        self.active = true;
        Ok(())
    }

    /// Stops polling and releases the remembered change count.
    pub fn end_session(&mut self) {
        self.active = false;
        self.last_change_count = None;
    }

    /// Returns new local text after a change count advances and the 250 ms poll is due.
    pub fn poll(&mut self, now_ms: u64) -> Result<Option<Vec<u8>>, PasteboardPollError<P::Error>> {
        if !self.active || now_ms < self.next_poll_ms {
            return Ok(None);
        }
        self.next_poll_ms = now_ms.saturating_add(MACOS_PASTEBOARD_POLL_INTERVAL_MS);
        let count = self
            .pasteboard
            .change_count()
            .map_err(PasteboardPollError::Adapter)?;
        if self.last_change_count == Some(count) {
            return Ok(None);
        }
        self.last_change_count = Some(count);
        let text = self
            .pasteboard
            .read_text_bounded(MAX_CLIPBOARD_BYTES)
            .map_err(PasteboardPollError::Adapter)?;
        let Some(text) = text else {
            return Ok(None);
        };
        validate_text(&text)?;
        Ok(Some(text))
    }

    /// Applies an accepted remote text update and records the resulting native count as an echo.
    pub fn apply_remote(&mut self, bytes: &[u8]) -> Result<(), PasteboardPollError<P::Error>> {
        validate_text(bytes)?;
        let text = std::str::from_utf8(bytes).map_err(|_| PasteboardPollError::InvalidUtf8)?;
        self.pasteboard
            .write_text(text)
            .map_err(PasteboardPollError::Adapter)?;
        self.last_change_count = Some(
            self.pasteboard
                .change_count()
                .map_err(PasteboardPollError::Adapter)?,
        );
        Ok(())
    }

    /// Returns the wrapped platform adapter for the owner of this poller.
    pub fn into_inner(self) -> P {
        self.pasteboard
    }
}

fn validate_text<E>(bytes: &[u8]) -> Result<(), PasteboardPollError<E>> {
    if bytes.len() > MAX_CLIPBOARD_BYTES {
        return Err(PasteboardPollError::TooLarge {
            bytes: bytes.len(),
            maximum: MAX_CLIPBOARD_BYTES,
        });
    }
    std::str::from_utf8(bytes)
        .map(|_| ())
        .map_err(|_| PasteboardPollError::InvalidUtf8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct State {
        count: i64,
        bytes: Option<Vec<u8>>,
        reads: usize,
    }
    #[derive(Clone, Default)]
    struct FakePasteboard(Arc<Mutex<State>>);
    impl PasteboardText for FakePasteboard {
        type Error = ();
        fn change_count(&mut self) -> Result<i64, Self::Error> {
            Ok(self.0.lock().unwrap().count)
        }
        fn read_text_bounded(
            &mut self,
            maximum_bytes: usize,
        ) -> Result<Option<Vec<u8>>, Self::Error> {
            let mut state = self.0.lock().unwrap();
            state.reads += 1;
            Ok(state
                .bytes
                .clone()
                .filter(|bytes| bytes.len() <= maximum_bytes))
        }
        fn write_text(&mut self, text: &str) -> Result<(), Self::Error> {
            let mut state = self.0.lock().unwrap();
            state.count += 1;
            state.bytes = Some(text.as_bytes().to_vec());
            Ok(())
        }
    }

    #[test]
    fn polls_at_250_ms_and_reads_only_after_change_count_advances() {
        let pasteboard = FakePasteboard::default();
        let mut poller = MacPasteboardPoller::new(pasteboard.clone());
        poller.start_session(10).unwrap();
        assert_eq!(poller.poll(259).unwrap(), None);
        assert_eq!(pasteboard.0.lock().unwrap().reads, 0);
        assert_eq!(poller.poll(260).unwrap(), None);
        assert_eq!(pasteboard.0.lock().unwrap().reads, 0);
        {
            let mut state = pasteboard.0.lock().unwrap();
            state.count += 1;
            state.bytes = Some(b"local".to_vec());
        }
        assert_eq!(poller.poll(509).unwrap(), None);
        assert_eq!(poller.poll(510).unwrap(), Some(b"local".to_vec()));
        assert_eq!(pasteboard.0.lock().unwrap().reads, 1);
    }

    #[test]
    fn remote_write_count_is_suppressed_as_local_echo() {
        let pasteboard = FakePasteboard::default();
        let mut poller = MacPasteboardPoller::new(pasteboard.clone());
        poller.start_session(0).unwrap();
        poller.apply_remote(b"remote").unwrap();
        assert_eq!(
            pasteboard.0.lock().unwrap().bytes.as_deref(),
            Some(b"remote".as_slice())
        );
        assert_eq!(poller.poll(250).unwrap(), None);
        assert_eq!(pasteboard.0.lock().unwrap().reads, 0);
    }

    #[test]
    fn validates_size_and_utf8_without_logging_text() {
        let mut poller = MacPasteboardPoller::new(FakePasteboard::default());
        poller.start_session(0).unwrap();
        assert_eq!(
            poller.apply_remote(&[0xff]),
            Err(PasteboardPollError::InvalidUtf8)
        );
        assert!(matches!(
            poller.apply_remote(&vec![b'x'; MAX_CLIPBOARD_BYTES + 1]),
            Err(PasteboardPollError::TooLarge { .. })
        ));
    }
}
