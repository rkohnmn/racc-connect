# Project Scope: Native Low-Latency Multi-Computer Remote Desktop (working title)

**Document purpose:** the complete statement of what this project is, what it includes, what it explicitly excludes, and how success is judged. It is the product-level companion to `AGENTS.md` (which instructs coding agents) and `docs/PROTOCOL.md` (which will hold the wire spec).

**Status:** planning complete, implementation not started.
**Owner:** the project author (sole developer and sole user in the initial release).

---

## 1. Vision

Open one lightweight application, see all of your own computers in a column on the left, pick one, pick one of its monitors the way you would pick a chat channel, and immediately watch and control that monitor in the main panel, with live connection statistics beside it. The application stays in the background until needed, uses little CPU and memory, and relies on Tailscale for connectivity and security so that none of the usual remote-desktop infrastructure (accounts, relays, certificates, port forwarding) is needed.

The product is a **personal, private-network tool**, not a general-purpose commercial remote-support service.

---

## 2. Goals and non-goals

### 2.1 Goals (in priority order)

1. **Low latency.** Interactive remote control that feels responsive on a local network and acceptable across a Tailscale tunnel.
2. **Reliability.** Survives display changes, sleep and wake, lock screens, network path changes and process crashes without manual intervention.
3. **Support for Windows 10 and a 2015 Intel Mac.** H.264 is always available; newer codecs are never required.
4. **Clear device and monitor switching.** Switching computers or monitors is obvious, fast, and does not drop the connection.
5. **Discord-inspired usability.** Familiar structure and polish without copying branding or assets.
6. **Low CPU and memory use.** Mostly achieved by hardware encode and decode and by not streaming when nobody is looking.

### 2.2 Non-goals

- Competing with commercial remote-support products.
- Supporting people or machines outside the owner's own tailnet.
- Maximum image quality or maximum resolution. The product targets comfortable everyday use, not media or gaming fidelity.

---

## 3. Users and environment

### 3.1 Users

- **Primary and only initial user:** the project author, controlling their own three computers.
- **Future (not committed):** other individuals running the same app on their own tailnets.

### 3.2 Final target environment

| Machine | Role | Notes |
|---|---|---|
| Windows 10 PC #1 | Host and viewer | Hardware encode (NVENC, QSV or AMF depending on GPU) |
| Windows 10 PC #2 | Host and viewer | Same as above |
| 2015 Intel Mac | Host and viewer | Most likely limited to macOS 12 (Monterey); weak Intel GPU; treat as the compatibility floor |

**Every machine can appear as a host in the device list, and every machine can act as a viewer.**

### 3.3 Network assumptions

- All devices are on one Tailscale network (tailnet), already authenticated by Tailscale.
- Connections may be direct (LAN or hole-punched) or relayed through Tailscale's DERP servers. The app must work in both cases and show which is in use.
- The Tailscale interface MTU is 1280 by default; the protocol is sized for that.

---

## 4. Functional scope

### 4.1 Device management

- Show all known computers in a left-hand rail with an online or offline indicator.
- Discover host-capable computers by querying Tailscale peer status and probing for the app's agent.
- Maintain a per-host allowlist of approved peers. Unknown peers prompt for approval on the host and are rejected until approved.
- "Home" view for global status and settings; "Add/discover device" to rescan the tailnet.

### 4.2 Monitor (display) management

- Each host announces its display topology: stable display ID, name, position, physical resolution, refresh rate, scale factor, primary and active flags (HDR may appear as a read-only label only).
- The viewer lists displays as "channels" under the selected computer, populated dynamically from the host.
- Selecting a display switches the stream to it without ending the session. On switch, the viewer keeps showing the last frame of the old display until the first valid keyframe of the new stream has decoded, then replaces the picture atomically.
- Topology changes (monitor plugged, unplugged, resolution change) are detected on the host and pushed to viewers.

### 4.3 Video streaming

- **Codec:** H.264 only is required. Encoder produces no B-frames and uses low-latency settings.
- **Quality range:** host-chosen, **480p30 minimum to 1080p30 maximum**. Frame rate is fixed at 30 fps. The Mac host defaults to 720p30 until it passes a sustained test at higher settings.
- **Viewer quality selector:** 480p, 720p, 1080p and Auto. These are preferences; the host makes the final decision within its capabilities.
- **Adaptation:** the host steps resolution and bitrate down on sustained packet loss or latency growth, and back up slowly when stable, emitting an event for each change.
- **Aspect ratio:** the viewer preserves aspect ratio and letterboxes or pillarboxes as needed.
- **Cursor:** cursor shape, hotspot and position are sent as metadata and drawn by the viewer, not baked into the video.
- **Pause on hide:** when the viewer window is hidden or minimized, it tells the host to pause video and stops decoding. Resuming triggers a fresh keyframe.

