# Racc Connect user guide

This guide describes the intended first-use flow and current human-run package scripts. M10 tray, settings, autostart UI, and window lifecycle code are source-integrated, but native runtime and install behavior are not verified; do not treat an unbuilt package as a supported release.

## Install

### Windows 10

1. Install and sign in to Tailscale on each of your computers. Verify that `tailscale status` lists the expected devices.
2. Build the portable package with `scripts/build-release.ps1` or build the Inno Setup installer as described in `docs/PACKAGING.md`.
3. For a human-run installer, verify its SHA-256 sidecar, inspect the unsigned publisher warning, and install on a test VM before your primary PC.
4. If Windows Firewall blocks the host, review and run `scripts/firewall-rules.ps1` from an elevated PowerShell window. The script limits inbound traffic to Tailscale IPv4/IPv6 ranges. Review paths and dry-run output first.
5. The Windows service and helper entry point and the macOS foreground host command exist in source, but they have not been installed or run on the owner's machines. Copying the binaries does not start a verified host; see `docs/HARDWARE.md` and `docs/PACKAGING.md`.

### macOS 12 or later (Intel target)

1. Install Tailscale and sign in. On Monterey, prefer the app-bundled CLI path documented in `docs/DEV_SETUP.md`.
2. Build the `.app` bundle on the Mac with `scripts/build-macos-app.sh`. The agent has not run this script on the 2015 Mac.
3. In Settings, check Screen Recording and Accessibility status reported by the host agent process. Use the matching Open System Settings button when a permission is missing; status is Unknown while the agent is offline, and the app does not prompt automatically. App autostart starts off and can be enabled in Settings. Grant access only to a build you trust, then reopen the app if macOS requires it.
4. The bundle is ad-hoc signed for personal testing; Gatekeeper may warn. Notarized sharing builds need a human-owned Developer ID and notary keychain profile; see `docs/PACKAGING.md`.
5. The setup script installs the host-agent LaunchAgent and opens the app once; app autostart remains off until enabled in Settings. The host-agent LaunchAgent/Unix-socket IPC have not been run on the Mac. Review `docs/MACOS.md`; do not infer that login hosting has been hardware-verified.

## First run and connecting

1. Start Tailscale on both machines and confirm each peer is online.
2. Start the host agent on the machine whose display you want to share, once its service/runtime setup has been verified.
3. Start Racc Connect on the viewer. The intended discovery source is Tailscale LocalAPI status, followed by a project control handshake. The host checks each peer against its allowlist; a new peer must be approved on the host before streaming.
4. Select the host in the left device rail. Select an available display from its display list. The video should hold the old frame until the new stream's matching keyframe arrives.
5. Choose Auto, 480p, 720p or 1080p. This is a preference; the host enforces 480p30 minimum and 1080p30 maximum, with 30 fps fixed.

**Current status:** discovery-to-live viewing and peer approval have not passed the required two-PC human tests. If the host does not appear, use the troubleshooting table and record the result in `docs/HARDWARE.md`.

## Remote control and release hotkey

1. Select a display, then enable Remote Desktop/keyboard and mouse capture.
2. Move and click inside the letterboxed video area. Input is mapped to the rendered video rectangle, not the entire window.
3. In Settings, choose `Ctrl+Alt+Shift+Escape` or `Ctrl+Alt+Shift+F12`. The selected chord releases capture immediately; `Ctrl+Alt+Shift+Escape` always remains available as an emergency fallback. These chords are consumed locally and never sent to the host.
4. The live viewer mode can discover a host-capable peer and connect when its host agent is running. Keyboard and pointer capture controls are present, but the two-PC input path has not passed hardware verification. Do not test on unsaved work.

## Clipboard

Text clipboard sync is available through the per-session Clipboard control and starts disabled. When enabled for a connected session, it reads and writes text only (up to 512 KiB) with loop prevention; images and files are out of scope. The owner authorized this behavior. A Windows test exercised the OS clipboard against a fake remote using fixed test strings; Windows-to-Windows and Mac round trips remain HUMAN-PENDING.

## Tray and window behavior

The native tray menu is source-integrated with Open Racc Connect, a Hosting action, and Quit. Left click restores and focuses the window. Closing hides it after tray setup; tray activation and a second launch restore it. Quit closes the app only. The Hosting action is sent through local IPC, but the Windows and macOS handlers currently report `Unavailable` because safe start/stop/rebind is not wired; the requested state does not change. Tray, close/restore, and second-instance behavior have compile and fake-test evidence but still need native Windows/macOS runtime verification.

## Troubleshooting

| Symptom | What to check |
|---|---|
| Tailscale not found | Start/login to Tailscale; check `tailscale status`; on Monterey, follow the bundled CLI path in `docs/DEV_SETUP.md`. |
| Host is not shown | Confirm Tailscale says online, verify the agent listener uses a Tailscale address, inspect firewall rules, and check host allowlist approval. The discovery and capability probe are implemented but have not been verified with a live remote host. |
| Black video | Check Screen Recording permission on macOS, Windows capture session and display mode, and the host's capture/encoder logs. A permission or capture error should be recorded, not retried indefinitely. |
| macOS permission prompt | Screen Recording is for capture; Accessibility is for remote input. Review the app identity and restart after changing permission. Rebuilds may require reapproval because ad-hoc signing changes identity. |
| Windows firewall prompt or connection failure | Run the firewall script in dry-run mode and confirm only the two Tailscale ranges are listed. Never create a public or all-address rule to work around a bind issue. |
| Stuck input | Use the release chord, then disable capture. If still stuck, disconnect the viewer and inspect the host injection state. Real injection remains HUMAN-PENDING. |
| Wrong monitor or stale frame | Disconnect/reconnect only after noting the topology and stream epoch; monitor switching must retain the previous good frame until a matching keyframe decodes. |

## Uninstall

On Windows, use the Inno uninstaller or review and run `scripts/firewall-rules-remove.ps1` and `scripts/uninstall-service.ps1` from an elevated PowerShell terminal. App autostart is stored in the current user's Run key and should be removed by uninstall. Settings/allowlist data is kept outside Program Files by default to preserve it; delete only after backing up or if you explicitly want to reset approvals.

On macOS, remove the app bundle and use `scripts/install-launch-agents.sh --disable` to unload/remove the named app and host LaunchAgents. This does not erase user settings or host allowlist. Those package steps are HUMAN-PENDING and should be verified on a clean machine first.
