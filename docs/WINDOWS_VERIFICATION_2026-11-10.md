# Windows verification — 2026-11-10

This follow-up is Windows-only at the owner's direction. The earlier cross-repository improvement backlog is complete. Hardware acceptance remains separate from automated checks.

## Observed on Windows PC #1

- **VERIFIED-RUN:** The release binaries and capture/encode probes built from `f7a999f` with the pinned toolchain. The machine has AMD Radeon and NVIDIA RTX 3050 Ti graphics. Encoder telemetry identifies the Media Foundation hardware backend; it does not establish which vendor encoder was selected.
- **VERIFIED-RUN — synthetic input:** Media Foundation hardware and OpenH264 each encoded 90/90 frames at 854×480, 1280×720 and 1920×1080. FFprobe reported H.264 Main, the expected dimensions, 90 decoded frames and no B-frames for all six files. FFmpeg decoded each file without errors. Configuration was 30 fps and 4 Mbit/s; raw H.264 does not establish sustained capture cadence.
- **VERIFIED-RUN — owner-approved desktop capture:** After the owner confirmed screen readiness, the bounded probe captured the primary 1920×1080 display and encoded 90/90 frames at 1280×720 through WindowsMediaFoundationHardware. All 90 frames decoded without errors and with no B-frames. The first decoded frame visibly contained desktop content. This is a short capture/decode result, not a two-machine session or recovery test. The recording and image remain in the local task workspace and are not committed.
- **VERIFIED-RUN — synthetic UI input:** The release app's 60-second fake run completed with 1,702 frame-source samples, median 31.59 ms, p95 47.26 ms and 215 intervals above 40 ms. These are app-observed frame-source updates, not physical presents. Automated window capture timed out twice, so this run does not establish visual acceptance.
- **VERIFIED-RUN — installed host diagnostic:** A read-only `GetStatus` request connected to the installed named pipe but received EOF before its reply completed. Its binary hash differs from the workspace release. No installed service or settings were changed.

## Native IPC regression

**VERIFIED-RUN — Windows PC #1:** A deterministic test uses a unique local named pipe. The client sends `GetStatus`, waits until the server has written its reply and destroyed its stream, then reads the reply. It failed with the original forced `DisconnectNamedPipe` and passed after removing that call. Each connection owns a new pipe instance; its handle is still closed through RAII. Existing transfer deadlines and pipe security are unchanged.

Microsoft documents that [`DisconnectNamedPipe` discards unread data](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-disconnectnamedpipe). This explains the source-level reply-loss race. The installed service requires an update and a separate status recheck before its observed failure can be declared resolved.

## Automated validation

**VERIFIED-RUN — Windows PC #1:** The complete `scripts/check-all.ps1` passed after the IPC change: formatting, strict workspace Clippy, workspace tests, layering/feature gates, notices/icons/packaging/checksum/hygiene and both target cargo-deny checks. Existing advisory warnings retain the configured policy. The clipboard-writing OS test remains intentionally ignored.

## Remaining Windows acceptance

- Update the installed app/host from a tested package, then recheck local status and tray/single-instance behavior.
- Establish access to PC #2 and verify a real approved session in both directions: topology, first frame, sustained playback, monitor switching, pause/resume, input, text clipboard and reconnect. The November 4 Tailscale ping reached an online Windows peer through DERP; that is connectivity evidence only.
- Run supervised lock/UAC/logoff/session-change and display recovery tests. The helper is limited to the default desktop; secure-desktop capture is not established.
- Review the UI, build/test an Inno installer and clean-VM uninstall, measure idle resources and complete the 24-hour soak.
- The hosting enable/disable command still returns `Unavailable`; reversible runtime teardown/rebind is a separate implementation item.
- Project license/distribution and signing remain owner decisions. Mac hardware testing is excluded from this pass, not marked complete.

The detailed manual checklists remain in [HARDWARE.md](HARDWARE.md).
