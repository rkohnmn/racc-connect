//! Host-side text clipboard policy and protocol bridge.
//!
//! The bridge is sans-I/O. The first authenticated Viewer-originated update opts the session into
//! clipboard synchronization because v0 has no separate enable/disable control message. Native
//! listeners and writers stay in the platform adapter; clipboard bytes never enter logs or events.

use racc_clipboard::{
    ClipboardDirections, ClipboardSync, LocalChangeResult, OriginId, RemoteChangeResult,
    MAX_CLIPBOARD_BYTES,
};
use racc_core::HostConnectionId;
use racc_proto::{
    ClipboardOrigin, ClipboardUpdate, ControlMessage, LogicalClock, CLIPBOARD_LOGICAL_CLOCK_VERSION,
};

const VIEWER_ORIGIN: OriginId = OriginId(0);
const HOST_ORIGIN: OriginId = OriginId(1);

/// A per-authorized-viewer clipboard bridge with bounded latest-wins buffering.
#[derive(Debug)]
pub struct HostClipboardBridge {
    connection_id: HostConnectionId,
    sync: ClipboardSync,
    active: bool,
    last_remote_wire_sequence: Option<(u32, u64)>,
    pending_wire_message: Option<ControlMessage>,
}

/// A local clipboard change could not be represented on the current protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostClipboardError {
    /// The peer used a logical-clock schema this host does not understand.
    UnsupportedLogicalClockVersion,
    /// The local logical clock exceeded the v0 protocol bound.
    LogicalClockOutOfRange,
    /// Starting the fixed host/viewer origin pair failed.
    SessionStartFailed,
    /// Policy produced bytes that were not valid UTF-8 (an internal invariant failure).
    InvalidUtf8,
}

impl HostClipboardBridge {
    /// Creates an inactive clipboard session for one authenticated viewer.
    pub fn new(connection_id: HostConnectionId) -> Self {
        Self {
            connection_id,
            sync: ClipboardSync::new(HOST_ORIGIN),
            active: false,
            last_remote_wire_sequence: None,
            pending_wire_message: None,
        }
    }

    /// Returns the viewer connection this bridge belongs to.
    pub const fn connection_id(&self) -> HostConnectionId {
        self.connection_id
    }

    /// Returns whether the viewer has opted into clipboard sync for this session.
    pub const fn is_enabled(&self) -> bool {
        self.active
    }

    /// Applies the authenticated viewer's per-session clipboard preference.
    pub fn set_enabled(&mut self, enabled: bool) -> Result<(), HostClipboardError> {
        if enabled == self.active {
            return Ok(());
        }
        if enabled {
            self.sync
                .start_session(VIEWER_ORIGIN, ClipboardDirections::both())
                .map_err(|_| HostClipboardError::SessionStartFailed)?;
            self.active = true;
        } else {
            self.end_session();
        }
        Ok(())
    }

    /// Resolves an authenticated viewer update through the shared size, sequence, and echo policy.
    ///
    /// A successful `Apply` contains text for the platform clipboard writer. Callers must not log
    /// that value. Non-Viewer origins are ignored; a host cannot accept its own update as remote.
    pub fn receive_remote(
        &mut self,
        update: ClipboardUpdate,
    ) -> Result<RemoteChangeResult, HostClipboardError> {
        if update.origin != ClipboardOrigin::Viewer {
            return Ok(RemoteChangeResult::Ignored(
                racc_clipboard::RemoteIgnoreReason::WrongOrigin,
            ));
        }
        if update.logical_clock.version != CLIPBOARD_LOGICAL_CLOCK_VERSION {
            return Err(HostClipboardError::UnsupportedLogicalClockVersion);
        }
        if LogicalClock::new(update.logical_clock.counter).is_err() {
            return Err(HostClipboardError::LogicalClockOutOfRange);
        }
        if update.text.len() > MAX_CLIPBOARD_BYTES {
            return Ok(RemoteChangeResult::Rejected(
                racc_clipboard::ClipboardRejection::TooLarge {
                    bytes: update.text.len(),
                    limit: MAX_CLIPBOARD_BYTES,
                },
            ));
        }
        if !self.active {
            return Ok(RemoteChangeResult::Ignored(
                racc_clipboard::RemoteIgnoreReason::SessionInactive,
            ));
        }
        let sequence = extend_wire_sequence(update.seq, &mut self.last_remote_wire_sequence);
        Ok(self.sync.receive_remote(racc_clipboard::ClipboardUpdate {
            origin: VIEWER_ORIGIN,
            seq: sequence,
            logical_clock: update.logical_clock.counter,
            bytes: update.text.into_bytes(),
        }))
    }

