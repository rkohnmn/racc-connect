M3a, topology, coordinate math and telemetry types

Save this file as docs/goals/M3a.md. Run it after M2.5 is complete (the prompt checks this itself).

Why M3 is split in two. AGENTS.md lumps display topology, input math, telemetry types and the session state machines into M3. The first three are pure, self-contained and easy to verify. The session state machines (connect, switch, pause, reconnect, failure recovery) depend on what M2.5 decides about NACK or FEC, so they come next as M3b, after this goal and the M2.5 result are known.

Before you run it (human checklist)
M2.5 has finished and its final report is in docs/PROGRESS.md. If you have not run it, do that first; the preflight below stops otherwise.
Working tree clean on main, synchronized with origin/main.
No hardware needed. Everything is pure logic.
Launcher (paste this single line)
/goal Complete the work specified in docs/goals/M3a.md. Read AGENTS.md, docs/DEV_SETUP.md, docs/PROGRESS.md and docs/PROTOCOL.md first. Done only when every acceptance check in docs/goals/M3a.md passes with evidence, then stop. Do not start M3b.
The full goal prompt
text
GOAL: M3a. Implement the pure display-topology model, the input coordinate math, and the telemetry types: crates/topology (package racc-topology) and crates/telemetry (package racc-telemetry). Everything is sans-IO and deterministic: no sockets, threads, clocks or platform calls. Time is passed in by the caller as microseconds on a monotonic timeline.

CONTEXT
Project: Racc Connect. Read AGENTS.md (sections 4.3, 4.4, 4.5, 5, 6, 9, 10, 11 M3), docs/PROJECT_SCOPE.md, docs/DEV_SETUP.md, docs/PROGRESS.md, docs/PROTOCOL.md and docs/TRANSPORT.md first. Follow the verification labels in DEV_SETUP.md section 1. Git rules: local identity is already rkohnmn; small logical commits; no Co-authored-by or tool-credit trailers; normal pushes only after checks pass; never force push; if a push fails do not work around authentication, mark BLOCKED-HUMAN.

PREFLIGHT
1. Working tree clean; scripts/check-all green; git log shows only the rkohnmn identity.
2. docs/PROGRESS.md must show M2.5 as complete with a final report. If it does not, stop and report that M2.5 must run first.
3. Read the M2.5 recommendation. This goal does not implement it, but note in docs/PROGRESS.md whether anything in it affects topology or telemetry types (for example new counters worth exposing) and add such items to docs/OPEN_QUESTIONS.md rather than building them.

DEPENDENCY POLICY (strict)
- racc-topology runtime dependencies: racc-proto (path) only. racc-telemetry runtime dependencies: racc-proto (path) only. Dev-dependency for both: proptest. Nothing else. No serde, no bitflags crate, no hashing crate, no time crate. Run cargo deny check after changes and keep it clean.
- #![forbid(unsafe_code)] stays in both crates. No panics, unwrap or expect on externally supplied values; checked or widened arithmetic (use i64 or u64 intermediates) for all coordinate and length math; no unbounded allocation.
- The layering rule holds: racc-topology and racc-telemetry must not depend on racc-net, racc-session, racc-core or any UI or host crate. scripts/check-layering must keep passing.

PART 1. racc-topology

1. Domain types (separate from the wire types; convert at the boundary):
   - DisplayId (nonzero u32 newtype; zero is reserved to mean "none" in the protocol).
   - Display: id, name (UTF-8, at most MAX_NAME_BYTES), origin x and y (i32, host physical pixels in virtual-desktop space), width_px and height_px (u32, nonzero), scale_milli (u16), refresh_mhz (u32), flags: primary, active, available, hdr (read-only label). Implement the flags as a small hand-written type, not a crate.
   - Topology: revision (u32), displays (at most MAX_DISPLAYS, unique nonzero ids, deterministic order), the currently streamed display id (Option). Validation function with typed errors: duplicate ids, zero id, zero size, too many displays, over-long names, more than one primary, active display not in the list.
