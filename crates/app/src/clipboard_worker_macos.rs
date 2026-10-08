//! Nonblocking app-side bridge to the macOS text pasteboard.
//!
//! The worker starts dormant and polls only while the active session has explicitly
//! enabled clipboard sync. Its queues coalesce text and retain only content-free status.
#![cfg(target_os = "macos")]

use racc_clipboard::{
    macos::MacPasteboard,
    macos_poll::{MacPasteboardPoller, PasteboardPollError},
    ClipboardAdapterError, ClipboardErrorKind, ClipboardOperation, MAX_CLIPBOARD_BYTES,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const WAKE_CAPACITY: usize = 1;
const STATUS_CAPACITY: usize = 32;
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Non-content status from the platform clipboard worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClipboardWorkerStatus {
    /// Pasteboard polling is active for the enabled session.
    Enabled,
    /// The worker stopped polling because sync was disabled.
    Disabled,
    /// The pasteboard poller could not be started or recovered.
    ListenFailed(ClipboardAdapterError),
    /// The pasteboard changed but its text could not be read.
    LocalReadFailed(ClipboardAdapterError),
    /// The local text exceeded the protocol limit and was discarded.
    LocalTextTooLarge,
    /// The local pasteboard write completed.
    RemoteApplied { sequence: u64 },
    /// The local pasteboard write failed; the error contains no text.
    RemoteApplyFailed {
        sequence: u64,
        error: ClipboardAdapterError,
    },
}

/// Bounded items drained by the UI thread. `local_text` is the newest observed
/// local value; statuses contain metadata only and never include clipboard text.
pub(crate) struct ClipboardWorkerDrain {
    /// Latest UTF-8 text read from macOS, if any.
    pub(crate) local_text: Option<Vec<u8>>,
    /// Up to the requested number of recent content-free status items.
    pub(crate) statuses: Vec<ClipboardWorkerStatus>,
}

/// Result of attempting to queue remote text for local application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QueueRemoteResult {
    /// Accepted into the single latest-wins slot.
    Queued,
    /// Clipboard sync is disabled.
    Disabled,
    /// Payload exceeds the fixed clipboard limit.
    TooLarge,
    /// Payload is not UTF-8 text.
    InvalidUtf8,
    /// The worker is momentarily taking the slot; retry on a later UI tick.
    Busy,
    /// Shutdown has started or the worker exited.
    Stopped,
}

struct PendingApply {
    generation: u64,
    sequence: u64,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct LatestApplySlot {
    latest: Option<PendingApply>,
}

impl LatestApplySlot {
    fn replace(&mut self, pending: PendingApply) {
        self.latest = Some(pending);
    }

    fn take_current(&mut self, enabled: bool, generation: u64) -> Option<PendingApply> {
        let pending = self.latest.take()?;
        (enabled && pending.generation == generation).then_some(pending)
    }

    fn clear(&mut self) {
        self.latest = None;
    }
}

#[derive(Default)]
struct OutputBuffer {
    local_text: Option<Vec<u8>>,
    statuses: VecDeque<ClipboardWorkerStatus>,
}

impl OutputBuffer {
    fn set_local_text(&mut self, bytes: Vec<u8>) {
        self.local_text = Some(bytes);
    }

    fn clear_local_text(&mut self) {
        self.local_text = None;
    }

    fn push_status(&mut self, status: ClipboardWorkerStatus) {
        if self.statuses.len() == STATUS_CAPACITY {
            self.statuses.pop_front();
        }
        self.statuses.push_back(status);
    }

