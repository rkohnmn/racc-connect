# AGENTS.md

Instructions for AI coding agents (Codex, `/goal` loops, or any other) working in this repository. Read this file completely before changing anything. When this file and your own assumptions disagree, this file wins. When this file is silent or ambiguous, stop and record the question in `docs/OPEN_QUESTIONS.md` instead of guessing.

---

## 1. Project summary

A native, low-latency remote desktop and screen streaming application written primarily in Rust. It runs on three personal computers connected by Tailscale:

1. Windows 10 PC #1
2. Windows 10 PC #2
3. A 2015 Intel-based Mac (treat as an important compatibility target)

**Every machine can be both a host (shares its screens) and a viewer (watches another machine).** The app lists devices on the left, shows the selected device's monitors like chat channels, and shows the chosen monitor live in the main panel with control and telemetry around it.

The UI takes structural inspiration from Discord's desktop layout (device rail, channel-style sidebar, main panel, info sidebar, user panel). Documentation may name Discord only to describe that design inspiration. The name, logo, and assets must never appear in code, identifiers, UI strings, window titles, branding, package or file names, or shipped assets.

### Fixed product decisions (do not revisit without asking)

| Topic | Decision |
|---|---|
| Language | Rust for networking, capture, encode, decode, input, core logic and UI |
| Network | Tailscale only. No public relays, no STUN/TURN/ICE, no port forwarding |
| Video codec | **H.264 always available.** HEVC/AV1 must never be required (optional later only if explicitly requested) |
| Stream quality | **Host-chosen, 480p30 minimum, 1080p30 maximum.** Frame rate is 30 fps always. Nothing above 1080p |
| Audio | **Not supported.** Do not build audio capture, transport or playback. The audio button in the UI is a disabled placeholder |
| HDR | **Not supported.** Show nothing HDR-related except, optionally, a read-only label if the OS reports it |
| Clipboard | **Required.** Text first. Images and files are out of scope until text works and is tested |
| Host GPU usage telemetry | **Dropped.** Optional host CPU usage only. GPU usage may be added later behind a feature flag |
| App behavior | Background application. Tray icon, normal window brought forward on demand. No video decoding while the window is hidden or minimized |
| Video rendering | Native wgpu surface. Never HTML images, never JavaScript canvas, never a webview video element |
| Zero-copy | **Relaxed.** Avoid CPU copies where easy. One GPU copy or a plain NV12 upload to the GPU is acceptable at these resolutions (1080p30 NV12 is about 93 MB/s). True zero-copy is a later optimization |

---

## 2. Architecture

### 2.1 Process model

Two cooperating programs per machine.

```
host-agent   Always-on background process. Owns capture, encode, host input injection,
             host-side networking, clipboard sync on the host side. Windows: runs as a service.
app          The Discord-style UI. Runs in the user session. Owns decode, render, viewer-side
             input capture, device list, telemetry display. Can be closed without stopping hosting.
```

- The `app` talks to the **local** `host-agent` over a local IPC channel (named pipe on Windows, Unix domain socket on macOS) for settings and host status.
- The `app` talks to **remote** `host-agent`s over Tailscale using the project protocol (section 4).
- A machine that only views does not need its host-agent enabled, but it must be possible to turn hosting on from the UI.

### 2.2 Windows host specifics (read carefully)

A Windows service runs in session 0 and **cannot capture the interactive desktop**. Required design:

- The service supervises a **capture helper process** launched into the active console session (via the session token APIs), running with sufficient rights to survive UAC and the secure desktop.
- The helper owns capture, GPU encode, and input injection. The service restarts it on session change, logoff, lock, fast user switching, and crashes.
- Capture primary: **DXGI Desktop Duplication**. Fallback: **Windows.Graphics.Capture** (cannot see the secure desktop; document this limit in telemetry/events).
- `DXGI_ERROR_ACCESS_LOST`, mode changes, display sleep and wake must be handled by a recoverable state machine, not by crashing.
- The mouse cursor is **not** composited into video frames. Cursor shape, hotspot and position travel as separate metadata and are drawn by the viewer.

### 2.3 Encode and decode strategy

