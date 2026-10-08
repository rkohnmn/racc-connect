//! Text-first clipboard synchronization policy.
//!
//! Portable and sans-I/O. OS clipboard access and control-channel wiring belong
//! to platform/core adapters.
#![deny(unsafe_code)]
#![warn(missing_docs)]

use std::{cmp::Ordering, str};

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod macos;
/// Portable change-count polling policy for a macOS NSPasteboard adapter.
pub mod macos_poll;
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
pub mod windows;

/// Maximum number of attempts when Windows reports a temporarily busy clipboard.
pub const CLIPBOARD_CONTENTION_ATTEMPTS: usize = 5;

/// Error category reported by a platform clipboard adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardErrorKind {
    /// Another process owns the clipboard for longer than the bounded retry period.
    Contended,
    /// Windows denied access; this can indicate a secure desktop or session boundary.
    AccessDenied {
        /// Whether the adapter observed the Windows access-denied status.
        secure_desktop_likely: bool,
    },
    /// Input text supplied to the Windows adapter was not valid UTF-8.
    InvalidUtf8,
    /// The OS clipboard text did not contain valid UTF-16.
    InvalidUtf16,
    /// Text exceeded the fixed clipboard payload limit.
    TooLarge,
    /// A platform operation failed with the specified native status code.
    Platform(u32),
}

/// Adapter operation that failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardOperation {
    /// Creating or registering the clipboard-change listener.
    Listen,
    /// Reading clipboard text.
    Read,
    /// Writing clipboard text.
    Write,
}

/// A platform clipboard operation error with a reportable operation and cause.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClipboardAdapterError {
    /// Operation that failed.
    pub operation: ClipboardOperation,
    /// Failure category.
    pub kind: ClipboardErrorKind,
}

impl std::fmt::Display for ClipboardAdapterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "clipboard {:?} failed: {:?}",
            self.operation, self.kind
        )
    }
}

impl std::error::Error for ClipboardAdapterError {}

#[cfg(any(target_os = "windows", test))]
fn retry_contended<T, E>(
    attempts: usize,
    mut operation: impl FnMut() -> Result<T, E>,
    mut is_contended: impl FnMut(&E) -> bool,
    mut wait: impl FnMut(usize),
) -> Result<T, E> {
    let attempts = attempts.max(1);
    for attempt in 0..attempts {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if is_contended(&error) && attempt + 1 < attempts => {
                wait(attempt);
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("at least one clipboard attempt always runs")
}

#[cfg(any(target_os = "windows", test))]
fn normalize_windows_newlines(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                normalized.push('\n');
            }
            other => normalized.push(other),
        }
    }
    normalized
}

#[cfg(any(target_os = "windows", test))]
fn to_windows_newlines(text: &str) -> String {
    normalize_windows_newlines(text).replace('\n', "\r\n")
}
/// Maximum accepted UTF-8 clipboard text payload, in bytes.
pub const MAX_CLIPBOARD_BYTES: usize = 512 * 1024;
/// Minimum spacing between outgoing updates, enforcing at most five per second.
pub const MIN_UPDATE_INTERVAL_MS: u64 = 200;

/// Opaque stable identifier for one endpoint.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OriginId(pub u128);

/// Direction switches configured for a session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClipboardDirections {
    /// Send local changes to the peer.
    pub local_to_remote: bool,
    /// Apply peer changes locally.
    pub remote_to_local: bool,
}

impl ClipboardDirections {
    /// Enables both directions.
    pub const fn both() -> Self {
        Self {
            local_to_remote: true,
            remote_to_local: true,
        }
    }
}

/// Text update carrying sender sequence and logical revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardUpdate {
    /// Sender identity.
    pub origin: OriginId,
    /// Sender sequence, compared with wrapping serial-number arithmetic.
    pub seq: u64,
    /// Logical revision for deterministic conflict ordering without wall clocks.
    pub logical_clock: u64,
    /// UTF-8 bytes; validate before applying.
    pub bytes: Vec<u8>,
}

/// Why clipboard content was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardRejection {
    /// Payload exceeded the fixed byte limit.
    TooLarge {
        /// Supplied byte count.
        bytes: usize,
        /// Maximum accepted byte count.
        limit: usize,
    },
    /// Payload was not valid UTF-8.
    InvalidUtf8,
}