    fn drain(&mut self, max_statuses: usize) -> ClipboardWorkerDrain {
        let count = max_statuses.min(self.statuses.len());
        ClipboardWorkerDrain {
            local_text: self.local_text.take(),
            statuses: self.statuses.drain(..count).collect(),
        }
    }
}

struct Shared {
    enabled: AtomicBool,
    generation: AtomicU64,
    shutdown: AtomicBool,
    pending: Mutex<LatestApplySlot>,
    output: Mutex<OutputBuffer>,
}

/// macOS pasteboard polling/apply worker.
///
/// Create this only in the user's app session. It starts disabled and does not
/// read or apply pasteboard text until [`set_enabled`](Self::set_enabled) enables
/// the current authenticated viewer session.
pub(crate) struct MacClipboardWorker {
    shared: Arc<Shared>,
    wake: SyncSender<()>,
    thread: Option<JoinHandle<()>>,
}

impl MacClipboardWorker {
    /// Starts a dormant worker. No pasteboard operation occurs yet.
    pub(crate) fn start() -> std::io::Result<Self> {
        let (wake, wake_rx) = mpsc::sync_channel(WAKE_CAPACITY);
        let shared = Arc::new(Shared {
            enabled: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
            pending: Mutex::new(LatestApplySlot::default()),
            output: Mutex::new(OutputBuffer::default()),
        });
        let worker_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("racc-clipboard-worker".to_owned())
            .spawn(move || run_worker(worker_shared, wake_rx))?;
        Ok(Self {
            shared,
            wake,
            thread: Some(thread),
        })
    }

    /// Enables or disables future reads and writes without waiting for OS work.
    pub(crate) fn set_enabled(&self, enabled: bool) -> bool {
        if self.shared.shutdown.load(Ordering::Acquire) {
            return false;
        }
        let current = self.shared.enabled.load(Ordering::Acquire);
        if current != enabled {
            self.shared.generation.fetch_add(1, Ordering::AcqRel);
            self.shared.enabled.store(enabled, Ordering::Release);
            if !enabled {
                if let Ok(mut pending) = self.shared.pending.try_lock() {
                    pending.clear();
                }
                if let Ok(mut output) = self.shared.output.try_lock() {
                    output.clear_local_text();
                }
            }
            if !self.signal_worker() {
                return false;
            }
        }
        true
    }

    /// Queues remote text for local application, replacing any older pending text.
    pub(crate) fn queue_remote_text(&self, sequence: u64, bytes: Vec<u8>) -> QueueRemoteResult {
        if self.shared.shutdown.load(Ordering::Acquire) {
            return QueueRemoteResult::Stopped;
        }
        if bytes.len() > MAX_CLIPBOARD_BYTES {
            return QueueRemoteResult::TooLarge;
        }
        if std::str::from_utf8(&bytes).is_err() {
            return QueueRemoteResult::InvalidUtf8;
        }
        if !self.shared.enabled.load(Ordering::Acquire) {
            return QueueRemoteResult::Disabled;
        }
        let generation = self.shared.generation.load(Ordering::Acquire);
        let mut pending = match self.shared.pending.try_lock() {
            Ok(pending) => pending,
            Err(TryLockError::WouldBlock) => return QueueRemoteResult::Busy,
            Err(TryLockError::Poisoned(_)) => return QueueRemoteResult::Stopped,
        };
        if !self.shared.enabled.load(Ordering::Acquire)
            || self.shared.generation.load(Ordering::Acquire) != generation
        {
            return QueueRemoteResult::Disabled;
        }
        pending.replace(PendingApply {
            generation,
            sequence,
            bytes,
        });
        drop(pending);
        if self.signal_worker() {
            QueueRemoteResult::Queued
        } else {
            QueueRemoteResult::Stopped
        }
    }

    /// Takes the newest local text and up to `max_statuses` recent statuses.
    pub(crate) fn drain(&self, max_statuses: usize) -> Option<ClipboardWorkerDrain> {
        let mut output = match self.shared.output.try_lock() {
            Ok(output) => output,
            Err(TryLockError::WouldBlock) => return None,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
        };
        Some(output.drain(max_statuses))
    }

    /// Requests worker shutdown without waiting for a pasteboard operation.
    pub(crate) fn request_shutdown(&self) {
        if !self.shared.shutdown.swap(true, Ordering::AcqRel) {
            self.shared.enabled.store(false, Ordering::Release);
            self.shared.generation.fetch_add(1, Ordering::AcqRel);
            if let Ok(mut pending) = self.shared.pending.try_lock() {
                pending.clear();
            }
            if let Ok(mut output) = self.shared.output.try_lock() {
                output.clear_local_text();
            }
            let _ = self.signal_worker();
        }
    }