- **Windows encode default:** Media Foundation hardware H.264 encoder (MFT) with D3D11 texture input. This lets one code path reach NVENC, Intel QSV and AMD AMF through vendor MFTs. Configure for low latency: no B-frames, no lookahead, small rate-control window, low-latency mode via the codec API properties. Direct vendor SDKs (NVENC, AMF, oneVPL) are a later optimization behind the same trait.
- **Software fallback:** OpenH264 (BSD licensed). **Do not link x264 (GPL)** into this project.
- **macOS encode:** VideoToolbox `VTCompressionSession`, real-time mode, frame reordering disabled, low-latency rate control where available, Main profile.
- **H.264 profile:** encode Main (or Constrained Baseline when a platform requires it). Decoders must accept Baseline through High. Never emit B-frames.
- **Decode:** Windows via Media Foundation / DXVA H.264 decoder to an NV12 D3D11 surface; macOS via VideoToolbox `VTDecompressionSession` to `CVPixelBuffer`. Upload or share with wgpu as NV12 and convert to RGB in a shader. Simple path first.
- **Color conversion:** GPU (shader or video processor). Never per-pixel CPU conversion in the hot path.

### 2.4 macOS host specifics

- Primary capture: **ScreenCaptureKit** (needs macOS 12.3+). The 2015 Intel Mac most likely tops out at macOS 12 (Monterey). Confirm with `sw_vers` and record the result in `docs/HARDWARE.md`.
- Fallback capture: **CGDisplayStream** (deprecated, compatibility only).
- Request NV12 (`420v`) pixel format from ScreenCaptureKit to avoid conversion.
- Required permissions: Screen Recording (capture) and Accessibility (input injection). The app must detect missing permissions and show a clear in-UI explanation with a button that opens the right System Settings pane. It must not crash or silently show black video.
- The Intel GPU is weak. Default the Mac host to 720p30 and let the user raise it up to 1080p30 after a successful sustained test. Never default it to 1080p.
- Distribution needs code signing and notarization. Note this in `docs/PACKAGING.md`; do not attempt it autonomously.

### 2.5 UI toolkit

UI and video must share one wgpu context so the Discord-style layout can surround a native video surface without overlapping child windows.

- Candidates: **iced** or **Slint**. (egui is not preferred for the Discord-like look; a webview toolkit such as Tauri is **excluded** because it forces overlapping native windows for video.)
- **Milestone M4a is a spike that picks one.** Record the choice and evidence in `docs/decisions/0001-ui-toolkit.md`. Until then do not write final UI code.
- Acceptance for the spike: a 1080p30 NV12 test stream rendered inside the layout at a steady 30 fps with no visible stutter, while the telemetry sidebar updates at 4 Hz.

---

## 3. Repository layout

Create this Cargo workspace. Keep crate boundaries strict: lower crates never depend on higher ones.

```
Cargo.toml                workspace
AGENTS.md                 this file
docs/
  PROGRESS.md             running log of milestone status (update every session)
  OPEN_QUESTIONS.md       unresolved questions for the human
  HARDWARE.md             results of manual hardware tests
  PACKAGING.md            signing, installers, autostart notes
  PROTOCOL.md             human-readable wire spec, kept in sync with crates/proto
  decisions/              short ADRs, one file per decision
crates/
  proto/                  wire types, constants, serialization. No I/O. No platform code
  net/                    UDP video transport, TCP control channel, slicing, reassembly, pacing
  topology/               display model, topology diffing, coordinate math
  session/                session state machines (connect, switch, pause, recover), epoch logic
  telemetry/              counters, rolling stats, event log types
  capture/                Capture trait + Windows and macOS backends (cfg gated) + fake backend
  encode/                 Encoder trait + MF, OpenH264, VideoToolbox backends + fake backend
  decode/                 Decoder trait + Windows, macOS backends + fake backend
  input/                  input event model, viewer capture helpers, host injection backends
  clipboard/              clipboard sync (text first)
  identity/               Tailscale LocalAPI client: peers, whois, path (direct/DERP)
  core/                   wires everything together, exposes the UI event bus and command API
  host-agent/             binary: service, helper, local IPC server
  app/                    binary: Discord-style UI, wgpu surface, tray
  testkit/                loopback harness, packet loss/reorder/jitter injector, fake devices
```

---

## 4. Protocol (v0 draft, finalize in M1)

Two channels per session.

| Channel | Transport | Carries |
|---|---|---|
| Control | TCP, length-prefixed messages, `TCP_NODELAY` | Handshake, topology, SwitchMonitor, StreamReset, Pause/Resume, quality requests, input events, clipboard, telemetry, keyframe requests |
| Video | UDP | Encoded H.264 slices (Annex B), cursor metadata |

