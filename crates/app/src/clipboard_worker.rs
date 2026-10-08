//! Nonblocking app-side bridge to the Windows text clipboard.
//!
//! The worker owns the OS listener and clipboard reads/writes. UI calls only
//! update small bounded queues and can drain content-free status metadata.
#![cfg(target_os = "windows")]

use racc_clipboard::{
    windows::{ClipboardListener, WindowsClipboard},
    ClipboardAdapterError, ClipboardOperation, MAX_CLIPBOARD_BYTES,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const WAKE_CAPACITY: usize = 1;
const STATUS_CAPACITY: usize = 32;
const WORKER_WAIT: Duration = Duration::from_millis(40);
const LISTENER_RETRY_DELAY: Duration = Duration::from_secs(1);

/// Non-content status from the platform clipboard worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClipboardWorkerStatus {
    /// A change listener is active for the enabled session.
    Enabled,
    /// The worker stopped listening because sync was disabled.
    Disabled,
    /// Windows could not start or recover its clipboard listener.
    ListenFailed(ClipboardAdapterError),
    /// The listener received a change but could not read Unicode text.
    LocalReadFailed(ClipboardAdapterError),
    /// The local text exceeded the protocol limit and was discarded.
    LocalTextTooLarge,
    /// The local clipboard write completed.
    RemoteApplied { sequence: u64 },
    /// The local clipboard write failed; the error contains no text.
    RemoteApplyFailed {
        sequence: u64,
        error: ClipboardAdapterError,
    },
}

/// Bounded items drained by the UI thread. `local_text` is the newest observed
/// local value; statuses contain metadata only and never include clipboard text.
pub(crate) struct ClipboardWorkerDrain {
    /// Latest UTF-8 text read from Windows, if any.
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

/// Windows clipboard listener/apply worker.
///
/// Create this only for a user session that supports clipboard sync. It starts
/// disabled and never reads the clipboard until [`set_enabled`](Self::set_enabled)
/// enables it. Incoming remote text is replaced in a one-item latest-wins slot.
pub(crate) struct WindowsClipboardWorker {
    shared: Arc<Shared>,
    wake: SyncSender<()>,
    thread: Option<JoinHandle<()>>,
}

impl WindowsClipboardWorker {
    /// Starts a dormant worker. No clipboard listener or OS read occurs yet.
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
    ///
    /// Returns `false` after shutdown has started. Disabling advances the session
    /// generation so any queued value from an earlier enable period is discarded.
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
    ///
    /// The byte count and UTF-8 are checked before storage. A successful call does
    /// not wait for the Windows clipboard; the worker rechecks the enabled session
    /// immediately before attempting the write.
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

    /// Takes the latest local text and up to `max_statuses` recent statuses.
    ///
    /// Returns `None` only if the worker is briefly updating the output buffer;
    /// callers can retry on their next regular UI tick.
    pub(crate) fn drain(&self, max_statuses: usize) -> Option<ClipboardWorkerDrain> {
        let mut output = match self.shared.output.try_lock() {
            Ok(output) => output,
            Err(TryLockError::WouldBlock) => return None,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
        };
        Some(output.drain(max_statuses))
    }

    /// Requests worker shutdown without waiting for clipboard operations to finish.
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

