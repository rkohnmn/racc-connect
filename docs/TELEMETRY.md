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
- `HostSnapshot` maps CPU ×10, capture backend, encoder, dimensions, refresh and target/actual bitrate from `StatsReport`. GPU usage is absent.

**Wire limitation:** current `racc_proto::StatsReport` has capture backend and encoder numeric enums, but no decoder field, and the proto decoder rejects unknown enum codes before it can form a `StatsReport`. `DecoderKind` therefore has a local reserved numeric mapping for future use; v0 `HostSnapshot::from_stats_report` leaves the decoder `Unknown`. No proto change was made in M3a.

## Event log

The event kinds cover connection established/lost, display switch, stream reset, decoder reset, quality adjustment, packet loss, capture lost/recovered, encoder fallback, permission missing, peer rejected, pause, and resume. Each event has a process-wide increasing u64 id, injected monotonic timestamp, kind, and UTF-8 detail limited to 160 bytes. Each log retains the newest 256 events and counts dropped oldest entries. `events_since(id)` returns only newer events; `EventLogSnapshot` is an immutable `Arc`-backed slice whose clones are cheap.

## Publication path

The later core should call small hub update methods from its own event-handling work, then publish `TelemetrySnapshot` to the UI at about 4 Hz. UI consumers clone immutable scalar snapshots and the Arc-backed event snapshot. No telemetry lock is held by the render loop, and pixels never travel through this path.
