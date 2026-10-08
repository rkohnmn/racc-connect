# Goal M3b: session state machines (`racc-session`)

Save as `docs/goals/M3b.md`.

**Run after:** M3a complete and pushed.
**Revisit before running:** read the M2.5 recommendation in `docs/TRANSPORT_AUDIT.md`. If it changes how keyframe requests or retransmits flow between the receiver and the session layer, adjust the "Transport inputs" section below first.

## Human checklist
1. Working tree clean, `main` synchronized with `origin/main`.
2. No hardware needed (pure logic).

## Launcher
```
/goal Complete the work specified in docs/goals/M3b.md. Read AGENTS.md, docs/goals/COMMON.md, docs/DEV_SETUP.md and docs/PROGRESS.md first. Done only when every acceptance check in docs/goals/M3b.md passes with evidence, then stop. Do not start the next goal.
```

## Prompt
~~~text
GOAL: M3b. Implement the sans-IO session state machines in crates/session (package racc-session): the viewer session, the host session, stream parameter selection, display switching with epochs, pause and resume, heartbeat, reconnect, and failure recovery. No sockets, threads, clocks, capture, encode, decode or UI. Everything is a deterministic function from (state, input, now) to (new state, actions).

COMMON.md applies in full. Read it, AGENTS.md (sections 4, 5.3, 6, 8), docs/PROTOCOL.md, docs/TRANSPORT.md, docs/TRANSPORT_AUDIT.md, docs/TOPOLOGY.md and docs/TELEMETRY.md first.

PREFLIGHT
Clean tree; scripts/check-all green; M3a and M2.5 complete in docs/PROGRESS.md. If scripts/check-hygiene is missing, create it per COMMON.md section 6.

DEPENDENCIES
racc-session runtime dependencies: racc-proto, racc-topology, racc-telemetry (paths) only. Dev: proptest. It must NOT depend on racc-net (transport signals arrive as plain inputs). #![forbid(unsafe_code)].

DESIGN (fixed; implement and document in docs/SESSION.md)

Shape: ViewerSession and HostSession, each with handle(input, now) -> Vec<Action> (or a bounded output buffer), plus accessors for current state. Time is injected microseconds. Inputs and Actions are plain enums, cloneable and debuggable. No hidden global state. You choose names; the variants below are the required content.

Constants (named, documented, tested): HEARTBEAT_INTERVAL_MS = 1000, HEARTBEAT_TIMEOUT_MS = 5000, HANDSHAKE_TIMEOUT_MS = 5000, SWITCH_TIMEOUT_MS = 2000 (one retry, then fail), FIRST_KEYFRAME_NUDGE_MS = 1000, FIRST_KEYFRAME_RESET_MS = 5000, RECONNECT_BACKOFF_MS = [500, 1000, 2000, 4000, 5000] (last value repeats), HOST_ORPHAN_TIMEOUT_MS = 10000, FORCE_IDR_MIN_INTERVAL_MS = 100, CAPTURE_RECOVERY_BACKOFF_MS = [50, 100, 200, 400, 800, 1000], ENCODER_FAILURE_THRESHOLD = 3 failures within 10 s.

Epochs: the host assigns a strictly newer epoch (u16, wrapping, serial arithmetic) to every StreamReset it sends. The viewer adopts only a StreamReset whose epoch is strictly newer than the newest it has seen; others are ignored and counted. Packets and requests carrying other epochs are ignored by the layer that owns them.

Single viewer per host in v0: a second Hello while a session is active gets HelloAck(Busy). Record this in an ADR.