2. Conversion to and from racc-proto TopologyAnnounce with full validation on the way in (a viewer receiving a zero display id, which the wire allows, must get a typed error here). Round-trip property tests.
3. Stable display identity: a pure function that derives a nonzero DisplayId from a DisplayIdentity (EDID manufacturer 3 letters, EDID product code u16, EDID serial u32 where 0 means absent, connector or instance name string, and an optional ordinal) using FNV-1a 32-bit over a documented canonical byte encoding; map a zero result to a documented nonzero value. Provide deterministic disambiguation for colliding ids within one topology. Golden test vectors (compute them with your implementation, then record the values in the tests and in docs/TOPOLOGY.md; also state that this hash is for identity stability, not security). Document the known limitation: displays without an EDID serial depend on the connector name, so cabling changes can change the id.
4. Topology diff: diff(old, new) returns added displays, removed displays, changed displays with a precise set of what changed (name, origin, size, scale, refresh, primary, active, available, hdr), whether the active display changed, and whether the primary changed. Properties: diff(a, a) is empty; added and removed sets are exactly the set differences by id; every changed field is reported and nothing else. Also provide requires_stream_reset(diff, current_display): true when the currently streamed display was removed, became unavailable, or changed size, refresh or scale. Revision handling: a received topology with a revision not newer than the stored one (u32 serial arithmetic) is ignored by the helper that applies updates, and this is tested including wraparound.
5. Virtual desktop bounds: bounding rectangle of all available displays (handle negative origins and gaps); returns None for an empty topology. Use i64 intermediates and report overflow as an error.

PART 2. Input and cursor coordinate math (in racc-topology, pure functions with exhaustive tests)

Implement exactly the conventions below, document them in docs/TOPOLOGY.md, and record the rounding decisions in an ADR.