**Rationale:** TCP gives simple reliable control. Input starts on the control channel; if measured loss makes mouse movement stall, move mouse motion to UDP as latest-wins and keep key and button events reliable. Record any such change as an ADR.

### 4.1 Security model

- Bind **only** to the Tailscale interface address (`100.x.y.z`). Never to `0.0.0.0`.
- WireGuard already authenticates and encrypts. **No application-layer encryption** for the native protocol.
- On every incoming connection, call Tailscale LocalAPI `whois` for the peer address. Accept only peers on an allowlist stored by the host (node ID or login). Unknown peers get an in-UI approval prompt on the host and are rejected until approved.
- No passwords, no accounts, no public certificate infrastructure.
- Treat all inbound bytes as hostile: bounded lengths, bounded counts, no panics on malformed input, no unbounded allocation.

### 4.2 Video datagram header (all integers little-endian)

| Field | Type | Notes |
|---|---|---|
| `version` | u8 | Protocol version |
| `kind` | u8 | 1 = video slice, 2 = cursor, 3 = reserved |
| `flags` | u8 | bit0 KEY, bit1 LAST_FRAGMENT, bit2 CONFIG (SPS/PPS present) |
| `reserved` | u8 | Zero |
| `epoch` | u16 | Increments on every stream reset. Receiver drops other epochs |
| `frame_id` | u32 | Monotonic per epoch |
| `frag_idx` | u16 | Fragment index within the frame |
| `frag_cnt` | u16 | Total fragments in the frame |
| `capture_ts_us` | u32 | Host capture time, microseconds, wrapping |

