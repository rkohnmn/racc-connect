# Hardware Verification Record

Hardware behavior checks require the project author's computers or tailnet. They remain unchecked until a human records the results.

## Environment record

### Owner-reported hardware

Specifications below were supplied by the owner on 2026-10-06 and have not been independently verified.

- Windows PC #1 (laptop) [OWNER-REPORTED]: AMD Ryzen 7 5800U; NVIDIA GeForce RTX 3050 Ti; 16 GB RAM at 4266 MHz.
- Windows PC #2 (server PC) [OWNER-REPORTED]: AMD Ryzen 9 5900X; AMD Radeon RX 7900 GRE; 32 GB DDR4.
- Late 2015 MacBook [OWNER-REPORTED]: macOS Monterey 12.7.6; Intel Core i7 2.2 GHz quad-core; Intel Iris Pro 1536 MB; 16 GB 1600 MHz DDR3. [UNVERIFIED INFERENCE] The stated CPU and GPU correspond to a 15-inch MacBook Pro in the 2013 to 2015 range; macOS 12.7.6 (Monterey) implies a 2015-or-later model, so the likely model is a 15-inch MacBook Pro (Mid 2015). The owner must confirm via Apple menu, About This Mac.

### Expected implications (unverified)

- PC #1 has an NVIDIA GPU, so the Windows Media Foundation H.264 path is expected to expose NVENC; this has not been tested.
- PC #2 has an AMD GPU, so the Windows Media Foundation H.264 path is expected to expose AMF; this has not been tested.
- macOS 12.7.6 is at or above the documented macOS 12.3 ScreenCaptureKit minimum, so ScreenCaptureKit is expected to be available; this is unverified on the Mac.
- The Mac's Intel GPU is expected to provide hardware H.264 encoding through VideoToolbox; this is unverified.

### This Codex machine — read-only probe, 2026-10-06

- [VERIFIED-RUN] Windows 10.0.19045 (build 19045).
- [VERIFIED-RUN] NVIDIA GeForce RTX 3050 Ti Laptop GPU, driver 32.0.15.9571; AMD Radeon(TM) Graphics, driver 31.0.21923.11000. Virtual display adapters were also reported.
- [VERIFIED-RUN] This machine matches Windows PC #1 (laptop) by the NVIDIA GeForce RTX 3050 Ti GPU.
- [VERIFIED-RUN] Rust: rustc 1.95.0 (59807616e 2026-04-14); Cargo 1.95.0 (f2d3ce0bd 2026-03-21).
- [VERIFIED-RUN] On 2026-10-06 Tailscale was not on PATH; the standard Program Files location had not yet been checked. The 2026-10-07 M5c probe found the installed CLI and verified live behavior below.
- [VERIFIED-RUN] Windows display API reported 3 active displays, each at 1920×1080.

## 2026-10-07 — M5a redacted display metadata probe (Windows PC #1)

- [VERIFIED-RUN] Ran `cargo run --offline -p racc-capture --example capture_probe -- --list`. This enumerated display metadata only; it did not start capture, acquire desktop frames, or read pixels. The output intentionally omitted display names, connector paths, and adapter LUIDs.
- [VERIFIED-RUN] All three active outputs are driven by the integrated AMD Radeon(TM) Graphics adapter. Each reports 1920×1080 and `scale_milli=1000`; two report 60,000 mHz and one reports 240,000 mHz. The redacted output ordering was:

| Redacted output | Desktop origin | Size | Active refresh | Scale | Adapter | Identity source |
|---|---:|---:|---:|---:|---|---|
| A | (0, 0) | 1920×1080 | 240,000 mHz | 1000 | AMD Radeon(TM) Graphics | Connector path |
| B | (-1920, 4) | 1920×1080 | 60,000 mHz | 1000 | AMD Radeon(TM) Graphics | Connector path |
| C | (-984, -1080) | 1920×1080 | 60,000 mHz | 1000 | AMD Radeon(TM) Graphics | Connector path |

- [HUMAN-PENDING] Capture performance, copy/scaling completion, cross-adapter encoding, cursor shape pixels/hotspots, and migration timing were not measured. Do not run capture modes until the visible screen is clear for capture; never capture the lock or UAC desktop.