1. Letterbox: given a container (width and height in device pixels, u32) and a stream size (u32 width and height, nonzero), compute the integer rendered rectangle preserving aspect ratio, centered. Use integer math with u64 intermediates: if container_w * stream_h <= container_h * stream_w the video is width-limited (rect width = container width, rect height = round(container_w * stream_h / stream_w)), otherwise height-limited with the symmetric formula; offsets are floor((container - rect) / 2). The rectangle is never zero-sized unless the container is zero-sized (return None). Test pillarbox, letterbox, exact fit, tiny containers (1 by 1), extreme aspect ratios, and very large sizes without overflow.
2. Pointer normalization (viewer side): given the pointer position in device pixels and the rendered rectangle, return the fixed-point (u, v) pair as u16 values in 0..=65535 where u = round(clamp((px - rect.x) / rect.w, 0, 1) * 65535) and likewise v. Provide two policies: Clamp (pointer outside the rectangle is clamped to the nearest edge; used while dragging or with captured mouse) and Reject (pointer outside the rectangle returns None; used for hover). A pointer at exactly the right or bottom edge counts as outside for Reject and maps to 65535 for Clamp. The input is the actual rendered video rectangle, never the window.
3. Host physical pixel: given a Display and (u, v), compute X = display.x + round_half_up(u * (width - 1) / 65535) using exact integer arithmetic (X offset = (u * (width - 1) * 2 + 65535) / (2 * 65535) using u64), and likewise Y. When width or height is 1 the offset is 0. u = 0 maps to the display origin and u = 65535 maps to the last pixel (width - 1).
4. Windows virtual-desktop absolute: given host physical pixel (X, Y) and the virtual desktop bounds V, compute abs_x = round((X - V.left) * 65535 / (V.width - 1)) and abs_y likewise as integers in 0..=65535 (0 when V.width or V.height is 1). Document that this serves the MOUSEEVENTF_VIRTUALDESK absolute mode, that Windows' exact internal rounding and DPI handling are NOT verified here, and that SetCursorPos with physical coordinates in a per-monitor-DPI-aware process is the recommended alternative if corner accuracy fails on hardware. The actual choice is made in M6 or M7 after the hardware checks.
5. macOS points: a host-local type HostDisplayGeometry carrying the logical rectangle in points (x_pt, y_pt, width_pt, height_pt as f64) supplied by the host's own OS queries (it is NOT part of the wire topology and must not be derived from the rounded scale factor). Function: point = (x_pt + (u / 65535) * width_pt, y_pt + (v / 65535) * height_pt). Test with Retina and non-Retina examples including a secondary display with a negative origin.
6. Relative mouse motion is never scaled by these functions; document that explicitly.
7. Cursor position mapping (viewer side): given the cursor datagram position (x, y host physical pixels relative to the streamed display's top-left), the Display and the rendered rectangle, compute the screen position by scaling position and, for the bitmap, scale and hotspot offset (hotspot in bitmap pixels scaled by rect.w / display.width). Return a rectangle in device pixels for drawing; document rounding.
8. A stream smaller than the display (for example a 480p stream of a 1080p display) must produce identical (u, v) to host pixel results as a native-size stream, because the rendered rectangle always covers the whole display. Add explicit tests.

PART 3. racc-telemetry

1. RateWindow: a bounded ring of fixed time buckets that tracks events and bytes per second over a configurable window (default 1 second in 100 ms buckets). Time injected. Memory is fixed. Handles time moving forward in large jumps (clears buckets) and tolerates a timestamp that goes backward (ignores it and counts it in a diagnostic counter instead of panicking).
2. RttEstimator: last sample, minimum over a configurable window, smoothed RTT and RTT variation using the standard RFC 6298 style update (alpha 1/8, beta 1/4, documented), and a simple jitter figure. PingTracker: generates no randomness itself (the caller supplies nonces); records outstanding pings in a bounded table (at most 16; the oldest is dropped when full and counted); match_pong(nonce, echo_ts, now) returns the RTT sample; expire(now, timeout) removes and counts timed-out pings and returns how many timed out. Unknown or duplicate pongs are counted and ignored.
3. Enums and structs: ConnectionState (Disconnected, Connecting, Handshaking, Connected, Reconnecting), PathKind (Unknown, Direct, Derp), CodecKind (H264), DecoderKind and EncoderKind and CaptureBackendKind (with an Unknown variant each; map to and from the proto StatsReport numeric codes, unknown codes map to Unknown, never panic), plus SessionSnapshot (connection state, path, rtt figures, loss fraction and frame-loss fraction as plain numbers supplied by the caller, bitrate, fps, codec, decoder, current epoch) and HostSnapshot (cpu percent x10 as in the wire, capture backend, encoder, width, height, refresh, target and actual bitrate) with conversion from a received proto StatsReport. Host GPU usage is NOT included (dropped from scope).
4. Event log: Event { id (monotonic u64 that never repeats within a process), ts_us, kind, detail (short string, bounded length) } with EventKind covering at minimum: ConnectionEstablished, ConnectionLost, DisplaySwitch, StreamReset, DecoderReset, QualityAdjustment, PacketLossEvent, CaptureLost, CaptureRecovered, EncoderFallback, PermissionMissing, PeerRejected, Paused, Resumed. EventLog is a ring buffer of capacity EVENT_LOG_CAPACITY = 256 (drops the oldest, counts drops) with events_since(id) so a UI can poll cheaply without copying everything, and a cheap Clone-able snapshot.
5. TelemetryHub: owns the above, exposes small update methods (record_rtt, record_frame, record_bytes, record_loss, set_path, set_connection_state, push_event, update_host_stats) all taking now, and a snapshot() that returns an immutable, cheaply cloneable struct containing session snapshot, host snapshot and the last N events. No locks, no threads, no I/O inside; document how the core (later) will publish snapshots to the UI at about 4 Hz without blocking video.

TESTS (all required)
1. Golden vectors for the stable-id function and for the topology to TopologyAnnounce conversion (bytes via racc-proto encode_frame) including a negative-origin display.
2. Table-driven coordinate tests with explicit expected numbers for: single 1920x1080 display; two displays side by side with the second at negative x; stacked displays; mixed DPI (a 3840x2160 display at scale 1500 beside a 1920x1080 at 1000); a display whose origin is not at (0, 0) in virtual space; 480p and 720p streams of 1080p and 4K displays; letterbox and pillarbox; edge pixels (u = 0, u = 65535, 1-pixel-wide display); the macOS point mapping; the Windows absolute mapping at all four corners of the virtual desktop for the two-display and three-display layouts (your laptop has three 1920x1080 displays: include a three-wide layout).
3. Property tests (proptest, default case count fast; also run once with PROPTEST_CASES=10000): host pixel always inside the display; monotonic in u and v; endpoints exact; Windows absolute mapping always in 0..=65535 and monotonic; letterbox rectangle fits in the container, is centered within one pixel, preserves the stream aspect ratio within one pixel of rounding, and is never zero-sized for nonzero inputs; normalize then to-host-pixel lands within ceil(display_w / rect_w) + 1 pixels of the intended position; topology diff properties; proto conversion round trips; telemetry structures never panic on arbitrary sequences of operations including time going backward and huge time jumps; EventLog never exceeds capacity; PingTracker never exceeds 16 entries.
4. Validation rejection tests for every Topology error variant and every unknown-code mapping.
5. Revision serial-arithmetic tests including u32 wraparound.

DOCUMENTATION
- docs/TOPOLOGY.md: domain model, validation rules, stable-id encoding and limitation, diff semantics, and all coordinate conventions with worked numeric examples (include at least the three-display layout and the mixed-DPI layout).
- docs/TELEMETRY.md: types, rolling windows, RTT algorithm, event kinds and capacity, snapshot model and the intended publish path.
- ADRs: 0015 (domain types separate from wire types; DisplayId nonzero; revision serial rule), 0016 (coordinate conventions and rounding decisions; Windows absolute mapping unverified; SetCursorPos fallback), 0017 (telemetry is sans-IO with immutable snapshots; event log capacity and ids). Content from this prompt only.
- docs/HARDWARE.md: add human checks (all HUMAN-PENDING): click all four corners and center of every monitor on each Windows PC and read back the cursor position (GetCursorPos or an on-screen readout) comparing the absolute-mapping path and the SetCursorPos path; the same with mixed DPI scaling set to 125% and 150%; macOS corner checks on the Mac for its built-in display (and an external display if available).
- Update docs/PROGRESS.md (M3a status, test counts, labels, final report) and docs/OPEN_QUESTIONS.md.

HARD CONSTRAINTS
- Touch only crates/topology, crates/telemetry, docs, and scripts or configuration strictly needed. No networking, sessions, state machines for connect or switch, capture, encode, decode, UI or Tailscale code. Do not implement quality adaptation.
- Do not modify racc-proto or racc-net. If you find an issue there, write a failing test, record it in docs/OPEN_QUESTIONS.md and report.
- Dependencies exactly as stated. No unsafe. No panics from external input.
- Do not fabricate results. Pure-logic tests are VERIFIED-RUN on this machine but say nothing about real display or injection behavior, which stay HUMAN-PENDING. Label verification per DEV_SETUP.md.
- Do not start M3b.

ACCEPTANCE CHECKS (each needs evidence)
1. cargo fmt --all -- --check exits 0.
2. cargo clippy --workspace --all-targets -- -D warnings exits 0.
3. cargo test --workspace exits 0; report test counts per crate.
4. PROPTEST_CASES=10000 cargo test -p racc-topology -p racc-telemetry exits 0.
5. RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps exits 0.
6. cargo deny check (standard flags) exits 0; cargo tree for both crates (normal edges) shows only racc-proto.
7. scripts/check-layering, scripts/check-features and scripts/check-all (.sh and .ps1) exit 0.
8. cargo check for x86_64-pc-windows-msvc and x86_64-apple-darwin exits 0 (COMPILE-ONLY).
9. The table-driven coordinate tests cover every case listed in TESTS item 2, with explicit expected values.
10. Golden vectors for stable ids and for the topology wire conversion exist in tests and in docs/TOPOLOGY.md and agree.
11. All required docs and ADRs 0015 to 0017 exist; HARDWARE.md has the new human checks; PROGRESS.md and OPEN_QUESTIONS.md are updated.
12. Commits are small, logical, authored as rkohnmn with no trailers; normal push to origin main succeeded (or BLOCKED-HUMAN with the exact error); no force push.

STOP CONDITIONS
- Stop after all checks pass and the final report is written. Do not start M3b.
- If the same check fails three times for the same reason, stop, record cause and attempts in docs/OPEN_QUESTIONS.md, and report blocked. Do not loop.
- If a specification above is contradictory or impossible, record the conflict in docs/OPEN_QUESTIONS.md, implement the closest safe behavior and flag it in the report.

FINAL REPORT (append to docs/PROGRESS.md and give as your final message)
- The 12 acceptance checks as PASS, FAIL or BLOCKED-HUMAN with commands and short output excerpts, including test counts and the 10000-case result.
- Everything created or changed.
- The worked coordinate examples you verified for the three-display layout (corner and center mappings, host pixel and Windows absolute values).
- Anything UNVERIFIED, COMPILE-ONLY, HUMAN-PENDING or BLOCKED-HUMAN, and why.
- Open questions added, including anything from the M2.5 recommendation that affects later types.
- One paragraph on what M3b (session state machines: viewer and host lifecycle