# Blocked and deferred verification

This is a review queue, not a stop condition. The owner asked us to continue through the prompts using best judgment and to record work that cannot be verified from the current Windows PC. We continue implementation and mark these items honestly in `docs/PROGRESS.md` and `docs/HARDWARE.md`.

| Item | Why it is blocked or deferred | Review or verification needed |
|---|---|---|
| M4b visual acceptance | The owner said the shell looked weak and cannot review the running screens right now. The attached image is structural inspiration only; the app is being restyled with original branding. | When convenient, inspect the app and report iterations against the M4b checklist in `docs/HARDWARE.md`. This no longer blocks later coding. |
| M5a/M5b real capture and encode | Current execution host is Windows PC #1; PC #2 GPU and its capture path are not available here. Real adapter-specific capture, hardware MFT, playback, mode-change recovery, and cursor behavior require hardware observation. | Run the capture/encode probes on both PCs, play the H.264 output, and record adapters, timing, copy behavior, recovery, cursor modes, CPU/memory, and driver results. |
| M5c live Tailscale identity | Tailscale is not installed or available on this PC; the prompt forbids installing it autonomously. | Owner installs/logs in on the machines and verifies status, peer discovery, `whois`, direct/DERP path, allowlist approval and rejection. Synthetic parser and allowlist work can proceed. |
| M6 service and secure desktop | Installing/uninstalling a Windows service needs an elevated owner session; lock, UAC, logoff and fast user switching require real interactive sessions. | Owner runs the documented install/dry-run and recovery checks on both PCs, including helper restart and secure-desktop behavior. We implement and test the supervisor and IPC with fakes. |
| M7 real cross-machine viewer/input | The second PC and working tailnet are not available to this process. Physical pointer accuracy, non-US keyboard behavior, real GPU decode and LAN/Tailscale latency cannot be inferred from loopback tests. | Owner verifies PC-to-PC streaming, monitor switch, input, pause/resume/reconnect, direct/DERP, pointer coordinates, latency and soak behavior. |
| M8 real clipboard and adaptive telemetry | The app currently lacks a real host runtime/tailnet. Clipboard interaction with elevated/locked applications and real path/loss telemetry need the machines. | Owner checks text sync both directions (including non-ASCII/large text/repeated copies), compares telemetry with Tailscale tools and runs impairment/adaptation measurements. |
| M9 macOS runtime | This session runs on Windows. A Mac cross-check is compile-only; it cannot verify ScreenCaptureKit, VideoToolbox, permissions, thermals or Accessibility input. The exact Mac model is still owner-reported. | Run the M9 build and tests on the 2015 Intel Mac; confirm model/macOS, permissions, sustained 720p30 then 1080p30, recovery, input and Windows↔Mac sessions. |
| M10 installers and release signing | Installer builds can be prepared, but clean-machine install, firewall, autostart and 24-hour checks need owner machines. Signing/notarization needs owner-held credentials and must not be handled by the agent. | Owner tests the Windows installer and Mac bundle/LaunchAgent, and runs notarization only if desired. License remains `undecided`; distribution defaults to personal-only for the three owner machines unless the owner changes it. |
| M11a multi-stream runtime | Its prompt requires M8 and M10 first; window capture, window tracking and multi-stream behavior need the completed host/viewer stack. | Implement after M8/M10 code is in place; run multi-stream, resize, minimize/restore, protection and input tests then. |

## Current coding policy

- Continue prompt-by-prompt implementation and automated verification while the checks above remain pending.
- Do not claim compile-only or fake results as hardware verification.
- Do not install services, manipulate firewall rules, sign binaries, notarize, install Tailscale, or use owner credentials autonomously.
- Preserve the project limits: H.264, 480p30–1080p30, no audio, no HDR, no clipboard images/files, no NACK/FEC, and Tailscale-only networking.