### 4.4 Remote control

- Mouse: absolute pointer movement mapped to the **rendered video rectangle** (not the whole window), buttons, wheel, and optional relative motion.
- Keyboard: key events by scancode with modifiers, with an optional "keyboard capture" mode that forwards system shortcuts where the OS allows it.
- Coordinate mapping handles negative monitor origins, mixed resolutions and DPI scales, and streams that are smaller than the host display (for example 480p on a 1080p screen).

### 4.5 Clipboard

- Required feature. **Text first**, both directions, with loop prevention and size limits.
- Images and files are out of scope until text is solid and tested.

### 4.6 Telemetry and events

Shown in a collapsible right-hand sidebar, updating about four times per second without redrawing the whole UI or interrupting video:

- **Session:** connected state, direct or DERP, RTT, packet loss, bitrate, FPS, codec, decoder.
- **Host:** CPU usage, capture backend, encoder, resolution, refresh rate.
- **Events:** display switch, decoder reset, connection established, quality adjustment, packet loss event.

Host GPU usage is dropped from the initial scope; it may return later as an optional feature.

### 4.7 Background and tray behavior

- Closing the window hides it to the system tray; hosting continues.
- Host agent runs independently of the UI and starts automatically (autostart is part of packaging).
- With no active viewer, only lightweight control keepalives are exchanged.

---

## 5. User interface scope

### 5.1 Layout

Four regions plus an overlay:

- **Device rail** (64 to 80 px): Home, one icon per computer with online state and a left-edge active indicator, Add/discover.
- **Device sidebar:** channel-style lists for the selected computer.
  - STREAM: one item per display (name, resolution, refresh, scale, availability).
  - CONTROL: Remote Desktop, Clipboard.
  - SYSTEM: Performance, Connection, Settings.
- **Session workspace:** header (device and display name, source and stream summary such as "144 Hz display, streaming 720p30, H.264, 8 ms"; monitor selector, quality selector, fullscreen, keyboard capture, disconnect), the native video surface, and an overlay for states such as connecting, paused, switching, error.
- **Telemetry sidebar:** collapsible, as in section 4.6.
- **Local session panel:** local machine name, connection state, buttons for keyboard capture, mouse capture, audio (disabled placeholder), settings.

### 5.2 Visual style

Original dark theme inspired by Discord's structure: very dark charcoal rail, slightly lighter secondary sidebar, dark gray main area, lighter gray cards and selected rows, near-white primary text, muted gray secondary text, blue-purple accent. One-pixel borders, 6 to 10 px radii, compact spacing, clear hover and selected states, minimal smooth animation, custom scrollbars, 14 to 16 px body text and 18 to 22 px headers. All values live in a design-token module.

**Branding constraint:** no Discord name, logo, icons, illustrations or other proprietary assets anywhere.

### 5.3 Rendering requirement

Video is decoded with hardware decoders and rendered on a native wgpu surface. It is never rendered as repeatedly updated images, never through a JavaScript canvas, and never through a webview. The UI toolkit must therefore share a wgpu context with the video (iced or Slint; chosen by a spike milestone).

---

## 6. Technical scope

### 6.1 Languages and runtime

Rust for networking, capture, encoding, decoding, input, core logic and UI. Platform bindings and unsafe code are confined to thin, documented modules.

### 6.2 Process model

- **host-agent:** always-on process owning capture, encode, host-side input injection and host-side networking. On Windows it is a service that launches a capture helper into the active console session (a service in session 0 cannot capture the interactive desktop).
- **app:** the UI, owning decode, render, viewer-side input capture, device list and telemetry display.
- The UI talks to the local agent over local IPC and to remote agents over the project protocol.

### 6.3 Capture

| Platform | Primary | Fallback |
|---|---|---|
| Windows | DXGI Desktop Duplication | Windows.Graphics.Capture (cannot see the secure desktop) |
| macOS | ScreenCaptureKit (macOS 12.3+) | CGDisplayStream |

Capture is damage-driven where the OS allows, so idle screens cost almost nothing.

### 6.4 Encode and decode

- Windows encode: Media Foundation hardware H.264 (reaching NVENC, QSV and AMF through vendor MFTs) with D3D11 input; OpenH264 software fallback. x264 is excluded because of GPL licensing.
- macOS encode: VideoToolbox in real-time mode, no frame reordering.
- Decode: Media Foundation/DXVA on Windows, VideoToolbox on macOS.
- GPU color conversion only. A GPU copy or NV12 upload is acceptable; true zero-copy interop is a later optimization.

