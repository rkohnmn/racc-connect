# Hardware Verification Record

The checks below require the project author's machines or tailnet. They are not complete until the human records results.

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

## Owner-reported hardware inventory

The following specifications were supplied by the owner on 2026-10-06. They have not been independently verified or measured by the agent.

## Environment record

Windows PC #1 (Laptop): Windows build ____  CPU AMD Ryzen 7 5800U  GPU NVIDIA GeForce RTX 3050 Ti  driver ____  monitors ____  RAM 16 GB at 4266 MHz
Windows PC #2 (Server PC): Windows build ____  CPU AMD Ryzen 9 5900X  GPU AMD Radeon RX 7900 GRE  driver ____  monitors ____  RAM 32 GB DDR4
MacBook (exact model and year not supplied): macOS Monterey 12.7.6  CPU Intel Core i7 2.2 GHz quad-core  GPU Intel Iris Pro 1536 MB  RAM 16 GB 1600 MHz DDR3
Tailscale versions: PC1 ____  PC2 ____  Mac ____
Agent sandbox: OS ____  Rust ____  Windows cross-check ____  macOS cross-check ____