## M5 — Windows capture and encode

- [VERIFIED-RUN — SYNTHETIC ONLY] On PC #1, Media Foundation encoded 90 synthetic 1280×720 frames as H.264 Main; ffprobe found 90 frames and no B-frames, and ffmpeg decoded the full stream. The elementary stream lacks timestamps, so this is not a measured 30 fps cadence or a real desktop-capture test. See `docs/ENCODE.md`.

- [VERIFIED-RUN] Metadata-only `capture_probe --list` on PC #1 confirmed the adapter/mode table above; this does not count as a frame-capture test.
- [ ] [HUMAN-PENDING] After clearing visible-screen privacy, run `cargo run -p racc-capture --example capture_probe -- --sample DISPLAY_ID --seconds 10 --acknowledge-visible-screen` on each output for static and moving-content runs; record frame counts, p50/p95, and drops. Add `--migrate-to TARGET_DISPLAY_ID` for migration timing.
- [ ] [HUMAN-PENDING] Run the same capture sample on PC #2 and verify recovery after a display mode change. Do not capture a lock/UAC screen; secure-desktop checks belong to the owner-run service checklist.
- [ ] [HUMAN-PENDING] Record the Windows PC GPU and driver; verify capture and H.264 encode using the available NVENC, QSV, and/or AMF paths.
- [ ] [HUMAN-PENDING] Play the recorded H.264 output and verify recovery after a display mode change.

## M5c — Tailscale identity and discovery (Windows PC #1)

- [VERIFIED-RUN] Read-only Tailscale CLI probe on 2026-10-07: version 1.102.4; 2/2 peers online; aggregate path counts 0 direct and 2 DERP; self-bind address passed the Tailscale bind policy.
- [VERIFIED-RUN] Twenty status calls measured approximately 210 ms median / 339 ms p95. Whois succeeded 20/20 times at approximately 217 ms median / 439 ms p95.
- No names, addresses, node identifiers, login names, raw JSON, or DERP region names are recorded.
- [HUMAN-PENDING] Repeat aggregate live checks on PC #2 and the Monterey Mac. Verify allowlist pending/approve/reject/reconnect end-to-end after M6 exists.

## M6 — Windows host agent

The SCM dispatcher, active-session helper launcher, foreground Windows host, and named-pipe IPC server/handler are present in source. The handler serves live status and persistence-backed allowlist operations; `SetHostingEnabled` returns `Failure(Unavailable)` because safe stop/rebind support is not wired. Host-agent tests passed TESTED-FAKE, and the Windows target passed COMPILE-ONLY. The service was not installed, registered, or started by the agent, and the named pipe was not launched or inspected. The host loop, SCM notifications, helper-token launch, real capture, allowlist/Tailscale path, and clipboard OS operations have not been run. The helper uses WinSta0\Default and reports secure-desktop capture unavailable; it does not switch onto the Winlogon desktop. The Windows Graphics Capture fallback is also absent.

- [ ] [HUMAN-PENDING] Run root setup-windows.ps1 elevated on both Windows PCs to build and install the current release, then verify service status and automatic startup. Test uninstall and confirm clean removal separately; the script has not yet been run on either PC.
- [ ] [HUMAN-PENDING — PROBE SOURCE INTEGRATED] On the other Windows PC, run `racc-probe` against the host over Tailscale and record aggregate frames, bytes, FPS, loss, RTT, and direct/DERP path without peer identifiers. `racc-probe` and a separate synthetic `host-loopback` harness now exist; the loopback harness does not prove real capture, encoding, or tailnet behavior.
- [ ] [HUMAN-PENDING — APP AND IPC SOURCE INTEGRATED] Start the foreground helper with `racc-host-agent console` (or the human-installed helper) and verify the app receives `GetStatus`, approved-list retrieval, pending snapshot/add/remove notifications, and approve/reject/remove actions. Confirm the pending subscription reconnects and applies a fresh snapshot. Confirm `SetHostingEnabled` returns `Failure(Unavailable)` and does not change reported state. The named pipe has not been launched or inspected.
- [ ] [HUMAN-PENDING — PIPE ACL RUNTIME CHECK] On each Windows PC, inspect the running pipe DACL and verify only SYSTEM, Administrators, and Interactive Users are granted access; verify a remote pipe client is rejected. The source SDDL is `D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)` and the server uses `PIPE_REJECT_REMOTE_CLIENTS`.
- [ ] [HUMAN-PENDING] Verify service/helper behavior through lock/unlock, UAC, logoff/logon, fast user switching, console/RDP session changes if used, and a manually killed helper. Do not expose private content. Confirm lock/UAC produces an explicit unavailable status and capture recovers after returning to the default desktop; do not claim secure-desktop capture.
- [ ] [HUMAN-PENDING] Record idle host private memory with hosting enabled and no viewer, then host CPU time/private memory at 720p30 and 1080p30 during a real stream. No measurements have been taken.
- [ ] [HUMAN-PENDING — BLOCKED ON END-TO-END STREAM] Validate one-second StatsReport fields against live host/viewer behavior and compare RTT to tailscale ping. Current code reports machine-wide and process CPU, DXGI/encoder, resolution/refresh, configured target bitrate, and successfully sent Racc UDP datagram bitrate; it excludes UDP/IP overhead.
## M7 — Viewer and input