### 6.4a Why not the original, more ambitious plan

Earlier research targeted 4K60 with sub-30 ms glass-to-glass latency, HEVC and AV1, forward error correction, a browser viewer and Linux support. These were reduced because the actual need is 480p30 to 1080p30 across three known machines. The reduced scope removes most of the hard engineering without affecting the intended use.

### 6.5 Networking and protocol

- Two channels per session: **TCP control** (handshake, topology, switching, input, clipboard, stats, keyframe requests) and **UDP video** (H.264 Annex B slices and cursor metadata).
- Maximum video datagram size is 1200 bytes including an 18-byte header (sized for Tailscale's 1280 MTU).
- Each stream has an epoch number that increments on reset; receivers discard packets from other epochs.
- Loss handling in v0: drop incomplete frames and request a keyframe (rate-limited). No FEC or NACK in v0.
- Sender pacing spreads each frame's packets over part of the frame interval; keyframes are never sent as line-rate bursts.

### 6.6 Security

- The agent binds only to the Tailscale interface address.
- WireGuard provides authentication and encryption; no application-layer encryption.
- Peer identity is checked with Tailscale's LocalAPI `whois` against a host-side allowlist, with an approval prompt for new peers.
- All inbound data is treated as hostile: bounded sizes, no panics on malformed input.
- No accounts, passwords, public certificates, analytics or external network calls beyond the tailnet and Tailscale LocalAPI.

### 6.7 Resource targets (to be measured, not promised)

- LAN glass-to-glass latency: under 100 ms at 720p30, lower where practical.
- Host agent: idle under about 20 MB private memory; active 1080p30 under about 60 MB private memory.
- Host CPU: low single digits when using hardware encode on the Windows PCs. The 2015 Mac is the exception to watch.
- Per-session bandwidth: roughly 1.5 Mbps at 480p, 3 to 4 Mbps at 720p, 6 to 8 Mbps at 1080p (configurable defaults).

---

## 7. Explicitly out of scope

- Audio capture, transport or playback (the audio button is a disabled placeholder).
- HDR handling.
- HEVC or AV1 as requirements (optionally later, never mandatory).
- Resolutions above 1080p and frame rates above 30.
- Forward error correction and NACK retransmission in the first version.
- Clipboard images and files; general file transfer.
- Browser or web-based viewer; mobile clients; Linux hosts or viewers.
- Multi-user accounts, passwords, public relays, STUN/TURN/ICE, public certificates.
- Host GPU usage telemetry (may be added later behind a feature flag).
- Recording, session sharing, or multi-viewer broadcast.

---

## 8. Deliverables

1. Rust workspace with the crate structure defined in `AGENTS.md`.
2. `host-agent` for Windows 10 and macOS (Windows as a service plus helper).
3. `app` (the Discord-inspired UI) for Windows 10 and macOS.
4. Documentation: `PROTOCOL.md`, `HARDWARE.md` test results, `PACKAGING.md`, architecture decision records.
5. Installers or packages with autostart, plus signing and notarization notes for macOS.

---

## 9. Milestones

| ID | Milestone | Verifiable by |
|---|---|---|
| M0 | Workspace scaffold | Agent (build and lint pass) |
| M1 | Protocol crate | Agent (round-trip and malformed-input tests) |
| M2 | Transport (UDP slicing, reassembly, pacing, TCP control) | Agent (loopback loss, reorder, jitter tests) |
| M3 | Topology, switch state machine, input math, telemetry types | Agent (state and coordinate tests) |
| M4a | UI toolkit spike (iced vs Slint) | Agent plus human confirmation of smoothness |
| M4b | UI shell with fake data | Agent |
| M5 | Windows capture and encode to file | Agent for logic; **human** for real GPU output |
| M6 | Windows host agent (service plus helper) | **Human** (lock screen, UAC, logoff, user switching) |
| M7 | End-to-end Windows viewer and control | **Human** (loopback, LAN, Tailscale) |
| M8 | Clipboard, live telemetry, events | Agent for logic; human for end-to-end |
| M9 | macOS host and viewer | **Human** on the 2015 Mac |
| M10 | Polish, tray, autostart, packaging | Human |

The recommended order is the Windows host and a bare viewer first, then the device rail and display list, then input and clipboard, then telemetry, then the Mac host last.

---

## 10. Success criteria

The project is successful when, on the owner's three machines:

1. The app lists all three computers with correct online and offline state.
2. Any machine can be selected as a host and any display chosen like a channel, and the chosen display appears live in the main panel.
3. Switching displays completes without dropping the session, holds the previous frame during the switch, and finishes in under about 150 ms on LAN.
4. Mouse and keyboard control land on the correct pixel on monitors of differing resolution, DPI and position, including a stream smaller than the host display.
5. Text clipboard transfers correctly in both directions without loops.
6. Hosting continues with the UI closed, survives lock screen, UAC and user switching on Windows, and recovers from display sleep, resolution change and network path changes.
7. The 2015 Intel Mac hosts and views stably at a documented sustained quality (at least 720p30) without unacceptable heat or dropped frames.
8. Measured latency, CPU and memory are recorded in `docs/HARDWARE.md` and meet or honestly report against the targets in section 6.7.
9. Windows PCs use hardware encode and decode; software fallback works when hardware fails.
10. No Discord branding or assets appear anywhere.

---

## 11. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Windows service cannot reach the interactive desktop | Hosting fails after lock or logon | Service plus console-session helper design; early human test in M6 |
| 2015 Mac struggles with 1080p30 encode, thermals, or is stuck on an old macOS | Poor Mac hosting experience | Default 720p30, allow 480p, fallback capture path, treat Mac as last milestone, honest documentation of limits |
| UI toolkit cannot present a native video surface smoothly inside a custom layout | UI and video conflict | Dedicated spike (M4a) before final UI work; toolkit must share the wgpu context |
| Media Foundation latency or behavior varies by GPU vendor | Higher latency or encoder quirks | Low-latency codec properties, fake-backend testing, OpenH264 fallback, direct vendor SDKs as a later optimization behind the same trait |
| Input injection edge cases (secure desktop, layouts, DPI) | Wrong clicks or lost keys | Table-driven coordinate tests, per-monitor DPI awareness, human tests across layouts |
| macOS permission friction (Screen Recording, Accessibility) | Black video or dead input | Detect and explain in the UI with a direct link to the right settings pane |
| Packet loss without FEC causes visible artifacts | Choppy video on poor paths | Drop incomplete frames, rate-limited keyframe requests, bitrate step-down, FEC deferred but designed for |
| Control over TCP stalls mouse movement under loss | Laggy input | Measure first; move mouse motion to UDP latest-wins if needed (recorded as an ADR) |
| Autonomous agents claim success on untestable hardware work | False confidence | `AGENTS.md` rules: hardware items go on a human checklist and are never marked done by the agent |
| Licensing mistakes (GPL code, H.264 patents) | Legal exposure if distributed | Avoid x264 and GPL dependencies, use OS-supplied hardware codecs and OpenH264, check licenses before distribution |
| macOS distribution needs signing and notarization | Cannot easily run on other Macs | Documented in `PACKAGING.md`; handled manually by the owner |

---

## 12. Assumptions and constraints

- The owner controls all three machines and the tailnet.
- Tailscale is installed, logged in and running on each machine, and its LocalAPI is reachable.
- Windows 10 machines have a GPU with a hardware H.264 encoder; otherwise software encode is used with higher CPU cost.
- The Mac can run at least macOS 12.3 for ScreenCaptureKit; otherwise the CGDisplayStream fallback is used.
- Initial release is for personal use; commercial distribution would require a separate licensing and support review.
- Performance numbers in this document are targets and engineering estimates until measured.

---

## 13. Future possibilities (not committed)

- Optional audio with its own bandwidth budget and toggle.
- NACK and forward error correction for lossy paths.
- Direct NVENC, AMF and QSV integrations for lower latency.
- True zero-copy decode-to-render paths.
- Clipboard images and files, then general file transfer.
- Optional HEVC or AV1 when both ends support hardware.
- Optional host GPU telemetry behind a feature flag.
- A browser viewer using WebCodecs and WebTransport.
- Linux host and viewer support.
- Higher resolutions and frame rates on capable hardware.

---

## 14. Glossary

- **Tailnet:** the private Tailscale network connecting the owner's devices.
- **DERP:** Tailscale's relay servers, used when a direct connection cannot be established.
- **Epoch:** a counter incremented on every stream reset so stale packets can be discarded.
- **IDR/keyframe:** a self-contained video frame that lets a decoder start or recover.
- **Host / viewer:** the computer being controlled and the computer doing the controlling; every machine can be both.
- **Host agent:** the always-on background process that captures, encodes and serves a computer's screens.
- **Damage-driven capture:** capturing and encoding only when the screen changes.
- **Letterboxing:** preserving aspect ratio by adding bars where the video does not fill the surface.