/// Result of observing a local clipboard notification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalChangeResult {
    /// Queued; a later tick may emit it.
    Queued,
    /// Send direction is disabled; local value still affects conflict ordering.
    DirectionDisabled,
    /// Notification matches the currently applied remote text.
    EchoSuppressed,
    /// Text is unchanged.
    Unchanged,
    /// Session is inactive.
    SessionInactive,
    /// Text was invalid or too large.
    Rejected(ClipboardRejection),
}

/// Why a remote update was ignored.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteIgnoreReason {
    /// Session is inactive.
    SessionInactive,
    /// Receive direction is disabled.
    DirectionDisabled,
    /// Origin does not match the current peer.
    WrongOrigin,
    /// Sequence is duplicate or stale.
    DuplicateOrStale,
    /// Update lost deterministic conflict ordering.
    ConflictLost,
}

/// Accepted remote text for the platform adapter to apply.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardApply {
    /// Origin of the accepted value.
    pub origin: OriginId,
    /// Sender sequence of the accepted value.
    pub seq: u64,
    /// Stable non-cryptographic fingerprint of the bytes.
    pub hash: u64,
    /// UTF-8 bytes to write locally.
    pub bytes: Vec<u8>,
}

/// Result of receiving a remote text update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteChangeResult {
    /// Accepted; apply this value to the local clipboard.
    Apply(ClipboardApply),
    /// Rejected; suitable for a user-visible notice.
    Rejected(ClipboardRejection),
    /// Ignored.
    Ignored(RemoteIgnoreReason),
}

/// Metadata for the latest accepted remote update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AppliedRemote {
    /// Origin of the accepted value.
    pub origin: OriginId,
    /// Sender sequence of the accepted value.
    pub seq: u64,
    /// Stable non-cryptographic fingerprint of the value.
    pub hash: u64,
}

/// Direction that may be toggled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardDirection {
    /// Send local changes to peer.
    LocalToRemote,
    /// Apply peer changes locally.
    RemoteToLocal,
}

/// Why starting a session failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionStartError {
    /// Endpoint IDs must differ.
    SameOrigin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Version {
    clock: u64,
    origin: OriginId,
    seq: u64,
}

/// Sans-I/O clipboard policy engine for one endpoint.
#[derive(Debug)]
pub struct ClipboardSync {
    local_origin: OriginId,
    remote_origin: Option<OriginId>,
    active: bool,
    directions: ClipboardDirections,
    next_seq: u64,
    clock: u64,
    current: Option<Version>,
    current_bytes: Option<Vec<u8>>,
    last_remote_seq: Option<u64>,
    last_applied_remote: Option<AppliedRemote>,
    pending: Option<ClipboardUpdate>,
    last_sent_ms: Option<u64>,
}

impl ClipboardSync {
    /// Creates an inactive policy engine.
    pub const fn new(local_origin: OriginId) -> Self {
        Self {
            local_origin,
            remote_origin: None,
            active: false,
            directions: ClipboardDirections {
                local_to_remote: false,
                remote_to_local: false,
            },
            next_seq: 0,
            clock: 0,
            current: None,
            current_bytes: None,
            last_remote_seq: None,
            last_applied_remote: None,
            pending: None,
            last_sent_ms: None,
        }
    }

    /// Starts a session. Sequence numbering remains monotonic across sessions.
    pub fn start_session(
        &mut self,
        remote_origin: OriginId,
        directions: ClipboardDirections,
    ) -> Result<(), SessionStartError> {
        if remote_origin == self.local_origin {
            return Err(SessionStartError::SameOrigin);
        }
        self.clear_state();
        self.remote_origin = Some(remote_origin);
        self.active = true;
        self.directions = directions;
        Ok(())
    }

    /// Ends the session, dropping pending text and all conflict and echo state.
    pub fn end_session(&mut self) {
        self.clear_state();
        self.remote_origin = None;
        self.active = false;
        self.directions = ClipboardDirections::default();
    }

    /// Returns whether a session is active.
    pub const fn is_session_active(&self) -> bool {
        self.active
    }