M7 source status: [TESTED-FAKE] Host-agent tests cover authenticated connection/epoch/display validation, valid injection through a fake injector, held-input release, reset reauthorization, and nonblocking behavior under queue saturation. The live app has discovery-first peer selection, ViewerRuntime, Windows Media Foundation and macOS VideoToolbox decoder paths, and a separate latest-frame/wgpu NV12 path. Windows now negotiates D3D11-aware hardware MFTs, attaches a DXGI device manager, validates DXGI NV12 surfaces, and reads them through a bounded staging texture; CPU-output synchronous MFTs remain the fallback. This still copies GPU→CPU→GPU before wgpu and does not establish that a real MFT/GPU uses the path. Hosts poll display topology once per second and rebase input/cursor mapping when the selected display remains compatible. [COMPILE-ONLY] The full Windows workspace target passes. No remote session, real DXVA output, real OS input, or Mac runtime was tested. M7 remains incomplete.

Owner-provided Windows and Mac app screenshots on 2026-10-08 show both Tailscale peers online and a Direct path (displayed RTTs: 19 ms on Windows and 9 ms on Mac). The Mac local host agent is shown running; the Windows local host agent is offline with a local IPC error. Both remote sessions are disconnected, no display topology or frames are shown, and the Mac app reports `Hello (Busy)`. These screenshots verify discovery and route telemetry only; live video and input remain unverified. Source fixes now retry Busy handshakes, keep zero-port capability probes from claiming the viewer slot or starting capture, restrict reconnect grace to the original approved peer, and announce live topology changes. The local host-agent IPC status is separate from the remote viewer connection. Retest after pulling the changes on both machines.
- [ ] [HUMAN-PENDING — M7 RETEST] Pull/rebuild the current source on both machines. Keep the Mac host agent running and Tailscale connected. Test one direction at a time: first, on Windows select the Mac and start the session; wait through one five-second Busy retry if needed. Confirm the display list appears, the session becomes connected, and a live frame/FPS is visible. End that session before testing Mac viewer → Windows host. If Windows is only a viewer, its local host agent may remain offline; the Windows host agent must be running for the reverse direction. If it still fails, send screenshots from both sides showing event lists and connection state. Record the Windows local IPC status separately from the remote session.

- [ ] [HUMAN-PENDING] Verify monitor hot-switching across displays with different resolutions and DPI.
- [ ] [HUMAN-PENDING] Measure LAN latency with a photodiode, high-speed camera, or on-screen timestamp overlay and record the method.
- [ ] [HUMAN-PENDING] Verify keyboard layouts beyond US and international input edge cases.
- [ ] [HUMAN-PENDING] Verify real Tailscale direct and DERP path behavior.
- [ ] [HUMAN-PENDING] Run the prompt's 30-second Windows host-loopback plus real DDA/hardware-decoder viewer scenario: switch display, pause/resume, quality change, kill/reconnect, and record the actual frame path and resource costs. Do not capture private visible content without clearing the screen first.
- [ ] [HUMAN-PENDING] Compare virtual-desktop absolute pointer injection against `SetCursorPos` at every monitor corner and center; record the chosen default and mapping error.
- [ ] [HUMAN-PENDING] Verify two-PC sessions in both directions, mixed-DPI displays, input layouts, drag/wheel, release after disconnect, pause, switch/reset, and helper shutdown, minimized pause/resume, rapid switching, network unplug/reconnect, 30-minute soak, and camera-based glass-to-glass latency at all tiers.