    /// Joins the worker. Call from app shutdown after [`request_shutdown`](Self::request_shutdown).
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

impl Drop for WindowsClipboardWorker {
    fn drop(&mut self) {
        self.request_shutdown();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_worker(shared: Arc<Shared>, wake: Receiver<()>) {
    let clipboard = WindowsClipboard::new();
    let mut listener: Option<ClipboardListener> = None;
    let mut listener_generation = 0;
    let mut retry_listener_at = Instant::now();

    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            break;
        }
        let enabled = shared.enabled.load(Ordering::Acquire);
        let generation = shared.generation.load(Ordering::Acquire);

        if listener_generation != generation {
            let was_listening = listener.take().is_some();
            listener_generation = generation;
            retry_listener_at = Instant::now();
            if was_listening && !enabled {
                push_status(&shared, ClipboardWorkerStatus::Disabled);
            }
        }
        if !enabled && listener.take().is_some() {
            push_status(&shared, ClipboardWorkerStatus::Disabled);
        }
        if enabled && listener.is_none() && Instant::now() >= retry_listener_at {
            match clipboard.start_listener() {
                Ok(started) => {
                    listener = Some(started);
                    push_status(&shared, ClipboardWorkerStatus::Enabled);
                }
                Err(error) => {
                    push_status(&shared, ClipboardWorkerStatus::ListenFailed(error));
                    retry_listener_at = Instant::now() + LISTENER_RETRY_DELAY;
                }
            }
        }

        if let Some(pending) = take_pending(&shared, enabled, generation) {
            if is_current_session(&shared, pending.generation) {
                match clipboard.write_text(&pending.bytes) {
                    Ok(()) => push_status(
                        &shared,
                        ClipboardWorkerStatus::RemoteApplied {
                            sequence: pending.sequence,
                        },
                    ),
                    Err(error) => push_status(
                        &shared,
                        ClipboardWorkerStatus::RemoteApplyFailed {
                            sequence: pending.sequence,
                            error,
                        },
                    ),
                }
            }
        }

        if enabled && is_current_session(&shared, generation) {
            if let Some(active_listener) = &listener {
                match active_listener.notifications().recv_timeout(WORKER_WAIT) {
                    Ok(()) if is_current_session(&shared, generation) => {
                        match clipboard.read_text() {
                            Ok(Some(bytes)) if bytes.len() <= MAX_CLIPBOARD_BYTES => {
                                if is_current_session(&shared, generation) {
                                    if let Ok(mut output) = shared.output.lock() {
                                        output.set_local_text(bytes);
                                    }
                                }
                            }
                            Ok(Some(_)) => {
                                push_status(&shared, ClipboardWorkerStatus::LocalTextTooLarge);
                            }
                            Ok(None) => {}
                            Err(error) => {
                                push_status(&shared, ClipboardWorkerStatus::LocalReadFailed(error))
                            }
                        }
                    }
                    Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        listener.take();
                        retry_listener_at = Instant::now() + LISTENER_RETRY_DELAY;
                        push_status(
                            &shared,
                            ClipboardWorkerStatus::ListenFailed(ClipboardAdapterError {
                                operation: ClipboardOperation::Listen,
                                kind: racc_clipboard::ClipboardErrorKind::Platform(0),
                            }),
                        );
                    }
                }
            } else {
                let until_retry = retry_listener_at.saturating_duration_since(Instant::now());
                let wait = WORKER_WAIT.min(until_retry.max(Duration::from_millis(1)));
                let _ = wake.recv_timeout(wait);
            }
        } else {
            let _ = wake.recv_timeout(WORKER_WAIT);
        }
    }
    if listener.take().is_some() {
        push_status(&shared, ClipboardWorkerStatus::Disabled);
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
    #[ignore = "writes two known non-private strings to the current Windows clipboard and leaves the last one there"]
    fn windows_real_clipboard_round_trip_with_fake_remote() {
        use racc_clipboard::{
            ClipboardDirections, ClipboardSync, LocalChangeResult, OriginId, RemoteChangeResult,
        };

        const LOCAL_TEXT: &[u8] = "Racc clipboard check — local café".as_bytes();
        const REMOTE_TEXT: &[u8] = "Racc clipboard check — remote naïve".as_bytes();
        let clipboard = WindowsClipboard::new();
        // Seed a known non-private value before enabling the listener. The test never reads or
        // saves whatever text was on the clipboard before it began.
        clipboard
            .write_text(b"Racc clipboard check - seed")
            .expect("write known clipboard seed");

        let worker = WindowsClipboardWorker::start().expect("start clipboard worker");
        assert!(worker.set_enabled(true));
        let mut enabled = false;
        for _ in 0..500 {
            let drain = worker.drain(STATUS_CAPACITY).expect("drain worker");
            for status in drain.statuses {
                match status {
                    ClipboardWorkerStatus::Enabled => enabled = true,
                    ClipboardWorkerStatus::ListenFailed(error) => {
                        panic!("Windows clipboard listener failed: {error:?}");
                    }
                    _ => {}
                }
            }
            if enabled {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(enabled, "clipboard listener did not start");

        clipboard
            .write_text(LOCAL_TEXT)
            .expect("write known local clipboard test text");
        let mut sync = ClipboardSync::new(OriginId(1));
        sync.start_session(OriginId(2), ClipboardDirections::both())
            .expect("start fake remote session");
        let mut local_observed = false;
        for _ in 0..500 {
            let drain = worker.drain(STATUS_CAPACITY).expect("drain worker");
            if let Some(text) = drain.local_text {
                assert_eq!(text, LOCAL_TEXT);
                assert_eq!(sync.local_change(&text), LocalChangeResult::Queued);
                local_observed = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            local_observed,
            "known local clipboard change was not observed"
        );

        let local_update = sync.tick(0).expect("send local update to fake remote");
        assert_eq!(local_update.bytes, LOCAL_TEXT);
        let remote = racc_clipboard::ClipboardUpdate {
            origin: OriginId(2),
            seq: 1,
            logical_clock: local_update.logical_clock.wrapping_add(1),
            bytes: REMOTE_TEXT.to_vec(),
        };
        let applied = match sync.receive_remote(remote) {
            RemoteChangeResult::Apply(applied) => applied,
            other => panic!("fake remote update was not accepted: {other:?}"),
        };
        assert_eq!(
            worker.queue_remote_text(applied.seq, applied.bytes),
            QueueRemoteResult::Queued
        );
        let mut remote_applied = false;
        let mut remote_echo_suppressed = false;
        for _ in 0..500 {
            let drain = worker.drain(STATUS_CAPACITY).expect("drain worker");
            for status in drain.statuses {
                if status == (ClipboardWorkerStatus::RemoteApplied { sequence: 1 }) {
                    remote_applied = true;
                }
            }
            if let Some(text) = drain.local_text {
                assert_eq!(text, REMOTE_TEXT);
                remote_echo_suppressed =
                    sync.local_change(&text) == LocalChangeResult::EchoSuppressed;
            }
            if remote_applied && remote_echo_suppressed {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(remote_applied, "remote test value was not applied");
        assert!(
            remote_echo_suppressed,
            "remote clipboard notification was not loop-suppressed"
        );
        assert_eq!(
            clipboard
                .read_text()
                .expect("read known remote test value")
                .as_deref(),
            Some(REMOTE_TEXT)
        );
        sync.end_session();
        drop(worker);
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
