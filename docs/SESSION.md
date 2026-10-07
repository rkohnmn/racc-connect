# Session lifecycle

`racc-session` owns deterministic host and viewer lifecycle logic. It has no sockets, capture devices, codecs, or UI dependencies beyond the protocol and topology crates. Callers pass control messages and operation results into the state machines, then execute the typed actions they return. All deadlines use caller-supplied monotonic microseconds.

## Host flow

1. The host accepts a protocol-compatible Hello that advertises H.264, replies with HelloAck and the current topology, and starts capture on the selected available display.
2. Capture completion returns an encoder configuration action with a bounded output size. Output fits within 1920×1080, preserves the display aspect ratio, and is raised to 480 pixels high where the bounds allow it.
3. After encoder configuration succeeds, the host advances the stream epoch, announces StreamReset, and requests an IDR. Capture and encoder completions carry operation ids; stale results from superseded work are ignored.
4. A monitor switch validates the requested display. The host keeps an in-flight switch when topology changes leave its target available; an unavailable target receives a terminal DisplayNotFound result. Successful changes configure the encoder before announcing a new epoch.
5. Capture loss pauses encoding, announces a paused reset, and retries at 50, 100, 200, 400, 800, then 1000 ms intervals. Recovery resumes encoding only while the viewer is connected and has not paused video. If hardware rebuild fails repeatedly, OpenH264 is selected; if that rebuild also fails, encoding is paused and the host emits an EncoderFailed event/reset.
6. A control disconnect starts the configured stop deadline (five seconds by default). Reconnection cancels the deadline and sends a fresh topology and keyframe reset. Expiration stops capture and closes the session.
7. Path degradation asks the host-owned quality controller to step down one tier and forces a keyframe. Return to a direct path requests gradual restoration; the quality controller owns actual bitrate and tier policy.

## Viewer flow

- The viewer sends Hello after transport connection, then waits for HelloAck, topology, and an initial stream reset.
- An active (`Ok`) reset is accepted only for the current topology revision, a matching request and display, H.264 at 30 fps, and dimensions within the output bound. Explicit selected-display invalidation resets are also accepted so the viewer can retire that stream epoch. Stale epochs and mismatched switch replies are ignored.
- During a display switch, decoder reset, or resume, the renderer keeps the last good image. Frames from stale epochs are dropped. The viewer promotes a new image only after a valid keyframe from the new epoch decodes.
- Packet loss and decoder errors request a keyframe at most once per 200 ms. Decoder errors retain the last good image.
- Control disconnect enters bounded exponential reconnect backoff (50 ms to one second). Pause state survives reconnect; a paused viewer resends PauseVideo and remains paused after topology until the caller resumes. Closing emits SessionEnded, sends Goodbye, and closes transport.

## UI and renderer boundary

The state machines exchange control messages, metadata, typed work actions, and lifecycle events only. Video frames and pixel data do not pass through `racc-session` or the UI event bus. A caller supplies decoded-frame metadata to `ViewerSession` and applies its `VideoDisposition` directly at the renderer handoff.

## Verification limits

The session transitions are covered with deterministic unit tests that inject capture, encoder, topology, and transport outcomes. These tests do not establish real capture, codec, Tailscale, input, or display-switch behavior. Platform runtime checks remain on the human checklist in `docs/HARDWARE.md`.