## M8 — Clipboard and telemetry

- [ ] [HUMAN-PENDING] With both Windows PCs available, run non-private text round trips in both directions using non-ASCII text, repeated copies, exactly 512 KiB, and a value over 512 KiB. Confirm the larger value is rejected and clipboard text is absent from logs and telemetry.
- [ ] [HUMAN-PENDING] Verify CONTROL > Clipboard is off by default, reads/writes only after the active-session toggle is enabled, stops future transfers immediately on disable, and clears pending values when the viewer disconnects or reconnects. Verify the authenticated host receives the explicit enable/disable signal.
- [ ] [HUMAN-PENDING — MAC SOURCE INTEGRATED; APPLE HOST CHECK BLOCKED] On the 2015 Mac, verify text-only clipboard round trips in both directions with a Windows peer as viewer and as host. Check disabled-by-default behavior, enable/disable/disconnect/reconnect gating, Unicode, exactly 512 KiB and over-limit rejection, echo prevention, and that clipboard contents never appear in logs or telemetry. No Mac runtime has been exercised; see `docs/CLIPBOARD.md` and ADR 0041.
- [ ] [HUMAN-PENDING — END-TO-END STREAM/UI] Compare live RTT to tailscale ping, check path, loss, bitrate, FPS, codec/decoder and machine/process host CPU telemetry against their sources, and verify the event list. Record app-observed frame-source interval p95 with telemetry sidebar updates on and off; this is not physical present timing.
- [ ] [HUMAN-PENDING — PACING COMMAND] On Windows PC #1, run from the updated repository with the Mac host already streaming and substitute each machine’s Tailscale IPv4 address:
  - Sidebar expanded: `cargo run --release -p racc-app -- --connect <MAC_TAILSCALE_IP>:47473 --bind <WINDOWS_TAILSCALE_IP> --measure-secs=60 --telemetry-expanded`
  - Sidebar collapsed: repeat with `--telemetry-collapsed` instead of `--telemetry-expanded`.
  Wait until video is visibly updating before interpreting the 60-second report. The app starts its timer on the first decoded frame and prints median/p95 frame-source update intervals, not physical display-present intervals. If the host is not streaming or no frame arrives, fix the connection first; a no-frame run is not a pacing result.
## M9 — 2015 Intel Mac

The macOS capture, input, VideoToolbox encode/decode, foreground host, app viewer, text clipboard paths, Settings permission panel, per-user Unix-socket IPC, machine-wide CPU sampler, and LaunchAgent scripts/template are source-integrated as recorded in `docs/MACOS.md`. App/capture/input checks and the app strict Clippy pass for the Apple target; the full foreground-host check stops in OpenH264 because target C++ tooling is unavailable. No Mac runtime is verified. The same-user IPC checks and LaunchAgent install/uninstall have not been exercised on the Mac. ADR 0042 selects pointer-in-video on Mac and hidden cursor metadata to avoid a second viewer cursor; confirm both capture paths on hardware. Keep host output at 720p30 until sustained Mac testing supports a higher tier; the product maximum remains 1080p30.