Header is 18 bytes. **Maximum datagram size is 1200 bytes including header** (Tailscale's default interface MTU is 1280; 1280 minus IPv6 40 minus UDP 8 leaves 1232, and 1200 is the safe working size). Payload per fragment is therefore at most 1182 bytes. This number is a named constant in `proto`.

No FEC in v0. Loss recovery is by keyframe request plus the reassembler dropping incomplete frames. NACK and FEC are explicitly deferred.

### 4.3 Control messages (names fixed; fields refined in M1)

- `Hello` / `HelloAck`: protocol version, device name, OS, capabilities (codecs, max resolution, features).
- `TopologyAnnounce`: `topology_rev`, list of displays. Sent on connect and on any change.
- Display fields: `display_id` (stable: derived from EDID serial plus connector, not an OS index), `name`, `x`, `y` (virtual desktop origin, physical pixels), `width_px`, `height_px`, `scale_milli` (1500 = 150%), `refresh_mhz`, `flags` (primary, active), optional read-only `hdr`.
- `SwitchMonitor { req_id, display_id }`
- `StreamReset { req_id, epoch, codec, width, height, fps, topology_rev, status }`
- `SetQuality { max_height, bitrate_hint }` (viewer preference; **host decides** within 480p30 to 1080p30)
- `RequestKeyframe`
- `PauseVideo` / `ResumeVideo` (sent when the UI is hidden or shown)
- `InputEvent` (mouse move absolute normalized, mouse button, wheel, key with scancode and modifiers, relative mouse motion)
- `ClipboardUpdate { seq, mime, bytes }` (text first)
- `StatsReport` (host CPU %, capture backend, encoder, current resolution and refresh)
- `Goodbye`

### 4.4 Display switch lifecycle

1. Viewer sends `SwitchMonitor(display_id)`.
2. Host controller validates the display and tells the capture engine to migrate.
3. Capture releases the old display and attaches to the new one without ending the capture thread.
4. Encoder reconfigures to the new resolution and fps. Create the encoder once at the maximum size the host will use and reconfigure down where the backend allows. Teardown and rebuild is the tested fallback.
5. Host forces an IDR, increments `epoch`, sends `StreamReset`, then sends the keyframe slices.
6. **Viewer holds the last frame of the old display, scaled, until the first valid keyframe of the new epoch has decoded. It then replaces the rendered frame atomically.** Rendering never blocks during a switch.
7. Viewer updates its input mapping to the new display's geometry.

Timing target: switch completes in under 150 ms on LAN. Blackout is hidden by the held frame.

### 4.5 Input coordinate mapping

The viewer maps pointer position to the **rendered video rectangle** (after letterboxing), never the whole window.

- Let `R` be the rendered video rect in device pixels. `u = clamp((px − R.x) / R.w, 0, 1)`, `v` likewise.
- Host physical pixel: `X = D.x + u · (D.width_px − 1)`, `Y = D.y + v · (D.height_px − 1)`, where `D` is the selected display from `TopologyAnnounce`.
- Windows injection with the virtual-desktop absolute mode: `abs_x = round((X − V.left) · 65535 / (V.width − 1))`, where `V` is the virtual desktop bounds. `V.left` can be negative. The helper process must be per-monitor DPI aware.
- macOS injection uses points: `x_pt = D.x_pt + u · D.width_pt`. Keep the logical size from the host; do not derive it from rounded scale factors.
- Relative mouse motion is sent raw and unscaled.

These formulas live in `crates/topology` with a table-driven test suite (negative origins, mixed DPI, letterboxed and pillarboxed video, 480p stream on a 1080p display, edge pixels).

---

## 5. Application behavior and UI

### 5.1 Layout (four regions plus overlay)

```
App
 ├ DeviceRail            64–80 px wide
 ├ DeviceSidebar         channel-style list for the selected device
 │  ├ DisplayList
 │  ├ ControlList
 │  └ SystemList
 ├ SessionWorkspace
 │  ├ SessionHeader
 │  ├ NativeVideoSurface   wgpu, letterboxed, aspect ratio preserved
 │  └ SessionOverlay
 ├ TelemetrySidebar       collapsible
 └ LocalSessionPanel      bottom of the left column
```

**Device rail:** Home, one icon per known computer with an online/offline state, and Add/discover device. The selected device has a Discord-style active indicator on the left edge.

**Device sidebar** (example for a device named `WIN10-GAMING-PC`):

- STREAM: Display 1, Display 2 (populated from `TopologyAnnounce`; each shows name, resolution, refresh, scale, available state)
- CONTROL: Remote Desktop, Clipboard
- SYSTEM: Performance, Connection, Settings

Selecting a display issues `SwitchMonitor`. "Remote Desktop" is the focus-and-control mode of the currently selected display (keyboard and mouse capture on). Display items only choose what you watch.

**Session header:** `Device / Display 1` with a subtitle showing source and stream, for example `144 Hz display • streaming 720p30 • H.264 • 8 ms`. Controls: monitor selector, quality selector (480p, 720p, 1080p, Auto), fullscreen, keyboard capture, disconnect.

**Telemetry sidebar:**

- SESSION: connected state, Direct or DERP, RTT, packet loss, bitrate, FPS, codec, decoder
- HOST: CPU usage, capture backend, encoder, resolution, refresh rate
- EVENTS: display switch, decoder reset, connection established, quality adjustment, packet loss event

The sidebar updates at about 4 Hz without redrawing the whole UI and without interrupting the renderer.

**Local session panel:** local machine name, connection state, buttons for keyboard capture, mouse capture, audio (disabled placeholder), settings.

### 5.2 Visual style

Original dark UI. Do not copy Discord's logo, icons, illustrations or name.

- Device rail: very dark charcoal. Secondary sidebar: slightly lighter. Main panel: dark gray. Cards and selected rows: slightly lighter gray.
- Primary text near-white, secondary text muted gray, accent blue-purple.
- 1 px subtle borders, 6–10 px corner radii, compact spacing, clear selected and hover states, minimal smooth animation, custom scrollbars.
- Body text 14–16 px, section and header text 18–22 px.
- Define all colors, radii, spacing and font sizes as design tokens in one module. No magic numbers scattered through widgets.

### 5.3 Background behavior

- Closing the window hides to tray; it does not stop hosting.
- When the window is hidden or minimized, the viewer sends `PauseVideo` and stops decoding. On show, it sends `ResumeVideo` and the host sends a keyframe.
- Idle bandwidth with no active viewer should be only control keepalives.

### 5.4 Device discovery

- Source of truth for peers: Tailscale LocalAPI status (names, online state, addresses).
- A peer is "host-capable" if it answers the project handshake on the agent port.
- "Add/discover device" rescans the tailnet and lists peers running the agent. No separate discovery server.
- Path (direct or DERP) comes from Tailscale status and is shown in telemetry.

---

## 6. Separation of UI and streaming (hard rules)

- The UI communicates with the Rust core through **messages and events only**. **Video frames never travel through this bus.** Only metadata and control.
- Core events (minimum set): `DeviceDiscovered`, `DeviceOffline`, `TopologyChanged`, `DisplaySelected`, `StreamStarted`, `StreamReset`, `DecoderReady`, `ConnectionStatsUpdated`, `InputCaptureChanged`, `SessionEnded`.
- The decode thread hands frames directly to the render surface. The UI never touches pixel data.
- Packet processing runs on its own threads or async tasks, never tied to the UI event loop.
- Never block rendering while switching monitors. Never recreate the whole UI on telemetry updates.
- The core crates must compile and run tests with **no UI crate present**.

---

## 7. Performance and resource rules

Priorities, in order: 1) low latency, 2) reliability, 3) Windows 10 and 2015 Intel Mac support, 4) clear device and monitor switching, 5) Discord-inspired usability, 6) low CPU and memory use.