    /// Returns the configured direction switches.
    pub const fn directions(&self) -> ClipboardDirections {
        self.directions
    }

    /// Enables or disables a direction; disabling send drops queued text.
    pub fn set_enabled(&mut self, direction: ClipboardDirection, enabled: bool) {
        match direction {
            ClipboardDirection::LocalToRemote => {
                self.directions.local_to_remote = enabled;
                if !enabled {
                    self.pending = None;
                }
            }
            ClipboardDirection::RemoteToLocal => self.directions.remote_to_local = enabled,
        }
    }

    /// Returns metadata for the most recently accepted remote update.
    pub const fn last_applied_remote(&self) -> Option<AppliedRemote> {
        self.last_applied_remote
    }

    /// Handles bytes read by a platform clipboard-change listener.
    ///
    /// Call on notifications rather than polling. No text is logged.
    pub fn local_change(&mut self, bytes: &[u8]) -> LocalChangeResult {
        if !self.active {
            return LocalChangeResult::SessionInactive;
        }
        if let Err(error) = validate(bytes) {
            return LocalChangeResult::Rejected(error);
        }

        if self.current_bytes.as_deref() == Some(bytes) {
            if self
                .current
                .is_some_and(|v| Some(v.origin) == self.remote_origin)
                && self
                    .last_applied_remote
                    .is_some_and(|r| r.hash == fingerprint(bytes))
            {
                // Exact byte equality above is authoritative; the hash is metadata.
                return LocalChangeResult::EchoSuppressed;
            }
            return LocalChangeResult::Unchanged;
        }

        self.clock = self.clock.wrapping_add(1);
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        let version = Version {
            clock: self.clock,
            origin: self.local_origin,
            seq,
        };
        self.current = Some(version);
        self.current_bytes = Some(bytes.to_vec());

        if !self.directions.local_to_remote {
            self.pending = None;
            return LocalChangeResult::DirectionDisabled;
        }
        self.pending = Some(ClipboardUpdate {
            origin: self.local_origin,
            seq,
            logical_clock: version.clock,
            bytes: bytes.to_vec(),
        });
        LocalChangeResult::Queued
    }

    /// Validates and resolves a remote update; accepted text is returned for local application.
    pub fn receive_remote(&mut self, update: ClipboardUpdate) -> RemoteChangeResult {
        if !self.active {
            return RemoteChangeResult::Ignored(RemoteIgnoreReason::SessionInactive);
        }
        if !self.directions.remote_to_local {
            return RemoteChangeResult::Ignored(RemoteIgnoreReason::DirectionDisabled);
        }
        if Some(update.origin) != self.remote_origin || update.origin == self.local_origin {
            return RemoteChangeResult::Ignored(RemoteIgnoreReason::WrongOrigin);
        }
        if let Err(error) = validate(&update.bytes) {
            return RemoteChangeResult::Rejected(error);
        }
        if self
            .last_remote_seq
            .is_some_and(|last| !is_newer(update.seq, last))
        {
            return RemoteChangeResult::Ignored(RemoteIgnoreReason::DuplicateOrStale);
        }

        self.last_remote_seq = Some(update.seq);
        if is_newer(update.logical_clock, self.clock) {
            self.clock = update.logical_clock;
        }

        let incoming = Version {
            clock: update.logical_clock,
            origin: update.origin,
            seq: update.seq,
        };
        if self
            .current
            .is_some_and(|current| compare(incoming, current) != Ordering::Greater)
        {
            return RemoteChangeResult::Ignored(RemoteIgnoreReason::ConflictLost);
        }

        self.clock = update.logical_clock;
        self.current = Some(incoming);
        self.current_bytes = Some(update.bytes.clone());
        self.pending = None;
        let hash = fingerprint(&update.bytes);
        self.last_applied_remote = Some(AppliedRemote {
            origin: update.origin,
            seq: update.seq,
            hash,
        });
        RemoteChangeResult::Apply(ClipboardApply {
            origin: update.origin,
            seq: update.seq,
            hash,
            bytes: update.bytes,
        })
    }

