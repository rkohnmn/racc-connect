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
- [VERIFIED-RUN] Tailscale is not installed or available on PATH.
- [VERIFIED-RUN] Windows display API reported 3 active displays, each at 1920×1080.

## M5 — Windows capture and encode

- [ ] [HUMAN-PENDING] Record the Windows PC GPU and driver; verify capture and H.264 encode using the available NVENC, QSV, and/or AMF paths.
- [ ] [HUMAN-PENDING] Play the recorded H.264 output and verify recovery after a display mode change.

## M6 — Windows host agent

- [ ] [HUMAN-PENDING] Verify the service and capture helper across lock screen, UAC, logoff/logon, and fast user switching.
- [ ] [HUMAN-PENDING] Verify real Tailscale LocalAPI whois allowlisting and approval behavior.

## M7 — Viewer and input

- [ ] [HUMAN-PENDING] Verify monitor hot-switching across displays with different resolutions and DPI.
- [ ] [HUMAN-PENDING] Measure LAN latency with a photodiode, high-speed camera, or on-screen timestamp overlay and record the method.
- [ ] [HUMAN-PENDING] Verify keyboard layouts beyond US and international input edge cases.
- [ ] [HUMAN-PENDING] Verify real Tailscale direct and DERP path behavior.

## M8 — Clipboard and telemetry

- [ ] [HUMAN-PENDING] Verify text clipboard synchronization end-to-end on the tailnet.

## M9 — 2015 Intel Mac

- [ ] [HUMAN-PENDING] Record macOS version and ScreenCaptureKit availability.
- [ ] [HUMAN-PENDING] Measure sustained VideoToolbox 720p30 and 1080p30, temperature, dropped frames, and permission prompts.

## M2 — Transport

- [ ] [HUMAN-PENDING] On Windows PC #1, rerun `cargo test -p racc-testkit real_udp_loopback_proxy_preserves_frames_and_recovers_under_seeded_loss -- --nocapture` if firewall or driver conditions differ, and record any deviation from the agent's 2026-10-06 loopback evidence in `docs/TRANSPORT.md`.
- [ ] [HUMAN-PENDING — M7] After Tailscale is installed and confirmed on both Windows PCs, run a 300-frame sender-to-viewer echo over the tunnel. Record direct/DERP path, packet loss, keyframe recovery, and whether large IDRs complete.
- [ ] [HUMAN-PENDING — Mac] Repeat the 233 KB paced-send measurement on the 2015 Intel Mac; record send duration, maximum inter-datagram gap, and sleep granularity against the 60% target.
