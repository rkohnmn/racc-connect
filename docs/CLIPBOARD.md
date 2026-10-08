# Clipboard synchronization

## Current status

The portable text policy, current protocol v5 session-enable control (introduced in v3), ViewerRuntime clipboard ports, and Windows and macOS host/viewer adapters are source-integrated. The viewer reads Unicode text only while the user enables clipboard sync for the active session. Remote text applies only while that same enabled session is current. Disable, disconnect, and shutdown clear queued text and gate future reads/writes. Clipboard contents never enter logs, telemetry, UI events, or status metadata.

Portable policy and fake round-trip tests pass. The NSPasteboard adapter and Mac viewer worker pass Apple-target compile checks, but the Mac foreground host check is blocked before host Rust compilation by OpenH264 needing a target C++ compiler unavailable on this Windows machine. No native clipboard or PC-to-PC round-trip has been verified on Windows or macOS; source integration is not runtime verification.

## Scope and limits

- Text only, encoded as UTF-8, with a 512 KiB maximum. Images, files, and rich formats are out of scope.
- Local reads and remote writes are gated by the session-level clipboard toggle. The app starts with clipboard sync disabled.
- The worker coalesces local clipboard notifications and queued remote writes. Repeated updates are rate-limited by the portable policy to five per second.
- Windows line endings normalize to LF when read and convert to CRLF when written.
- Status reports contain only direction, sequence, byte count when known, and outcome. They never contain text.

## Portable policy and protocol

`racc-clipboard::ClipboardSync` is a sans-IO state machine. It supports independent directions, validates UTF-8 and size, coalesces bursts to the newest value, limits sends to five per second, rejects stale sequences, suppresses an echo after remote text is applied, and orders concurrent updates with a logical clock, origin, and wrapping sequence.

`ClipboardUpdate` carries a sender sequence, Viewer/Host origin, versioned u64 logical clock, UTF-8 text, and the bounded text payload. Protocol v3 introduced `ClipboardSyncControl { enabled }`; the current wire protocol is v5. ViewerRuntime sends this after each successful handshake and whenever the user toggles sync. The authenticated host starts its clipboard bridge only after an explicit enabled signal; disable ends the host policy session and clears pending state. Clipboard sync remains disabled until the user turns it on.

Tests cover echo suppression, concurrent convergence, invalid UTF-8, exact size bounds, sequence wrap, burst coalescing and rate limits, direction toggles, session cleanup, malformed protocol values, and fake clipboard contention retries.

## Windows viewer adapter

`WindowsClipboardWorker` owns the listener and OS reads/writes on a dedicated thread. It starts dormant, creates the listener only while sync is enabled, reads Unicode text after clipboard-change notifications, and checks the current enable generation before sending data or applying remote text. Worker-to-UI data is a one-slot latest local value plus a bounded content-free status queue. Remote actions use a one-slot latest-wins apply queue. Disabling clears queued local and remote values; connection loss disables the worker until a session reconnects. Shutdown requests are nonblocking and the app joins the worker when the live viewer is dropped.

The live viewer feeds local text into the separate ViewerRuntime clipboard ingress and drains accepted remote actions into the platform worker. The Mac worker starts dormant, polls NSPasteboard at 250 ms only while the active connected session is enabled, and checks the session generation before applying inbound text. It coalesces local text, keeps a bounded status queue with no clipboard content, and joins on viewer shutdown. The CONTROL > Clipboard toggle is offered only when the local adapter exists and the selected host advertises `FEATURE_TEXT_CLIPBOARD`.

## Windows host bridge

The authenticated Windows and foreground macOS hosts handle `ClipboardSyncControl` only for the active authorized viewer connection. An enable signal starts the host clipboard policy and OS listener; a disable signal ends the session, stops reads, and drops pending sends. Viewer-originated updates pass origin, logical-clock, sequence, UTF-8, and size checks before the platform writer applies them. Listener notifications pass through shared echo suppression. Host-originated changes use the bounded policy and retain one pending update until the control queue accepts it. The Mac host advertises text clipboard capability because this handler is wired; it remains disabled until an explicit session signal.

Fake tests cover viewer-to-host and host-to-viewer round trips, two-sided echo suppression, explicit opt-in/disable, retry retention, stale sequence rejection, wraparound, and oversized text rejection. These tests verify policy and dispatch only; they do not verify real Windows clipboard contention or the full service/helper lifecycle.

## Remaining acceptance checks

Run a real Windows PC #1/PC #2 session with non-private text, non-ASCII characters, repeated changes, exactly 512 KiB, and a value over 512 KiB. Confirm the disabled state does not read or apply clipboard text, the explicit toggle enables both directions, disable immediately stops future transfers, and no text appears in logs or telemetry. Compare live route/RTT/loss/bitrate/FPS/codec/decoder/host telemetry with their sources and measure frame pacing with the telemetry sidebar on and off. The human checklist is in `docs/HARDWARE.md`.

The NSPasteboard adapter reads and writes only `NSPasteboardTypeString`; it checks the UTF-8 byte count before converting to a Rust string and rejects values above 512 KiB. Remote-write change counts suppress echoes. macOS APIs have only compile/fake evidence here. Human verification must cover both host/viewer directions, disabled-by-default behavior, disable/disconnect/reconnect gating, Unicode and 512 KiB limits, echo prevention, and content-free logs. Monterey runtime behavior and later macOS pasteboard prompts have not been checked.