    /// Emits the latest queued local update if the 200 ms interval permits it.
    ///
    /// The first update may be emitted immediately. Later local changes replace
    /// the queued value; no queue can grow. The caller supplies monotonic milliseconds.
    pub fn tick(&mut self, now_ms: u64) -> Option<ClipboardUpdate> {
        if !self.active || !self.directions.local_to_remote {
            self.pending = None;
            return None;
        }
        let pending = self.pending.as_ref()?;
        if let Some(last) = self.last_sent_ms {
            let now = now_ms.max(last);
            if now < last.saturating_add(MIN_UPDATE_INTERVAL_MS) {
                return None;
            }
        }
        let update = pending.clone();
        self.pending = None;
        self.last_sent_ms = Some(self.last_sent_ms.map_or(now_ms, |last| now_ms.max(last)));
        Some(update)
    }

    fn clear_state(&mut self) {
        self.clock = 0;
        self.current = None;
        self.current_bytes = None;
        self.last_remote_seq = None;
        self.last_applied_remote = None;
        self.pending = None;
        self.last_sent_ms = None;
    }
}

fn validate(bytes: &[u8]) -> Result<(), ClipboardRejection> {
    if bytes.len() > MAX_CLIPBOARD_BYTES {
        return Err(ClipboardRejection::TooLarge {
            bytes: bytes.len(),
            limit: MAX_CLIPBOARD_BYTES,
        });
    }
    str::from_utf8(bytes)
        .map(|_| ())
        .map_err(|_| ClipboardRejection::InvalidUtf8)
}

fn fingerprint(bytes: &[u8]) -> u64 {
    // Stable FNV-1a metadata. Echo suppression also compares the complete bytes.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn is_newer(candidate: u64, reference: u64) -> bool {
    let distance = candidate.wrapping_sub(reference);
    distance != 0 && distance < (1_u64 << 63)
}

