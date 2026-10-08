# Packaging and first-release procedure

## Current release state

M10 packaging sources and support artifacts are being prepared from the Windows 10 PC #1 development environment. This repository is still a personal-use prototype: the app's native tray, startup behavior, Settings UI integration, host service lifecycle and Mac runtime have not all been observed end to end. No installer, firewall rule, service, LaunchAgent, signing identity, or notarization credential was used by the implementation agent. Every platform install step below is **HUMAN-PENDING**.

Distribution scope is **personal-only as an assumption from `docs/PROJECT_SCOPE.md`**, pending owner confirmation. The project license remains undecided. No `LICENSE` file is added. These are not legal conclusions.

## Reproducible portable release

The workspace release binaries use the profile in the root Cargo manifest. Package archives are assembled in a stable path order with fixed ZIP timestamps; each archive has a SHA-256 sidecar. The Rust toolchain and lockfile are pinned. Generated dependency notices are part of the archive.

Windows PC:

```powershell
python scripts/gen-notices.py --check
scripts/build-release.ps1 -DryRun
scripts/build-release.ps1
```

macOS:

```sh
python3 scripts/gen-notices.py --check
scripts/build-release.sh --dry-run
scripts/build-release.sh
```

The release script rejects Linux because this project supports Windows and macOS only.

Artifacts go to ignored `dist/`. The portable archive contains the app, host-agent, support documentation, icon outputs and the platform's human-run setup scripts. ZIP timestamps are normalized to 1980-01-01 UTC; compression output can still differ across Python/zlib implementations or binary toolchains. Checksums identify the exact produced bytes; they are not a claim that all compiler builds are byte-for-byte reproducible.

A release build and artifact sizes are recorded in the current session entry in `docs/PROGRESS.md`. Private idle memory and 24-hour resource use remain unmeasured; record them in `docs/HARDWARE.md` after observing the release processes. Do not infer idle memory from fake UI measurements.

The release profile uses measured thin LTO, one codegen unit, symbol stripping, and unwind panic behavior; see [ADR 0032](decisions/0032-release-profile.md) for the default-versus-candidate sizes and build times. Startup behavior and idle private memory remain unmeasured; the release build has not been run as a live app or host. The app now installs the settings module's panic hook during startup. The hook writes only a bounded panic location/message record and does not inspect frames, clipboard, input, or environment values.

## Windows installer

The chosen installer is **Inno Setup 6**, a freely available Windows installer compiler. The agent did not install it. After producing the portable zip, extract it so the stage exists at `dist/racc-connect-<version>-windows-x64/`, then from the repository root run:

```powershell
$env:RACC_VERSION = '0.1.0' # use the version printed by cargo metadata
& 'C:\Program Files (x86)\Inno Setup 6\ISCC.exe' packaging\windows\racc-connect.iss
```

The script installs both binaries beneath Program Files, adds a Start Menu shortcut, offers an optional current-user Run-key task, calls the existing service registration script, adds scoped Windows Firewall rules, and schedules removal of those service and firewall rules on uninstall. The user-data directory remains outside Program Files and is not removed by default. `scripts/uninstall-service.ps1` and `scripts/firewall-rules-remove.ps1` can be reviewed independently.

Before release, **HUMAN-PENDING**: install Inno Setup, build the installer, inspect the command output, install and uninstall on a clean Windows 10 VM, observe SmartScreen and firewall prompts, verify SCM/helper startup, confirm Run-key behavior, verify only Tailscale ranges are allowed, and check cleanup. SCM service and helper source exists but has not been installed or run on either Windows PC; do not treat the installer source or portable ZIP as a verified host product.

Run firewall setup only from an elevated PowerShell session and only after reviewing the displayed paths:

```powershell
scripts/firewall-rules.ps1 --agent-path 'C:\Program Files\Racc Connect\racc-host-agent.exe' --app-path 'C:\Program Files\Racc Connect\racc-app.exe' --dry-run
# Human action after review, elevated:
scripts/firewall-rules.ps1 --agent-path 'C:\Program Files\Racc Connect\racc-host-agent.exe' --app-path 'C:\Program Files\Racc Connect\racc-app.exe'
```

This permits inbound TCP 47473 for the host-agent and inbound UDP for the viewer app, restricted to `100.64.0.0/10` and `fd7a:115c:a1e0::/48`. Removal script deletes only the two named rules. Neither script has been executed against Windows Defender Firewall.

The installer source calls the M6 PowerShell service registration and starts the service. SCM entry, service supervision, and helper-launch source are present, but have not been installed or run on either PC. The installer source has not been compiled by ISCC because Inno Setup is not installed here.

## macOS app bundle and login agents

`scripts/build-macos-app.sh` must run on an Intel Mac with the pinned Rust toolchain and Xcode command-line tools. It creates `dist/Racc Connect.app`, embeds the app and host-agent, adds the app icon and third-party notices, writes `Info.plist` with Screen Recording and Accessibility explanations, chooses a normal Dock-visible app (`LSUIElement=false`), ad-hoc signs for local testing, and runs `codesign --verify` and `plutil -lint`.

The script is **not run** on Windows. **HUMAN-PENDING on the 2015 Mac:** build the Intel bundle, inspect its contents, verify ad-hoc signing, launch it, grant Screen Recording and Accessibility, and check host/viewer runtime and thermal behavior. A per-user host LaunchAgent template/script and a private Unix-socket host IPC server/client are source-integrated, but neither was run on a Mac, so background login hosting is not verified.