    /// Joins the worker after requesting shutdown.
    pub(crate) fn join(&mut self) -> thread::Result<()> {
        self.request_shutdown();
        match self.thread.take() {
            Some(thread) => thread.join(),
            None => Ok(()),
        }
    }

    fn signal_worker(&self) -> bool {
        match self.wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => true,
            Err(TrySendError::Disconnected(())) => false,
        }
    }
}

impl Drop for MacClipboardWorker {
    fn drop(&mut self) {
        self.request_shutdown();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_worker(shared: Arc<Shared>, wake: Receiver<()>) {
    let mut poller: Option<MacPasteboardPoller<MacPasteboard>> = None;
    let mut active_generation = u64::MAX;
    let started_at = Instant::now();

    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            break;
        }
        let enabled = shared.enabled.load(Ordering::Acquire);
        let generation = shared.generation.load(Ordering::Acquire);
        if active_generation != generation {
            if poller.take().is_some() && !enabled {
                push_status(&shared, ClipboardWorkerStatus::Disabled);
            }
            active_generation = generation;
            if enabled && is_current_session(&shared, generation) {
                let mut next_poller = MacPasteboardPoller::new(MacPasteboard::new());
                match next_poller.start_session(elapsed_ms(started_at)) {
                    Ok(()) if is_current_session(&shared, generation) => {
                        poller = Some(next_poller);
                        push_status(&shared, ClipboardWorkerStatus::Enabled);
                    }
                    Ok(()) => {}
                    Err(PasteboardPollError::Adapter(error)) => {
                        push_status(&shared, ClipboardWorkerStatus::ListenFailed(error));
                    }
                    Err(PasteboardPollError::TooLarge { .. }) => {
                        push_status(
                            &shared,
                            ClipboardWorkerStatus::ListenFailed(adapter_error(
                                ClipboardOperation::Listen,
                                0,
                            )),
                        );
                    }
                    Err(PasteboardPollError::InvalidUtf8) => {
                        push_status(
                            &shared,
                            ClipboardWorkerStatus::ListenFailed(adapter_error(
                                ClipboardOperation::Listen,
                                0,
                            )),
                        );
                    }
                }
            }
        }

        if let (Some(poller), Some(pending)) =
            (poller.as_mut(), take_pending(&shared, enabled, generation))
        {
            if is_current_session(&shared, pending.generation) {
                match poller.apply_remote(&pending.bytes) {
                    Ok(()) => push_status(
                        &shared,
                        ClipboardWorkerStatus::RemoteApplied {
                            sequence: pending.sequence,
                        },
                    ),
                    Err(PasteboardPollError::Adapter(error)) => push_status(
                        &shared,
                        ClipboardWorkerStatus::RemoteApplyFailed {
                            sequence: pending.sequence,
                            error,
                        },
                    ),
                    Err(PasteboardPollError::TooLarge { .. }) => push_status(
                        &shared,
                        ClipboardWorkerStatus::RemoteApplyFailed {
                            sequence: pending.sequence,
                            error: adapter_error(ClipboardOperation::Write, 0),
                        },
                    ),
                    Err(PasteboardPollError::InvalidUtf8) => push_status(
                        &shared,
                        ClipboardWorkerStatus::RemoteApplyFailed {
                            sequence: pending.sequence,
                            error: adapter_error(ClipboardOperation::Write, 0),
                        },
                    ),
                }
            }
        }

        if enabled && is_current_session(&shared, generation) {
            if let Some(active_poller) = poller.as_mut() {
                match active_poller.poll(elapsed_ms(started_at)) {
                    Ok(Some(bytes)) if is_current_session(&shared, generation) => {
                        if let Ok(mut output) = shared.output.lock() {
                            output.set_local_text(bytes);
                        }
                    }
                    Ok(Some(_)) | Ok(None) => {}
                    Err(PasteboardPollError::Adapter(error))
                        if error.kind == ClipboardErrorKind::TooLarge =>
                    {
                        push_status(&shared, ClipboardWorkerStatus::LocalTextTooLarge);
                    }
                    Err(PasteboardPollError::Adapter(error)) => {
                        push_status(&shared, ClipboardWorkerStatus::LocalReadFailed(error));
                    }
                    Err(PasteboardPollError::TooLarge { .. }) => {
                        push_status(&shared, ClipboardWorkerStatus::LocalTextTooLarge);
                    }
                    Err(PasteboardPollError::InvalidUtf8) => {
                        push_status(
                            &shared,
                            ClipboardWorkerStatus::LocalReadFailed(adapter_error(
                                ClipboardOperation::Read,
                                0,
                            )),
                        );
                    }
                }
            }
            let _ = wake.recv_timeout(POLL_INTERVAL);
        } else {
            let _ = wake.recv_timeout(POLL_INTERVAL);
        }
    }
    if poller.take().is_some() {
        push_status(&shared, ClipboardWorkerStatus::Disabled);
    }
}