Viewer states: Idle, Connecting, Handshaking, AwaitingTopology, AwaitingStream, Streaming, Switching, Paused (window hidden), Resuming, Reconnecting, Ended(reason).
Viewer inputs: Connect(target), Disconnect, SelectDisplay(id), SetQuality(pref), SetVisible(bool), ControlConnected, ControlClosed(error), ControlMessage(msg), Tick, FrameDecoded(epoch, is_key), DecoderError, NeedKeyframe (from the receiver), UserInput(event).
Viewer actions: ConnectControl(target), CloseControl, SendControl(msg), ResetReassembler(epoch), ConfigureDecoder(w, h), ResetDecoder, HoldLastFrame, ReplaceFrameAtomically, UpdateInputMapping(display, epoch), ScheduleReconnect(at), Telemetry(event), Ui(notification), ForwardInput(event) (only emitted when allowed).
Viewer rules:
- Handshake: Hello, expect HelloAck. Ok then await TopologyAnnounce, then StreamReset(Ok). UnsupportedVersion or NotAuthorized end the session with no automatic reconnect. Busy retries after 5 s.
- Heartbeat: Ping every second; Pong feeds the RTT estimate (racc-telemetry); five seconds without any Pong or message counts as control loss.
- Control loss: enter Reconnecting with the backoff list, hold the last frame, emit overlay and telemetry events, and on reconnect redo the handshake and resume the previously selected display if it still exists (otherwise the host's active display).
- Display switch: SelectDisplay(id) sends SwitchMonitor{req_id, id} and enters Switching, holding the last frame. A StreamReset with matching req_id (or host-initiated, req_id 0, newer epoch) and Ok status: ResetReassembler(new epoch), ConfigureDecoder(w, h), wait for the first decoded keyframe of the new epoch; then, and only then, emit ReplaceFrameAtomically and UpdateInputMapping. A non-Ok status returns to Streaming on the previous display and notifies the UI. Rapid repeated selections supersede earlier ones (new req_id; replies for stale req_ids are ignored). Timeout: resend the same SwitchMonitor once (same req_id; the host must treat it idempotently), then fail back to the previous display.
- Input during Switching: pointer motion and pointer buttons are suppressed (not queued); key and wheel events continue (keys carry no geometry). After the atomic replace, pointer input resumes with the new mapping. Record this decision in an ADR.
- First keyframe nudge: after StreamReset Ok, if no decoded keyframe within 1 s send RequestKeyframe{epoch}; at 5 s reset the decoder and request again. NeedKeyframe inputs from the receiver become RequestKeyframe{epoch} (the receiver already rate-limits).
- Decoder error: ResetDecoder, mark need-keyframe, send RequestKeyframe, emit a DecoderReset telemetry event.
- Pause: SetVisible(false) sends PauseVideo, stops decode (Action), state Paused. SetVisible(true) sends ResumeVideo, arms need-keyframe in the same epoch, state Resuming until a keyframe decodes (with the same nudge rules). Hidden time never counts as control loss because Ping continues.
- SetQuality sends SetQuality; the resulting host-initiated StreamReset is handled like a switch to the same display.

Host states: AwaitHello, Authorizing, Active (streaming), Switching, CaptureLost, Paused, Ended.
Host inputs: PeerConnected(identity), ControlMessage(msg), ControlClosed, Tick, AuthorizationResult(approved, rejected), TopologyUpdated(topology), CaptureReady, CaptureFailed(kind), EncoderReady, EncoderFailed(kind), SoftwareEncoderReady, ForceKeyframeSignal (from sender backpressure).
Host actions: SendControl(msg), RequestAuthorization(identity), StartCapture(display), MigrateCapture(display), ConfigureEncoder(params), UseSoftwareEncoder, ForceKeyframe, StopStreaming, CloseControl, Telemetry(event), InjectInput(event) (the event is passed through; injection itself is not implemented here).
Host rules:
- Hello: validate protocol version; send HelloAck with status; on a new peer request authorization; rejection sends HelloAck(NotAuthorized) then Goodbye(NotAuthorized) and ends. Busy as above.
- After approval send HelloAck(Ok), the TopologyAnnounce, start capture on the active or primary display, configure the encoder with parameters from choose_stream_params, then send StreamReset(Ok, new epoch) and ForceKeyframe.
- SwitchMonitor: validate the display exists and is available (else StreamReset with DisplayNotFound, epoch newer, no capture change). Same req_id as the last handled request is idempotent: resend the same outcome without a new epoch. Otherwise MigrateCapture, reconfigure, new epoch, StreamReset, ForceKeyframe.
- RequestKeyframe: ignored if epoch is not current; rate-limited by FORCE_IDR_MIN_INTERVAL_MS (excess requests are coalesced into one forced IDR when the interval expires). ResumeVideo forces one keyframe immediately.
- ForceKeyframeSignal from the sender (queue overflow) forces an IDR under the same limit.
- InputEvent: pointer-absolute events are accepted only for the current epoch and display; other input is accepted while Active. Track pressed keys and mouse buttons; on session end, pause or control loss emit release events for everything still pressed (stuck-key protection). Test this.
- Capture lost: StreamReset(CaptureFailed or Paused, newer epoch) immediately; retry with CAPTURE_RECOVERY_BACKOFF_MS; on recovery reconfigure, send a TopologyAnnounce if the topology changed, then StreamReset(Ok, newer epoch) and ForceKeyframe. TopologyUpdated while streaming: announce it; if the current display was removed or changed size, refresh or scale (use racc-topology requires_stream_reset), perform a host-initiated reset (to the same display, or to the primary if it vanished).
- Encoder failure: after ENCODER_FAILURE_THRESHOLD failures in 10 s emit UseSoftwareEncoder and an EncoderFallback event; if the software encoder also fails send StreamReset(EncoderFailed) and stay in a retryable state.
- Control closed: stop streaming immediately; if no new Hello arrives within HOST_ORPHAN_TIMEOUT_MS release capture resources (Action) and return to AwaitHello. Heartbeat: answer Ping with Pong; treat 5 s of silence as closed.
- Goodbye handling on both sides.

choose_stream_params(display, quality_pref, host_caps) -> params: pure, tested. Tiers 480, 720, 1080 (fps always 30); Auto uses host_caps.default_height; never exceeds the display's height or host_caps.max_height (no upscaling: a 768-high display streams at most 768 scaled to a supported even height, document the rule); preserve aspect ratio; width and height even numbers; handle 4:3, 16:10, ultrawide and portrait; default bitrates 1500, 3500 and 7000 kbps unless host_caps overrides; bitrate hint from SetQuality respected within sane bounds (document them).

TESTS (all required)
1. Scenario harness: a deterministic in-memory control channel connecting a ViewerSession and a HostSession with scripted fake capture, encoder and decoder responders, configurable delays, drops and reorders of control messages, on a virtual clock. Scenarios: happy path connect to first frame; select second display; switch to a display that disappears mid-switch; rapid triple switch (stale replies ignored, epoch monotonic); switch timeout then retry then fail-back; host-initiated reset (resolution change) during streaming; host-initiated reset arriving during a switch; pause and resume; pause with the host sending frames anyway; control loss during streaming then reconnect and resume; control loss during a switch; rejection (NotAuthorized, no reconnect); Busy; version mismatch; capture lost and recovered with a topology change; encoder failure then software fallback then total failure; keyframe nudge and decoder reset timers; forced-IDR coalescing; orphan timeout; stuck-key release on disconnect.
2. Property tests (proptest, fast default; also once at 10000 cases): random sequences of valid and invalid inputs never panic; the viewer's adopted epoch is monotonic; both machines always sit in a valid state; the number of actions per input is bounded; no action storm (a Tick produces at most a small constant number of actions); reconnect backoff never exceeds the cap; the atomic replace never happens before a keyframe of the adopted epoch has decoded.
3. choose_stream_params table tests for each tier and display shape, including a 1366x768 display, a 2560x1080 ultrawide, a 1080x1920 portrait display, 3840x2160, and a 640x480 display.
4. Timer tests on the virtual clock for every named constant.

DOCUMENTATION
docs/SESSION.md: both state machines (state diagrams in text or Mermaid fenced blocks), input and action tables, every constant, the epoch rules, the switch lifecycle with timing, failure matrix mapping to AGENTS.md section 8, and the input-during-switch policy. ADRs: single viewer per host v0; input policy during switching; sans-IO session design (no dependency on racc-net). Update PROGRESS.md and OPEN_QUESTIONS.md. Add to docs/HARDWARE.md nothing new unless needed.

HARD CONSTRAINTS
No I/O, no threads, no clocks, no unsafe. Do not modify racc-proto or racc-net behavior. Do not implement quality adaptation (loss-driven tier changes), NACK or FEC, clipboard or capture. Do not start the next goal.

ACCEPTANCE CHECKS
B1 to B8 from COMMON.md, plus:
S1. PROPTEST_CASES=10000 cargo test -p racc-session passes.
S2. cargo tree -p racc-session -e normal shows only racc-proto, racc-topology, racc-telemetry (and their dependencies), and no racc-net.
S3. Every scenario in TESTS item 1 exists and passes; test names listed in the report.
S4. All named constants exist with the stated values and are asserted.
S5. docs/SESSION.md and the ADRs exist; the failure matrix covers every row of AGENTS.md section 8.

FINAL REPORT: COMMON.md section 14, plus the scenario list with pass status, the choose_stream_params results for the example displays, and one paragraph on where M2.6 or M4a starts.
~~~