`scripts/install-launch-agents.sh` writes or removes only the current user's two named LaunchAgents; `scripts/uninstall-launch-agents.sh` is the matching removal entry point. The app agent uses the same `com.racc.connect` label as the Settings autostart toggle and starts at login without KeepAlive; the host agent invokes `racc-host-agent host` with KeepAlive enabled so launchd restarts it after failure. The foreground Mac host ignores optional stdin EOF so it can remain alive under launchd. `packaging/macos/com.racc.connect.host-agent.plist.in` records the host launch contract. Newer login-item APIs are not required because the target may be macOS 12. The scripts, template, and private Unix-socket IPC have not been run on a Mac. The source includes a fixed-message 1 MiB-capped host log; no LaunchAgent or logger was run on the Mac. No LaunchAgent was installed by the agent.

For sharing beyond the owner's personal devices, an ad-hoc signature is insufficient. The owner must use a Developer ID Application certificate, hardened runtime, timestamped signing, and a Keychain notary profile. `scripts/notarize-macos.sh` takes those values from the owner's environment, submits via `notarytool`, staples the result, and creates a DMG. It was not run and contains no credentials.

macOS firewall prompts are controlled by the macOS Application Firewall and Tailscale's network extension. The owner should approve the signed app when prompted, verify the connection over Tailscale, and record which prompts appeared. There is no macOS script that weakens the firewall or exposes a listener on public interfaces.

## Signing and SmartScreen

Windows code signing is not attempted. An unsigned installer or executable may show SmartScreen's publisher warning; the owner should verify the downloaded archive checksum and expected publisher/source before choosing **Run anyway**. A public distribution should use a trusted code-signing certificate and stable signing identity. Self-signing does not establish publisher trust.

macOS ad-hoc signing is for local evaluation and is not notarization. Gatekeeper may warn or refuse to open it; right-click override behavior should only be used for an artifact the owner built and verified. Public distribution should use Developer ID signing with hardened runtime and notarization. The agent must never receive or handle those credentials.

## Tray, close and single instance

The app now uses `tray-icon` 0.24.2 (MIT OR Apache-2.0), with a native Windows/macOS icon and menu. The icon is created by the iced update callback after the event loop has started, as required by the tray library. Since iced does not expose the underlying winit event proxy, the app polls tray and menu event receivers on its 250 ms UI tick; actions therefore have up to one tick of dispatch delay. The menu provides Open Racc Connect, a checked Hosting preference, and Quit. Left-click restores and focuses the window. The tooltip shows only the app name and connection state. Windows switches between generated connected/disconnected color icons; macOS uses the generated monochrome template image. Quit exits the UI process only.

Close requests now hide the native window after tray initialization. The app obtains the raw window handle inside iced's UI-thread `window::run` callback and uses Win32 `ShowWindow` or AppKit `NSWindow::orderOut`; tray Open and second-instance requests show and focus the window. If the tray or native operation is unavailable, the app falls back to the taskbar or Dock. The app marks the viewer hidden before invoking the native operation, so the viewer pauses decoding and sends its visibility command while hosting remains in the separate agent.

Saved logical geometry is validated against Windows monitor work areas converted with each monitor's effective DPI or macOS visible frames before the first window opens. If OS enumeration fails, it clamps to a bounded 1280×800 origin fallback. Cross-target compilation and the pure geometry/menu tests pass, but platform runtime behavior remains **HUMAN-PENDING** on both machines. The app now sends hosting enable/disable requests through local IPC; the Windows and macOS handlers currently return `Unavailable` because a safe host lifecycle toggle is not wired, so hosting does not change from that request. Tray icon/menu/tooltip, close/show, single-instance activation, and multi-monitor restore need hardware observation before release.

Settings now load from the per-user JSON file, apply sidebar/quality/hosting defaults, persist preference changes and window geometry, and display sanitized recovery notices. Startup geometry validation uses the current OS work areas, with a bounded origin fallback if monitor enumeration fails. The About page embeds the generated dependency notices and project artwork attribution. The project license remains undecided; no `LICENSE` file is present.

## License notices and license selection

`THIRD_PARTY_LICENSES.md` is generated from the non-development dependency graph of both binaries with `scripts/gen-notices.py`. `--check` fails when a dependency has no recognized SPDX license expression or when `THIRD_PARTY_LICENSES.md` differs from the current resolved dependency graph or `docs/ASSETS.md` artwork provenance. The resolved package inventory and license texts found in the local Cargo source cache are included. Cargo-deny policy was not changed.

The source has no chosen project license. `docs/LICENSE_OPTIONS.md` is comparison material, not legal advice. The Noto Emoji raccoon artwork was not used; `docs/ASSETS.md` records the original vector artwork and the blocked Noto verification path.

## First-release human checklist

1. Confirm personal-only distribution scope or decide to share, and choose a project license separately. Do not publish with an implied license.
2. Run `scripts/check-all.ps1`, build Release, record app and agent binary sizes, and measure idle private memory.
3. Install Inno Setup 6, validate the `.iss`, build the Windows installer, and inspect the signed/unsigned warning behavior.
4. Install and uninstall on a clean Windows VM; test service startup, firewall scope, app autostart, tray/close/quit semantics, settings migration, and data preservation.
5. On the Mac, build and verify the Intel app bundle, permission prompts, tray/Dock choice, LaunchAgents, sustained VideoToolbox behavior and thermals.
6. If sharing, obtain and use owner-managed signing credentials, verify SmartScreen/Gatekeeper behavior, and complete notarization on the Mac.
7. Verify upgrade preserves settings and the service allowlist, then observe Windows and Mac resource usage for 24 hours.
8. Review `docs/HARDWARE.md` and `blocked.md`; remove pending marks only after the corresponding observation is recorded.
