# Telemetry model

`racc-telemetry` is sans-I/O and depends at runtime only on `racc-proto`. It owns no locks or threads and never handles video frames. Core code can update it from network/capture events and publish cloned immutable snapshots to the UI about four times per second. Video rendering remains independent of telemetry publication.

## Rolling rates and RTT

`RateWindow` has a configurable fixed ring of 1–60 buckets; the default is ten 100 ms buckets (one second). It tracks saturating event and byte totals, derives rates from the configured duration, ignores backward timestamps while incrementing a diagnostic counter, and clears all buckets after a jump at least as large as its capacity. Storage is bounded by the requested count.

`RttEstimator` keeps at most 256 recent samples and computes a rolling-window minimum, last sample, SRTT, RTTVAR, and mean absolute adjacent-sample difference (jitter). The first sample initializes `SRTT = R` and `RTTVAR = R/2`. Later samples follow the RFC 6298-style coefficients: `RTTVAR = 3/4 * old_RTTVAR + 1/4 * abs(old_SRTT - R)`, then `SRTT = 7/8 * old_SRTT + 1/8 * R`.

`PingTracker` accepts caller-generated nonces and stores at most 16 outstanding pings. When full it drops and counts the oldest entry. Pongs must match both nonce and echoed timestamp; unknown, duplicate, malformed, or backwards-time pongs are counted and ignored. Expiration removes entries whose age reaches the caller-provided timeout.

## Snapshot and code types

- `ConnectionState`: disconnected, connecting, handshaking, connected, reconnecting.
- `PathKind`: unknown, direct, DERP.
- `CodecKind`: unknown or H.264.
- Capture backend and encoder kinds map to/from their existing `StatsReport` numeric codes. Unknown numeric values map to `Unknown`; zero maps back for `Unknown`.
- `SessionSnapshot` carries status, RTT values, caller-supplied packet/frame loss fractions, measured bitrate, observed FPS, codec, decoder, and current epoch. Fractions are finite-clamped to `0..=1`.
- `HostSnapshot` maps machine CPU ×10, process CPU share ×10, capture backend, encoder, dimensions, refresh and target/actual bitrate from `StatsReport`. GPU usage is absent.

**Wire limitation:** protocol v5 `racc_proto::StatsReport` has capture backend and encoder numeric enums, but no decoder field, and the proto decoder rejects unknown enum codes before it can form a `StatsReport`. `DecoderKind` therefore has a local reserved numeric mapping for future use; `HostSnapshot::from_stats_report` leaves the decoder `Unknown` until a decoder source is added deliberately.

## M8 source and wiring status

The viewer runtime updates telemetry from its control and video workers and publishes the latest bounded snapshot through `poll_telemetry` about four times per second. The iced app polls this slot on its 250 ms UI telemetry tick and maps it into the existing telemetry view model; no frame data enters this path. This wiring has unit/loopback coverage, but the displayed values have not been compared against real Tailscale or hardware measurements.