fn compare(left: Version, right: Version) -> Ordering {
    if left.clock != right.clock {
        return if is_newer(left.clock, right.clock) {
            Ordering::Greater
        } else {
            Ordering::Less
        };
    }
    left.origin.cmp(&right.origin).then_with(|| {
        if left.seq == right.seq {
            Ordering::Equal
        } else if is_newer(left.seq, right.seq) {
            Ordering::Greater
        } else {
            Ordering::Less
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: OriginId = OriginId(1);
    const B: OriginId = OriginId(2);

    #[derive(Debug)]
    struct FakeClipboardAdapter {
        busy_attempts_left: usize,
        open_attempts: usize,
    }

    impl FakeClipboardAdapter {
        fn open(&mut self) -> Result<(), ClipboardErrorKind> {
            self.open_attempts += 1;
            if self.busy_attempts_left > 0 {
                self.busy_attempts_left -= 1;
                Err(ClipboardErrorKind::Contended)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn fake_platform_adapter_retries_contention_with_a_finite_budget() {
        let mut clipboard = FakeClipboardAdapter {
            busy_attempts_left: 2,
            open_attempts: 0,
        };
        let mut pauses = 0;
        let opened = retry_contended(
            CLIPBOARD_CONTENTION_ATTEMPTS,
            || clipboard.open(),
            |error| *error == ClipboardErrorKind::Contended,
            |_| pauses += 1,
        );
        assert_eq!(opened, Ok(()));
        assert_eq!(clipboard.open_attempts, 3);
        assert_eq!(pauses, 2);
    }

    #[test]
    fn fake_platform_adapter_stops_after_bounded_contention_attempts() {
        let mut clipboard = FakeClipboardAdapter {
            busy_attempts_left: usize::MAX,
            open_attempts: 0,
        };
        let mut pauses = 0;
        let opened = retry_contended(
            CLIPBOARD_CONTENTION_ATTEMPTS,
            || clipboard.open(),
            |error| *error == ClipboardErrorKind::Contended,
            |_| pauses += 1,
        );
        assert_eq!(opened, Err(ClipboardErrorKind::Contended));
        assert_eq!(clipboard.open_attempts, CLIPBOARD_CONTENTION_ATTEMPTS);
        assert_eq!(pauses, CLIPBOARD_CONTENTION_ATTEMPTS - 1);
    }

    #[test]
    fn windows_clipboard_newline_conversion_is_idempotent() {
        let local = "first\nsecond\nthird\r\nfourth\rfinal";
        let windows = to_windows_newlines(local);
        assert_eq!(windows, "first\r\nsecond\r\nthird\r\nfourth\r\nfinal");
        assert_eq!(
            normalize_windows_newlines(&windows),
            local.replace("\r\n", "\n").replace('\r', "\n")
        );
    }

    fn engine() -> ClipboardSync {
        let mut result = ClipboardSync::new(A);
        result
            .start_session(B, ClipboardDirections::both())
            .unwrap();
        result
    }

    fn remote(seq: u64, clock: u64, bytes: &[u8]) -> ClipboardUpdate {
        ClipboardUpdate {
            origin: B,
            seq,
            logical_clock: clock,
            bytes: bytes.to_vec(),
        }
    }

    fn accept(result: RemoteChangeResult) -> ClipboardApply {
        match result {
            RemoteChangeResult::Apply(value) => value,
            other => panic!("expected apply, got {other:?}"),
        }
    }

    #[test]
    fn remote_write_echo_is_not_sent_back() {
        let mut a = engine();
        let mut b = ClipboardSync::new(B);
        b.start_session(A, ClipboardDirections::both()).unwrap();
        a.local_change(b"hello");
        let outbound = a.tick(0).unwrap();
        let applied = accept(b.receive_remote(outbound));
        assert_eq!(applied.bytes, b"hello");
        assert_eq!(b.last_applied_remote().unwrap().hash, fingerprint(b"hello"));
        assert_eq!(b.local_change(b"hello"), LocalChangeResult::EchoSuppressed);
        assert_eq!(b.tick(500), None);
    }

    #[test]
    fn bursts_coalesce_and_rate_limit_at_five_hz() {
        let mut s = engine();
        s.local_change(b"one");
        s.local_change(b"two");
        let first = s.tick(10).unwrap();
        assert_eq!(first.bytes, b"two");
        assert_eq!(first.seq, 1);
        s.local_change(b"three");
        assert_eq!(s.tick(209), None);
        s.local_change(b"four");
        let second = s.tick(210).unwrap();
        assert_eq!(second.bytes, b"four");
        assert_eq!(second.seq, 3);
        assert_eq!(s.tick(410), None);
    }

    #[test]
    fn validates_utf8_and_exact_512_kib_limit_in_both_directions() {
        let mut s = engine();
        assert_eq!(
            s.local_change(&[0xff]),
            LocalChangeResult::Rejected(ClipboardRejection::InvalidUtf8)
        );
        assert_eq!(
            s.local_change(&vec![b'a'; MAX_CLIPBOARD_BYTES + 1]),
            LocalChangeResult::Rejected(ClipboardRejection::TooLarge {
                bytes: MAX_CLIPBOARD_BYTES + 1,
                limit: MAX_CLIPBOARD_BYTES,
            })
        );
        assert_eq!(
            s.local_change(&vec![b'a'; MAX_CLIPBOARD_BYTES]),
            LocalChangeResult::Queued
        );
        assert_eq!(
            s.receive_remote(remote(0, 1, &[0xff])),
            RemoteChangeResult::Rejected(ClipboardRejection::InvalidUtf8)
        );
        assert_eq!(
            s.receive_remote(remote(0, 1, &vec![b'a'; MAX_CLIPBOARD_BYTES + 1])),
            RemoteChangeResult::Rejected(ClipboardRejection::TooLarge {
                bytes: MAX_CLIPBOARD_BYTES + 1,
                limit: MAX_CLIPBOARD_BYTES,
            })
        );
    }

    #[test]
    fn concurrent_updates_converge_on_greater_origin_id() {
        let mut left = ClipboardSync::new(A);
        left.start_session(B, ClipboardDirections::both()).unwrap();
        let mut right = ClipboardSync::new(B);
        right.start_session(A, ClipboardDirections::both()).unwrap();
        left.local_change(b"left");
        right.local_change(b"right");
        let left_update = left.tick(0).unwrap();
        let right_update = right.tick(0).unwrap();

        assert!(matches!(
            left.receive_remote(right_update),
            RemoteChangeResult::Apply(_)
        ));
        assert_eq!(
            right.receive_remote(left_update),
            RemoteChangeResult::Ignored(RemoteIgnoreReason::ConflictLost)
        );
        assert_eq!(
            left.local_change(b"right"),
            LocalChangeResult::EchoSuppressed
        );
    }

    #[test]
    fn later_logical_revision_wins_even_from_lower_origin() {
        let mut s = engine();
        s.local_change(b"local");
        s.tick(0);
        assert_eq!(
            accept(s.receive_remote(remote(0, 2, b"remote"))).bytes,
            b"remote"
        );
    }

    #[test]
    fn remote_sequence_wraparound_duplicates_and_old_values_are_handled() {
        let mut s = engine();
        accept(s.receive_remote(remote(u64::MAX - 1, 1, b"a")));
        accept(s.receive_remote(remote(u64::MAX, 2, b"b")));
        accept(s.receive_remote(remote(0, 3, b"c")));
        assert_eq!(
            s.receive_remote(remote(u64::MAX, 4, b"old")),
            RemoteChangeResult::Ignored(RemoteIgnoreReason::DuplicateOrStale)
        );
        assert_eq!(
            s.receive_remote(remote(0, 5, b"duplicate")),
            RemoteChangeResult::Ignored(RemoteIgnoreReason::DuplicateOrStale)
        );
        assert_eq!(s.last_applied_remote().unwrap().seq, 0);
        assert_eq!(s.last_applied_remote().unwrap().hash, fingerprint(b"c"));
    }

    #[test]
    fn direction_toggles_drop_pending_text_and_do_not_flush_stale_values() {
        let mut s = engine();
        s.local_change(b"queued");
        s.set_enabled(ClipboardDirection::LocalToRemote, false);
        assert_eq!(s.tick(0), None);
        assert_eq!(
            s.local_change(b"private"),
            LocalChangeResult::DirectionDisabled
        );
        s.set_enabled(ClipboardDirection::LocalToRemote, true);
        assert_eq!(s.tick(500), None);
        s.local_change(b"new");
        assert_eq!(s.tick(500).unwrap().bytes, b"new");

        s.set_enabled(ClipboardDirection::RemoteToLocal, false);
        assert_eq!(
            s.receive_remote(remote(0, 4, b"ignored")),
            RemoteChangeResult::Ignored(RemoteIgnoreReason::DirectionDisabled)
        );
        s.set_enabled(ClipboardDirection::RemoteToLocal, true);
        assert!(matches!(
            s.receive_remote(remote(0, 4, b"accepted")),
            RemoteChangeResult::Apply(_)
        ));
    }

    #[test]
    fn session_end_clears_pending_text_and_echo_metadata() {
        let mut s = engine();
        s.local_change(b"pending");
        accept(s.receive_remote(remote(0, 3, b"remote")));
        assert!(s.last_applied_remote().is_some());
        s.end_session();
        assert_eq!(s.last_applied_remote(), None);
        assert_eq!(
            s.local_change(b"outside"),
            LocalChangeResult::SessionInactive
        );
        assert_eq!(s.tick(20_000), None);
        s.start_session(B, ClipboardDirections::both()).unwrap();
        assert_eq!(s.tick(20_000), None);
        assert!(matches!(
            s.receive_remote(remote(0, 1, b"fresh")),
            RemoteChangeResult::Apply(_)
        ));
    }

    #[test]
    fn wrong_origin_and_inactive_state_are_ignored() {
        let mut s = ClipboardSync::new(A);
        assert_eq!(s.local_change(b"x"), LocalChangeResult::SessionInactive);
        assert_eq!(
            s.receive_remote(remote(0, 1, b"x")),
            RemoteChangeResult::Ignored(RemoteIgnoreReason::SessionInactive)
        );
        assert_eq!(
            s.start_session(A, ClipboardDirections::both()),
            Err(SessionStartError::SameOrigin)
        );
        s.start_session(B, ClipboardDirections::both()).unwrap();
        assert_eq!(
            s.receive_remote(ClipboardUpdate {
                origin: OriginId(3),
                seq: 0,
                logical_clock: 1,
                bytes: b"x".to_vec(),
            }),
            RemoteChangeResult::Ignored(RemoteIgnoreReason::WrongOrigin)
        );
    }
}