- Target glass-to-glass latency on LAN: under 100 ms at 720p30, as low as is practical. Do not trade reliability for the last few milliseconds.
- Host agent idle memory: keep small (target under 20 MB private). Active 1080p30 target: under 60 MB private. These are targets to measure, not guarantees to claim.
- No per-frame heap allocation in the capture, encode, packetize and decode paths where avoidable. Use pools and ring buffers with a fixed depth (2–3 frames).
- Latest-frame-wins: if the decoder or renderer falls behind, drop old frames rather than queueing. Never build a growing queue.
- Never run encode or decode in the UI process path that blocks the event loop.
- Pacing: spread each frame's packets across at most about 60% of the frame interval. Do not send a keyframe as a line-rate burst.
- Do not run video through a JavaScript canvas. Do not do CPU BGRA copies for UI presentation.

### Quality adaptation (v0)

- Host owns the decision. Range 480p30 to 1080p30, 30 fps fixed.
- Default bitrate hints: 480p about 1.5 Mbps, 720p about 3–4 Mbps, 1080p about 6–8 Mbps. Make these configurable constants.
- Step down one tier on sustained loss or RTT inflation; step up slowly after a stable period. Emit a `quality adjustment` event for each change.

---

## 8. Failure handling

Each of these needs an explicit state machine in `crates/session` with unit tests:

| Event | Required behavior |
|---|---|
| Capture lost (resolution change, sleep, lock, UAC, access lost) | Pause encode, notify viewer (paused state), re-create capture with backoff, new epoch plus keyframe on resume |
| Network path change (direct to DERP, MTU change) | Lower bitrate one tier, force keyframe, restore gradually |
| Packet loss producing an incomplete frame | Drop the frame, request a keyframe (rate-limited to once per 200 ms) |
| Decoder error or stall | Reset decoder, request keyframe, emit `decoder reset` event, keep showing last good frame |
| Encoder failure | Rebuild encoder, fall back to software OpenH264 if the hardware path keeps failing, emit event |
| Control connection drop | Viewer auto-reconnects with backoff and shows an overlay; the host stops streaming after a timeout |
| Missing OS permission (macOS) | Clear in-UI message with a button to the right Settings pane |
| Peer not on allowlist | Reject, prompt on host, never stream |

Never `unwrap()` or `expect()` on anything derived from the network, the OS, or a driver. Return typed errors.

---

## 9. Coding standards

- Stable Rust, edition 2021 or later, pinned via `rust-toolchain.toml`.
- `cargo fmt --all` clean and `cargo clippy --workspace --all-targets -- -D warnings` clean before any milestone is declared done.
- `unsafe` is allowed only in thin platform-binding modules, each block with a `// SAFETY:` comment stating the invariants. No `unsafe` in `proto`, `net`, `topology`, `session`, `telemetry`.
- Platform code is gated with `cfg(target_os = ...)` and sits behind traits (`Capture`, `Encoder`, `Decoder`, `InputInjector`, `Clipboard`) so logic can be tested with fake backends.
- Use `tracing` for logs. Log levels: errors for failures, info for lifecycle, debug for per-session detail, trace for per-packet. Never log per-packet at info.
- Prefer small, well-maintained crates. Before adding a dependency, check its license (no GPL/AGPL in the shipped binaries) and record it in `docs/decisions/` if it is significant (UI toolkit, async runtime, platform bindings).
- Public items get doc comments. Every non-trivial module gets tests.
- Keep wire compatibility deliberate: any change to header or message layout bumps the protocol version and updates `docs/PROTOCOL.md` in the same commit.

---

## 10. Testing and verification

### 10.1 What you can verify yourself