- [ ] [HUMAN-PENDING] Confirm exact Mac model/year in About This Mac and record `sw_vers`, `uname -a`, CPU/GPU, Xcode Command Line Tools, Rust target, and Tailscale app version. Current device details are owner-reported.
- [ ] [HUMAN-PENDING — SETTINGS UI SOURCE-INTEGRATED] On Monterey, verify Screen Recording missing/granted/revoked status is read from the LaunchAgent host process (not just the viewer UI), the explicit button's System Settings destination, and capture's typed permission failure instead of silent black video. If the host agent is offline, Settings must show Unknown. The app does not prompt automatically.
- [ ] [HUMAN-PENDING — SOURCE INTEGRATED; APPLE HOST BUILD BLOCKED] Exercise ScreenCaptureKit and CGDisplayStream fallback with approved content at 480p30, default 720p30, and 1080p30. Record frame intervals, drops, latency, CPU/private memory, verify the pointer is baked into captured video without a duplicate overlay, and test display migration/removal and sleep/wake recovery. Do not capture lock/UAC screens or other private content without the owner's explicit review.
- [ ] [HUMAN-PENDING — SOURCE INTEGRATED; APPLE BUILD/RUNTIME UNVERIFIED] Test VideoToolbox encode/decode and wgpu presentation at supported tiers. Record hardware-acceleration and low-latency property results, bitrate, IDR response, parameter sets, B-frame inspection, upload cost, frame intervals, drops, CPU, and temperature. Run a sustained ten-minute 720p30 test before considering a higher default.
- [ ] [HUMAN-PENDING — SETTINGS UI SOURCE-INTEGRATED] On Monterey, verify Accessibility missing/granted/revoked status is read from the LaunchAgent host process and that the explicit button opens the correct privacy pane. Under owner supervision, inject harmless input and check corners/center on the built-in and an external display, supported keyboard layouts, mouse buttons, drag, wheel, and release-on-disconnect.
- [ ] [HUMAN-PENDING — MAC CLIPBOARD SOURCE INTEGRATED; APPLE HOST CHECK BLOCKED] Verify NSPasteboard viewer and foreground-host paths with Windows peers in both directions. Include text-only Unicode, 512 KiB boundary, explicit opt-in, immediate disable and reconnect reset, echo suppression, and content-free logs. Capability is advertised by the Mac host only because the host bridge is wired; this is not evidence of runtime behavior.
- [ ] [HUMAN-PENDING — SOURCE INTEGRATED; NOT INSTALLED] Run the provided LaunchAgent install/start-at-login/kill/restart/uninstall scripts and verify the user-private Unix socket ownership/mode, data paths, sleep/wake, screen lock, and logged-out behavior. The fixed-message 1 MiB logger is source-integrated; verify the file and directory permissions on the Mac.
- [ ] [HUMAN-PENDING — END-TO-END] Run as host and viewer with each Windows PC over Tailscale. Verify allowlist authorization, display switch, pause/resume, clipboard/input paths, telemetry, and ten-minute thermal/drop counts. Confirm one-second StatsReport reports the actual capture backend, VideoToolbox, resolution, refresh, target bitrate, sent UDP payload bitrate, machine-wide CPU, and host process CPU after the first valid tick interval. No Mac runtime or hardware result is currently verified.

- [ ] [HUMAN-PENDING — CURSOR SOURCE-INTEGRATED] On the Mac, confirm both ScreenCaptureKit and CGDisplayStream include the pointer in captured video and confirm the viewer does not draw a second cursor. The M9 Mac cursor rule is recorded in ADR 0042; Windows continues to use separate cursor metadata.

## M2 — Transport

- [ ] [HUMAN-PENDING] On Windows PC #1, rerun `cargo test -p racc-testkit real_udp_loopback_proxy_preserves_frames_and_recovers_under_seeded_loss -- --nocapture` if firewall or driver conditions differ, and record any deviation from the agent's 2026-10-06 loopback evidence in `docs/TRANSPORT.md`.
- [ ] [HUMAN-PENDING — M7] After Tailscale is installed and confirmed on both Windows PCs, run a 300-frame sender-to-viewer echo over the tunnel. Record direct/DERP path, packet loss, keyframe recovery, and whether large IDRs complete.
- [ ] [HUMAN-PENDING — Mac] Repeat the 233 KB paced-send measurement on the 2015 Intel Mac; record send duration, maximum inter-datagram gap, and sleep granularity against the 60% target.

## M3a — Coordinate input verification (human run required)

- [ ] [HUMAN-PENDING] On each Windows PC, click the four corners and center of every monitor; read back the cursor location with `GetCursorPos` or an on-screen readout. Compare `MOUSEEVENTF_VIRTUALDESK` absolute mapping against `SetCursorPos` using physical coordinates in a per-monitor-DPI-aware process.
- [ ] [HUMAN-PENDING] Repeat the Windows corner and center comparison with mixed monitor scaling at 125% and 150%.
- [ ] [HUMAN-PENDING] On the 2015 Intel Mac, test all four corners and center on the built-in display and an external display if available, using OS-reported point geometry.
## M4a — UI toolkit smoothness check