| Field | Current source and wiring | Remaining limit |
|---|---|---|
| RTT and RTT baseline | Control Ping is sent about once per second; matching Pong nonce/timestamp updates `PingTracker` and `RttEstimator` | No comparison against Tailscale measurements has been recorded |
| Packet loss and frame loss | `VideoReceiver::reassembly_stats` feeds rolling missing-fragment and whole-frame loss estimates | Only synthetic/loopback coverage is available; real path quality remains unverified |
| Decode p95 | Bounded per-access-unit decoder call durations are sampled for the active epoch and summarized at the one-second ViewerReport cadence | This is decoder call duration, not glass-to-glass latency |
| Dropped frames | Reassembly whole-frame gap deltas plus decoder input-queue drops are accumulated per epoch and reported once per second | Partial-frame loss is represented in loss estimates, not counted as a known full-frame drop |
| Target bitrate | Current `HostRuntime` quality tier and encoder configuration actions | Real host encoder application and sustained bitrate measurement remain unverified |
| Actual bitrate and FPS | Annex B payload bytes and successfully decoded frames feed rolling telemetry | Bitrate excludes UDP/IP overhead; no hardware measurement was recorded |
| Codec, decoder and epoch | Accepted `StreamReset` selects H.264 and epoch; successful frames refresh stream activity; app selects decoder kind locally | Decoder kind is not present in protocol v5 `StatsReport` |
| Direct/DERP path | The bounded Tailscale peer refresh supplies the selected peer path to ViewerRuntime, which updates `TelemetryHub` | Refreshes run about every 15 seconds; real path classification remains unverified on the three PCs |
| Host CPU | Machine-wide usage comes from Windows `GetSystemTimes` or macOS aggregate Mach CPU ticks. Process share is separately sampled from Windows `GetProcessTimes` or macOS `getrusage(RUSAGE_SELF)`, normalized by elapsed time × logical processor count | Samples warm up to Unknown; target-machine readings remain unverified |
| Capture backend, encoder, resolution and refresh | Incoming `StatsReport` maps into `HostSnapshot`; host-agent sender emits reports | Real hardware selection and values remain unverified |
| Quality and lifecycle events | Host bitrate/tier outcomes are sent as protocol type 19 `QualityAdjustment`; ViewerRuntime accepts only the active epoch and appends the bounded event log | Event timestamps are viewer-local monotonic offsets, rendered as `T+`; real path behavior remains unverified |
| Capture lost/recovered, encoder fallback, pause/resume | Host sends a closed-code protocol type 20 `HostEventReport`; the viewer maps it to a fixed local event label | Synthetic/loopback coverage only; no host lifecycle event has been observed on a real peer |
| Clipboard state | The CONTROL toggle is available only for an active session with an OS adapter and peer text-clipboard capability. It reports this-session enabled/disabled state, status metadata, and the last completed transfer direction/age | Text is read and applied only while the session toggle is enabled; real PC-to-PC clipboard round trips remain human-pending |

While connected with an accepted stream epoch, the viewer sends `ViewerReport` once per second. The report carries RTT in milliseconds, packet and whole-frame loss in permille, decoder call p95 in milliseconds, and bounded dropped-frame counts. Host quality policy accepts viewer reports only for the authorized viewer and current epoch. The Windows foreground helper measures `dispatch_frame` duration (encode plus packetization and bounded sender enqueue); the macOS helper measures the VideoToolbox encode call. Neither is presented as glass-to-glass latency. Windows also reports bounded video-send queue overflow counts. The Mac paced sender exposes no queue-overflow counter, so that signal remains unavailable there. Timing is retained without an inferred quality threshold. Stale reports cannot adapt the stream. Source tests cover the policy and wiring; actual encoder behavior and Tailscale path quality remain unverified end to end.

## TESTED-FAKE loopback sample

`viewer_runtime_bridges_clipboard_and_reports_live_control_telemetry` in `crates/core/src/viewer_runtime.rs` passed with a fake host and loopback control connection. It exercised a connected H.264 stream at epoch 1, a manually injected DERP path, an RTT sample (the test asserts presence, not a fixed value), zero packet/frame loss, and a fake `StatsReport`: host CPU 23.7%, process CPU 12.3%, DXGI capture, Media Foundation hardware encoder, 1280×720 at 60 Hz, 3,500 kbps target and 3,100 kbps reported actual payload bitrate. The test also asserted a one-second `ViewerReport` for epoch 1. These are fixed synthetic inputs and loopback assertions, not measurements from a real machine or Tailscale route.
## Event log

The event kinds cover connection established/lost, display switch, stream reset, decoder reset, quality adjustment, packet loss, capture lost/recovered, encoder fallback, permission missing, peer rejected, pause, and resume. Each event has a process-wide increasing u64 id, viewer-local injected monotonic timestamp, kind, and UTF-8 detail limited to 160 bytes. The app renders the timestamp as elapsed `T+` time rather than local wall time. Each log retains the newest 256 events and counts dropped oldest entries. `events_since(id)` returns only newer events; `EventLogSnapshot` is an immutable `Arc`-backed slice whose clones are cheap.

## Publication path

The viewer core updates the hub from its control and video-receiver workers, then stores the latest immutable snapshot in a single-slot bounded slot for `poll_telemetry`. The app reads it on its 250 ms telemetry tick without a growing queue. No telemetry lock is held by the render loop, and pixels never travel through this path.