- Pure logic: `proto`, `net` (with loopback), `topology`, `session`, `telemetry`, `core` with fakes.
- Property and table tests: serialization round-trips, reassembly under loss, reorder, duplication and jitter (use the `testkit` injector), coordinate math, state machine transitions.
- Compile checks of platform code where a cross target is available, for example `cargo check --target x86_64-pc-windows-msvc` and `cargo check --target x86_64-apple-darwin`. If the target or SDK is unavailable in your environment, say so in `docs/PROGRESS.md` and mark the code **unverified**. Do not claim platform code works because it compiles.

### 10.2 What only the human can verify (never claim these as done)

Record each as a checklist item in `docs/HARDWARE.md` for the human to run, then wait for results:

- Windows service survives lock screen, UAC prompt, logoff and logon, fast user switching.
- Capture on a real GPU with NVENC, QSV and AMF paths (whichever hardware exists).
- Multi-monitor hot-switch on Windows PCs with differing resolutions and DPI.
- 2015 Intel Mac: macOS version, ScreenCaptureKit availability, VideoToolbox sustained 720p30 and 1080p30, temperature, dropped frames, permission prompts.
- Real Tailscale behavior: direct vs DERP path, `whois` allowlisting.
- Real latency measurement (photodiode, high-speed camera, or on-screen timestamp overlay).
- Keyboard layouts beyond US and international input edge cases.

### 10.3 Standard commands (keep these working)

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Add any extra scripts (for example `scripts/check-all.sh`) and keep them listed here. Additional scripts: scripts/check-all.sh and scripts/check-all.ps1; both run the layering check.

---

## 11. Milestones

Work one milestone at a time. A milestone is done only when every acceptance item is met, the standard commands pass, and `docs/PROGRESS.md` is updated. Do not start the next milestone's work early.

### M0. Workspace scaffold
- Cargo workspace with all crates from section 3 as compiling stubs, `rust-toolchain.toml`, `docs/` files, CI-style check script.
- **Done when:** standard commands pass on an empty-but-structured workspace.

### M1. Protocol crate (`proto`)
- Header and control message types, constants (`MAX_DATAGRAM = 1200`), serialization and parsing with strict bounds, `docs/PROTOCOL.md`.
- **Done when:** round-trip and malformed-input tests pass, including truncated, oversized and random byte inputs without panics.

### M2. Transport (`net`)
- UDP slicer, reassembler (drop incomplete frames, latest-wins), sender pacing, TCP control framing with `TCP_NODELAY`, bind-to-interface logic (testable via a configurable address).
- **Done when:** loopback tests pass under injected loss (0%, 2%, 5%), reorder, duplication and jitter; incomplete frames are dropped and keyframe requests are emitted (rate-limited); no unbounded memory growth in a soak test.

### M3. Topology, switching, input math (`topology`, `session`, `telemetry`)
- Display model, topology diffing, `SwitchMonitor` state machine with epochs (using fake capture and encoder), coordinate math from section 4.5, telemetry counters and event log types.
- **Done when:** state machine tests cover normal switch, switch during loss, switch to a removed display, rapid repeated switches (stale epoch packets ignored), and coordinate tests cover negative origins, mixed DPI, letterboxing and edge pixels.

### M4a. UI toolkit spike
- Evaluate iced and Slint with a synthetic NV12 stream in a wgpu surface, plus a mock Discord-style layout.
- **Done when:** `docs/decisions/0001-ui-toolkit.md` records the choice with frame-rate and stutter evidence. Ask the human to confirm on real hardware if you cannot measure it.

### M4b. UI shell with fake data (`app`)
- Full layout from section 5 driven by `testkit` fake devices and fake topology; design tokens module; hover, selected and collapse states; tray and hide-to-tray behavior stubbed.
- **Done when:** the UI runs with fake data, telemetry updates at 4 Hz without redrawing the whole UI, and the core compiles and tests without the UI crate.

### M5. Windows capture and encode to file
- Desktop Duplication capture, Media Foundation hardware H.264 encode with D3D11 input, OpenH264 software fallback, output to an `.h264` file for inspection.
- **Done when (agent):** `cargo check` for the Windows target passes and unit tests with fake backends pass. **Done when (human):** a recorded file plays correctly in a standard player and capture recovers from a mode change. Mark human items in `docs/HARDWARE.md`.

### M6. Windows host agent (service plus helper)
- Service, helper-in-console-session launch, supervision and restart, local IPC, allowlist and `whois` check, topology announcement from real displays.
- **Human tests required:** lock screen, UAC, logoff and logon, fast user switching.