    /// Passes a text-only OS listener change into the coalescing local policy.
    ///
    /// This is inert until the viewer has opted into the session with a valid update.
    pub fn local_change(&mut self, bytes: &[u8]) -> LocalChangeResult {
        if !self.active {
            return LocalChangeResult::SessionInactive;
        }
        self.sync.local_change(bytes)
    }

    /// Returns the next host-originated message due for the control channel.
    ///
    /// The single pending wire message is retained until the caller confirms that the bounded
    /// `ControlSendHandle` queue accepted it. Repeated calls therefore cannot duplicate the send.
    pub fn next_outbound(
        &mut self,
        now_ms: u64,
    ) -> Result<Option<ControlMessage>, HostClipboardError> {
        if let Some(message) = &self.pending_wire_message {
            return Ok(Some(message.clone()));
        }
        let Some(update) = self.sync.tick(now_ms) else {
            return Ok(None);
        };
        let text = String::from_utf8(update.bytes).map_err(|_| HostClipboardError::InvalidUtf8)?;
        let logical_clock = LogicalClock::new(update.logical_clock)
            .map_err(|_| HostClipboardError::LogicalClockOutOfRange)?;
        let message = ControlMessage::ClipboardUpdate(ClipboardUpdate {
            seq: update.seq as u32,
            origin: ClipboardOrigin::Host,
            logical_clock,
            text,
        });
        self.pending_wire_message = Some(message.clone());
        Ok(Some(message))
    }

    /// Marks the pending message queued by the control sender as accepted.
    pub fn confirm_outbound_queued(&mut self) {
        self.pending_wire_message = None;
    }

    /// Ends the session and drops all queued content and echo state.
    pub fn end_session(&mut self) {
        self.sync.end_session();
        self.active = false;
        self.last_remote_wire_sequence = None;
        self.pending_wire_message = None;
    }
}