fn elapsed_ms(started_at: Instant) -> u64 {
    started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn adapter_error(operation: ClipboardOperation, code: u32) -> ClipboardAdapterError {
    ClipboardAdapterError {
        operation,
        kind: racc_clipboard::ClipboardErrorKind::Platform(code),
    }
}

fn is_current_session(shared: &Shared, generation: u64) -> bool {
    !shared.shutdown.load(Ordering::Acquire)
        && shared.enabled.load(Ordering::Acquire)
        && shared.generation.load(Ordering::Acquire) == generation
}

fn take_pending(shared: &Shared, enabled: bool, generation: u64) -> Option<PendingApply> {
    let mut pending = shared
        .pending
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    pending.take_current(enabled, generation)
}

fn push_status(shared: &Shared, status: ClipboardWorkerStatus) {
    let mut output = shared
        .output
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    output.push_status(status);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(generation: u64, sequence: u64, bytes: &[u8]) -> PendingApply {
        PendingApply {
            generation,
            sequence,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn latest_apply_slot_replaces_old_text_and_takes_only_current_generation() {
        let mut slot = LatestApplySlot::default();
        slot.replace(pending(4, 1, b"old"));
        slot.replace(pending(4, 2, b"new"));
        let newest = slot.take_current(true, 4).expect("latest item");
        assert_eq!(newest.sequence, 2);
        assert_eq!(newest.bytes, b"new");
        assert!(slot.latest.is_none());
    }

    #[test]
    fn disabled_or_stale_generation_discards_pending_remote_text() {
        let mut slot = LatestApplySlot::default();
        slot.replace(pending(3, 8, b"stale"));
        assert!(slot.take_current(false, 3).is_none());
        slot.replace(pending(4, 9, b"old session"));
        assert!(slot.take_current(true, 5).is_none());
    }

    #[test]
    fn output_coalesces_local_text_and_bounds_status_history() {
        let mut output = OutputBuffer::default();
        output.set_local_text(b"first".to_vec());
        output.set_local_text(b"latest".to_vec());
        for _ in 0..(STATUS_CAPACITY + 4) {
            output.push_status(ClipboardWorkerStatus::Disabled);
        }
        let drained = output.drain(3);
        assert_eq!(drained.local_text.as_deref(), Some(&b"latest"[..]));
        assert_eq!(drained.statuses.len(), 3);
        assert_eq!(output.statuses.len(), STATUS_CAPACITY - 3);
        assert!(output.local_text.is_none());
    }

    #[test]
    fn session_generation_check_requires_enabled_and_matching_epoch() {
        let (wake, _receiver) = mpsc::sync_channel::<()>(1);
        let shared = Shared {
            enabled: AtomicBool::new(true),
            generation: AtomicU64::new(12),
            shutdown: AtomicBool::new(false),
            pending: Mutex::new(LatestApplySlot::default()),
            output: Mutex::new(OutputBuffer::default()),
        };
        assert!(is_current_session(&shared, 12));
        assert!(!is_current_session(&shared, 11));
        shared.enabled.store(false, Ordering::Release);
        assert!(!is_current_session(&shared, 12));
        drop(wake);
    }
}