### M7. End-to-end viewer
- Windows decode, wgpu render with letterbox, input capture and host injection, display switching with held-frame replacement, pause on hide.
- **Done when:** viewing and controlling a Windows host from a Windows viewer over loopback, then LAN, then Tailscale, with human confirmation.

### M8. Clipboard, telemetry and events
- Text clipboard sync in both directions with loop prevention, live telemetry (RTT, loss, bitrate, FPS, codec, decoder, path, host CPU), event log.
- **Done when:** clipboard round-trip tests pass with fakes, loop prevention is tested, telemetry fields all populate.

### M9. macOS host and viewer
- ScreenCaptureKit capture, VideoToolbox encode, CGDisplayStream fallback, permission handling UI, macOS decode and render, input injection (Accessibility).
- **Human tests required** on the 2015 Intel Mac: version, performance, thermals, permissions. Default to 720p30 until verified.

### M10. Polish and packaging
- Tray, autostart, settings persistence, installers, signing and notarization notes, README for the human.

---

## 12. Suggested `/goal` prompts

Use one at a time. Each ends with a verifiable condition.

- **M0:** "Create the Cargo workspace described in AGENTS.md section 3 with compiling stub crates and docs files. Done when `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` all pass."
- **M1:** "Implement the `proto` crate per AGENTS.md section 4 and write `docs/PROTOCOL.md`. Done when all round-trip and malformed-input tests pass and the standard commands are clean."
- **M2:** "Implement the `net` crate per AGENTS.md M2. Done when loopback tests pass at 0%, 2% and 5% injected loss plus reorder, duplication and jitter, and the standard commands are clean."
- **M3:** "Implement `topology`, `session` and `telemetry` per AGENTS.md M3 using fake capture and encoder backends. Done when the listed state-machine and coordinate tests pass."
- **M4a:** "Run the UI toolkit spike in AGENTS.md M4a and write `docs/decisions/0001-ui-toolkit.md`. Done when the ADR exists with evidence, and list any measurements the human must confirm in `docs/HARDWARE.md`."
- **M4b:** "Build the UI shell with fake data per AGENTS.md section 5 and M4b. Done when the app runs with fake devices, the core compiles and tests without the UI crate, and the standard commands are clean."
- **M5 onward:** use the same pattern, but always finish by listing human-only checks in `docs/HARDWARE.md` and stopping until the human reports results.

---

## 13. Working agreement for agents

1. Read this file, docs/DEV_SETUP.md, and docs/PROGRESS.md at the start of every session. Cargo package names use the racc- prefix; see ADR 0008.
2. Work on one milestone only. Keep changes small and committed in logical steps.
3. Update `docs/PROGRESS.md` at the end of every session with what was done, what is verified, what is unverified, and what is blocked.
4. Never mark hardware-dependent work as complete. Put it on the human checklist.
5. Never fabricate measurements. If you did not measure it, say so.
6. If a requirement here is impossible, unclear or in conflict, stop that item, write the question in `docs/OPEN_QUESTIONS.md`, and continue with unblocked work.
7. Do not expand scope. Items on the exclusion list (audio, HDR, HEVC/AV1 requirements, FEC and NACK in v0, clipboard images and files, GPU usage telemetry) stay out unless the human adds them.
8. Do not add telemetry, analytics or network calls beyond the Tailscale network and the Tailscale LocalAPI.
9. Discord may be named in documentation only to describe design inspiration. The name, logo, and assets must never appear in code, identifiers, UI strings, window titles, branding, package or file names, or shipped assets. The project's own logo, when made at milestone M10, is a raccoon emoji using a blue-purple, dark-theme colorway. It must use artwork under an open license (for example Google Noto Emoji; verify its current license) and must not use Apple's emoji artwork or any Discord logo shapes. Track the asset and license check as an M10 open question. Do not create a logo before then.
10. Do not weaken security rules in section 4.1 to make something easier to test. Use a test-only configuration instead.

---

## 14. Out of scope (do not build)

Audio streaming, HDR, HEVC and AV1 as requirements, resolutions above 1080p, frame rates above 30, forward error correction and NACK retransmission (v0), clipboard images and files, file transfer, multi-user accounts, passwords, public relays, STUN/TURN/ICE, browser or web viewer, mobile clients, Linux hosts or viewers.