- [x] [HUMAN-VERIFIED — OWNER; 2026-10-06] On Windows PC #1, watched the iced M4a 1920×1080 synthetic NV12 stream during the 60-second run; owner reported no visible stutter. Agent-side upload intervals still show a 159.509 ms maximum outlier, and framework callbacks do not measure physical presents. Run command: `cargo run --manifest-path spikes/m4a-iced/Cargo.toml --release -- --duration-secs 60`.

## M4b — Fake UI visual review

The owner reports that the current UI is weak and cannot inspect screens now. The app is being improved from the supplied structural reference and AGENTS.md style tokens; this review is recorded in `blocked.md` and does not halt coding per the owner's instruction.


- [ ] [HUMAN-PENDING] Launch cargo run -p racc-app -- --fake; confirm the four regions, fake devices/displays, synthetic stream, and telemetry/events are visible. Check the new header monitor selector and the compact local controls when the device sidebar is collapsed.
- [ ] [HUMAN-PENDING] Review the dark palette, spacing, text sizes, and selected/hover/pressed states against AGENTS.md section 5.2; report concrete changes desired.
- [ ] [HUMAN-PENDING] Collapse and expand both sidebars; resize the window and drag it between displays. Check that the workspace remains usable and the video stays letterboxed.
- [ ] [HUMAN-PENDING] Verify keyboard traversal with Tab and Shift+Tab, activation with Enter and Space, the visible focus ring, and Escape release of keyboard/mouse capture.
- [ ] [HUMAN-PENDING] Check session/host telemetry and event-log readability while the stream runs; report any visible jank or layout disturbance during updates.
- [ ] [HUMAN-PENDING] Confirm Audio is visibly disabled and marked “not supported.” Note any accessibility or screen-reader issues; the current UI does not assign explicit accessible names/roles.
- [ ] [HUMAN-PENDING] Tell the agent what visual or interaction changes to make. M4b remains pending until the requested iterations are reviewed.


## M10 — Polish and packaging

- [ ] [HUMAN-PENDING] Run root setup-windows.ps1 to install prerequisites, build the portable package and Inno installer, and install on each Windows PC. Separately install/uninstall on a clean Windows 10 VM and verify service, firewall, Start Menu, autostart and data cleanup behavior; record prompts.
- [ ] [HUMAN-PENDING] Run root setup-macos.sh on the Mac to build/install the `.app` and current-user LaunchAgents. Verify ad-hoc signing and permission usage strings, test startup/removal with the integrated Unix-socket IPC, and record Gatekeeper behavior. Do not use signing credentials autonomously.
- [ ] [HUMAN-PENDING] Verify tray open/close/quit behavior, single-instance activation, autostart toggles, geometry restoration across display changes, and local IPC behavior. The Windows host currently returns `Unavailable` for hosting enable/disable because its helper cannot safely stop and rebind; confirm that visible response.
- [x] [VERIFIED-RUN — WINDOWS PC #1; 2026-10-07] Final source rebuild: app 12,371,456 B, host-agent 1,876,480 B, portable ZIP 5,644,455 B. ZIP SHA-256: `d8e42189f915329413cc17540be5850762427f4f200a717b5d0a31fdc6e2900e`. Independently confirmed with `Get-FileHash`; not installed or run as a release app.
- [x] [VERIFIED-RUN — WINDOWS PC #1; 2026-10-08] Rebuilt the M10 portable release after the packaging and reconnect fixes: app 12,402,176 B, host-agent 1,900,544 B, ZIP 5,663,590 B. ZIP SHA-256: `5df57c1b4acc4e240a5abe48792329280950085e8f869dc6ab05c1b3c7863b5b`. The sidecar was independently verified with `Get-FileHash`, and the staged ZIP contains `scripts/remove-app-autostart.ps1`. The artifact was not installed or run.
- [ ] [HUMAN-PENDING] Measure release app/host-agent idle private memory and complete the requested 24-hour stability soak.
- [ ] [OWNER DECISION] Choose a project license and confirm personal-only or broader distribution. Review the Noto Emoji license question in `docs/OPEN_QUESTIONS.md`; no Noto artwork is shipped.
