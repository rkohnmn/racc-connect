# Viewer implementation status

The app has a deterministic `--fake` mode and a discovery-first live viewer mode. The live source path now connects Tailscale discovery, peer capability probes, the viewer runtime, platform decoder factory, latest-frame handoff, native wgpu upload, session controls and metadata events. Source integration does not establish that a remote host session works on the owner's PCs.

## Device discovery and session startup

Normal launch starts a bounded discovery worker using the Tailscale LocalAPI client. The app lists online and offline peers and marks peers that answer the project handshake as host-capable. Selecting a peer requires an online host-capable record and a valid Tailscale address pair in the same IP family. The app then creates `ViewerRuntime` with the peer control endpoint and a local Tailscale video bind address. The Settings UI and local-session panel are source-wired to the Windows named-pipe and macOS Unix-socket host-agent clients for status, allowlist and approval actions. Target-OS runtime behavior remains unverified, and Windows currently returns Unavailable for hosting enable/disable because a safe lifecycle toggle is not implemented.

`ViewerRuntime` owns the TCP control connection, UDP video receiver, M3b viewer session, decoder worker and metadata telemetry. It processes handshake, topology, display switch, pause/resume, keyframe, input, clipboard policy and report messages outside the UI event loop. Reconnect uses the session backoff policy. The UI receives commands, immutable metadata snapshots and events; video pixels never enter the `CoreEvent` or `CoreSnapshot` bus.

## Decode and render handoff

The Windows app selects the Media Foundation H.264 decoder. It returns bounded NV12 planes for the app's wgpu renderer to upload into persistent Y and UV textures. This CPU-plane path does not currently use a D3D11 decoder device manager or DXVA surface interop, so hardware decode is not established. The macOS app factory selects the VideoToolbox decoder and uses the same NV12 render path; the macOS app target passes strict Clippy from the Windows coding host, but Apple linking and runtime are unverified.

Decoded frames publish to a bounded latest-frame store, separate from UI events. The renderer preserves the aspect ratio, letterboxes the image, and retains the last good frame through a display reset until matching decoder readiness metadata permits replacement. The NV12 shader assumes limited-range BT.709 because colorimetry is not carried in the protocol; visual color validation remains pending. Telemetry updates rebuild only their cached widget subtree, although iced still lays out and redraws the window.

### Opt-in frame timing overlay

Start the app with `--debug-latency` (for example, `cargo run -p racc-app -- --discover --debug-latency`) to show the latest frame ID, the raw host capture timestamp, the viewer-local duration of the decoder call, and the interval at which the app observed new frame IDs. The timing metadata stays on `VideoFrame` and the separate `FrameSource` handoff; it is not added to `CoreEvent` or `CoreSnapshot`. Synthetic fake frames show unavailable values for remote capture and decode timing.

The video timestamp is a wrapping host monotonic counter. The app does not estimate capture age because it has no host/viewer clock-offset measurement; the overlay says so instead of subtracting unrelated clocks. Its frame interval is measured when the UI's 30 Hz frame tick observes a new source frame. It is an app-observed update interval, not GPU completion or a physical monitor-present interval. The overlay is diagnostic context only and does not satisfy the M7 decode/upload/render, switch-time, input-timing, or glass-to-glass measurement requirements.

Fake mode continues to draw its synthetic pattern and to exercise device, display, reset, quality, event and telemetry flows. It does not prove capture, encoding, network behavior, decode correctness, or physical presentation.

## Cursor and input

The app includes separate cursor-shape/position presentation and viewer input helpers. The Windows host has a bounded input worker that validates authenticated connection, epoch, display and announced topology before calling the platform injector; release paths are covered with a fake injector. The app's viewer-side input controls and helper source are integrated, but real key and pointer events have not been verified between PCs. Both virtual-desktop absolute injection and `SetCursorPos` are implemented behind `RACC_POINTER_INJECTION_METHOD`; the existing virtual-desktop method remains a provisional default. The four-corner/center accuracy comparison, keyboard-layout coverage and measured input latency remain HUMAN-PENDING.

Mac capture follows M9's cursor-in-video rule: both ScreenCaptureKit and CGDisplayStream include the pointer in captured frames, while the host sends hidden-cursor metadata per stream epoch so the viewer does not draw a duplicate. This is source-integrated but remains HUMAN-PENDING on Monterey hardware.

## M8 clipboard and telemetry boundary

Portable text clipboard policy includes bounds, sequence/origin tracking, loop prevention, rate limiting and fake round trips. Windows and macOS viewer workers apply remote text to the local OS clipboard and read local text only while clipboard sync is explicitly enabled for the active session; the host adapters use the same gate. The owner authorized Windows text sync up to 512 KiB to the selected Tailscale peer. A Windows OS clipboard test against a fake remote passed with fixed harmless text. Real peer-to-peer round trips and Mac runtime behavior remain HUMAN-PENDING.

The viewer records RTT, loss, frame loss, received bitrate, delivered FPS, codec, decoder, epoch and events. The app passes the selected peer's Direct/DERP classification into the runtime and refreshes it when the bounded Tailscale discovery worker completes a status refresh (currently every 15 seconds). A core loopback test verifies the route reaches the immutable telemetry snapshot. Windows and macOS foreground hosts now queue one-second StatsReport updates for the active backend, encoder, resolution, refresh, target bitrate and measured sent UDP payload bitrate. The Mac host CPU field remains zero/unknown until a machine-wide sampler is implemented. Per-session remote clipboard enable/disable signaling uses type 18, introduced in protocol v3; the current wire protocol is v4. Type 19 quality adjustments now enter the bounded event log, whose monotonic timestamps are rendered as elapsed `T+` time. The live telemetry sidebar and its pacing still require source-to-display and hardware checks.

## Current verification and remaining checks

- **TESTED-FAKE:** app reducer, host input validation/release, clipboard policy, bounded queues, session state, topology math and core runtime tests pass as recorded in `docs/PROGRESS.md`.
- **COMPILE-ONLY:** the full Windows workspace target passes `cargo check --workspace --all-targets --target x86_64-pc-windows-msvc --offline`.
- **COMPILE-ONLY:** macOS app strict Clippy passes for `x86_64-apple-darwin`; the full Apple host/encode target remains blocked by the missing target C++ compiler required by OpenH264.
- **HUMAN-PENDING:** real DDA capture, hardware encoder and decoder path, two-PC/Tailscale sessions, monitor switching, frame cadence, pointer accuracy, keyboard layout and release behavior, path telemetry, clipboard round trip, and resource/latency measurements.

See `docs/HARDWARE.md`, `docs/INPUT.md`, `docs/MACOS.md`, `docs/PROGRESS.md`, and `blocked.md` for detailed checklists and blockers.
