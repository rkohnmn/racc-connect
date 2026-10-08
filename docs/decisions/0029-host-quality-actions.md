# ADR 0029: Host quality decisions become encoder and stream actions

- Status: Accepted
- Date: 2026-10-07

## Context

The host quality controller had deterministic policy tests, but `HostRuntime` forwarded viewer feedback without applying any decisions. Resolution changes must follow the session’s encoder-completion and stream-epoch lifecycle so the viewer is not told that a new stream is active before the encoder accepts it. Bitrate trims also need a concrete platform-adapter action.

## Decision

Keep the thresholds and tier behavior already implemented by `racc-session::QualityController`: 2% packet loss or 5% frame loss for a 3-second dwell; RTT above three times the observed minimum for 5 seconds; two sender-queue overflows in 2 seconds; stable recovery below 0.5% loss and at most 1.5 times the RTT baseline for 20 seconds; at most one tier step-up per 30 seconds; bitrate trims in 10% steps down to 70% before tier reduction. The host/display cap and 480p30–1080p30 limits remain in force.

`HostRuntime` applies feedback only from the authorized connection and current stream epoch. Its v1 report conversion uses loss permille to basis points, RTT milliseconds to microseconds, and the lowest nonzero session RTT as the controller baseline. The host owns the resulting bitrate and tier decisions; viewer `SetQuality` is limited to Auto, 480, 720, or 1080 and is clamped by available host/display output dimensions.

A bitrate decision becomes an `EncoderAction::SetBitrate`. A tier decision requests an encoder-only `Configure` through `HostSession`; after successful completion, the session advances the epoch, sends the matching `StreamReset`, and forces an IDR. Failed or superseded quality configurations do not advance the active epoch; the runtime restores its prior policy target and bitrate when configuration fails.

## Consequences

- Adapter consumers must apply `SetBitrate` and report encoder configuration completion for quality-tier changes.
- A monitor switch continues to use the existing capture and encoder lifecycle. The new quality request does not move capture.
- The policy uses no invented decode-time, encoder-lag, or dropped-frame threshold. `ViewerReport` v1 does not supply queue-overflow telemetry.
- Unit tests establish policy and typed action sequencing only. Live host encoder, packet sender, viewer report generation, UI event-log publication, and hardware adaptation remain unverified.