fn extend_wire_sequence(sequence: u32, previous: &mut Option<(u32, u64)>) -> u64 {
    const HALF_RANGE: u32 = 1 << 31;
    match *previous {
        None => {
            let extended = u64::from(sequence);
            *previous = Some((sequence, extended));
            extended
        }
        Some((last_wire, last_extended)) => {
            let distance = sequence.wrapping_sub(last_wire);
            if distance == 0 || distance >= HALF_RANGE {
                last_extended
            } else {
                let extended = last_extended.saturating_add(u64::from(distance));
                *previous = Some((sequence, extended));
                extended
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use racc_clipboard::{ClipboardSync, LocalChangeResult, RemoteChangeResult};

    fn wire(origin: ClipboardOrigin, seq: u32, clock: u64, text: &str) -> ClipboardUpdate {
        ClipboardUpdate {
            seq,
            origin,
            logical_clock: LogicalClock::new(clock).expect("test clock is in range"),
            text: text.to_owned(),
        }
    }

    fn apply(result: RemoteChangeResult) -> racc_clipboard::ClipboardApply {
        match result {
            RemoteChangeResult::Apply(value) => value,
            other => panic!("expected clipboard apply, got {other:?}"),
        }
    }

    #[test]
    fn fake_viewer_host_round_trip_suppresses_both_local_echoes() {
        let mut viewer = ClipboardSync::new(VIEWER_ORIGIN);
        assert!(viewer
            .start_session(HOST_ORIGIN, ClipboardDirections::both())
            .is_ok());
        let mut host = HostClipboardBridge::new(7);
        host.set_enabled(true)
            .expect("viewer explicitly enabled clipboard sync");

        assert_eq!(
            viewer.local_change(b"viewer text"),
            LocalChangeResult::Queued
        );
        let viewer_update = viewer
            .tick(0)
            .expect("first viewer update is immediately due");
        let host_result = host.receive_remote(wire(
            ClipboardOrigin::Viewer,
            viewer_update.seq as u32,
            viewer_update.logical_clock,
            "viewer text",
        ));
        let applied_at_host = apply(host_result.expect("valid viewer update"));
        assert_eq!(applied_at_host.bytes, b"viewer text");
        assert_eq!(
            host.local_change(&applied_at_host.bytes),
            LocalChangeResult::EchoSuppressed
        );

        assert_eq!(host.local_change(b"host text"), LocalChangeResult::Queued);
        let host_message = host.next_outbound(0).expect("host clock valid");
        let Some(ControlMessage::ClipboardUpdate(host_update)) = host_message else {
            panic!("host should produce a clipboard update");
        };
        assert_eq!(host_update.origin, ClipboardOrigin::Host);
        assert_eq!(host_update.text, "host text");
        assert!(matches!(
            viewer.receive_remote(racc_clipboard::ClipboardUpdate {
                origin: HOST_ORIGIN,
                seq: u64::from(host_update.seq),
                logical_clock: host_update.logical_clock.counter,
                bytes: host_update.text.into_bytes(),
            }),
            RemoteChangeResult::Apply(_)
        ));
        assert_eq!(
            viewer.local_change(b"host text"),
            LocalChangeResult::EchoSuppressed
        );
        host.confirm_outbound_queued();
        assert_eq!(host.next_outbound(1).expect("no queued update"), None);
    }

    #[test]
    fn host_waits_for_viewer_opt_in_and_retains_one_bounded_send_for_retry() {
        let mut host = HostClipboardBridge::new(8);
        assert_eq!(
            host.local_change(b"private"),
            LocalChangeResult::SessionInactive
        );
        assert_eq!(host.next_outbound(0).expect("inactive"), None);

        assert!(matches!(
            host.receive_remote(wire(ClipboardOrigin::Viewer, 4, 1, "before enable")),
            Ok(RemoteChangeResult::Ignored(
                racc_clipboard::RemoteIgnoreReason::SessionInactive
            ))
        ));
        host.set_enabled(true)
            .expect("viewer explicitly enabled sync");
        assert!(matches!(
            host.receive_remote(wire(ClipboardOrigin::Viewer, 4, 1, "opt in")),
            Ok(RemoteChangeResult::Apply(_))
        ));
        assert_eq!(host.local_change(b"host text"), LocalChangeResult::Queued);
        let first = host.next_outbound(0).expect("outbound").expect("message");
        let retry = host
            .next_outbound(50)
            .expect("retry")
            .expect("same message");
        assert_eq!(first, retry);
        host.confirm_outbound_queued();
        assert_eq!(host.next_outbound(100).expect("cleared"), None);
        assert!(matches!(
            host.receive_remote(wire(ClipboardOrigin::Viewer, 4, 2, "duplicate")),
            Ok(RemoteChangeResult::Ignored(
                racc_clipboard::RemoteIgnoreReason::DuplicateOrStale
            ))
        ));
    }

    #[test]
    fn explicit_disable_stops_host_clipboard_reads_and_drops_pending_sends() {
        let mut host = HostClipboardBridge::new(10);
        host.set_enabled(true).expect("enable");
        assert_eq!(
            host.local_change(b"before disable"),
            LocalChangeResult::Queued
        );
        host.set_enabled(false).expect("disable");
        assert_eq!(
            host.local_change(b"after disable"),
            LocalChangeResult::SessionInactive
        );
        assert_eq!(
            host.next_outbound(0).expect("pending send was dropped"),
            None
        );
        assert!(!host.is_enabled());
    }

    #[test]
    fn wire_sequences_extend_across_wrap_and_stale_values_do_not_advance() {
        let mut previous = None;
        assert_eq!(
            extend_wire_sequence(u32::MAX - 1, &mut previous),
            u64::from(u32::MAX - 1)
        );
        assert_eq!(
            extend_wire_sequence(1, &mut previous),
            u64::from(u32::MAX) + 2
        );
        assert_eq!(
            extend_wire_sequence(u32::MAX, &mut previous),
            u64::from(u32::MAX) + 2
        );
    }

    #[test]
    fn oversized_remote_text_is_rejected_without_retaining_it() {
        let mut host = HostClipboardBridge::new(9);
        let update = wire(
            ClipboardOrigin::Viewer,
            0,
            1,
            &"x".repeat(racc_clipboard::MAX_CLIPBOARD_BYTES + 1),
        );
        assert!(matches!(
            host.receive_remote(update),
            Ok(RemoteChangeResult::Rejected(
                racc_clipboard::ClipboardRejection::TooLarge { .. }
            ))
        ));
        assert_eq!(host.next_outbound(0).expect("no update"), None);
        assert!(!host.is_enabled());
    }
}
