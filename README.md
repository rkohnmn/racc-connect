# Racc Connect

Racc Connect is a native Rust desktop project for viewing and controlling displays across the owner's computers over Tailscale. The target is a Windows 10 PC pair and a 2015 Intel Mac. The workspace includes protocol, network, topology/session, telemetry, capture/encode/decode, input, clipboard, identity, core and iced app components. The live discovery and viewer paths are wired in source; real cross-machine operation remains to be verified.

## Status

This is **not a first-release-ready product**. Portable tests cover protocol and state-machine behavior, and M4a's 60-second iced prototype was watched by the owner with no visible stutter. The app has a discovery-first live viewer mode and a separate `--fake` mode. Discovery, viewer networking, decoder and wgpu presentation are connected in source, but no real host-to-viewer session has been verified. Windows service/helper and app-to-host IPC source are present but unverified at runtime; Mac runtime and installer runs remain open. See [progress](docs/PROGRESS.md), [hardware checks](docs/HARDWARE.md), and [blocked work](blocked.md).

## What is implemented or being verified

- Native Rust workspace and iced/wgpu desktop shell with a direct video-surface path.
- Bounded H.264 protocol, TCP/UDP transport foundations, topology math and session transitions.
- Windows capture and encode paths, plus a live viewer/runtime and native wgpu NV12 presentation path; hardware decode, capture and end-to-end behavior still need owner verification.
- Tailscale discovery, host-capability probing, peer approval UI and host local IPC are source-integrated. Live service/IPC behavior and two-PC connections remain HUMAN-PENDING.
- Text clipboard sync is source-integrated for Windows and macOS viewers and hosts. Sync starts disabled, is gated by the active session toggle, and remains unverified across real machines; the owner authorized the text-only transfer. A Windows OS clipboard test with a fake remote passed using fixed test strings.

## Supported targets and limits

- Windows 10 x64 is the active development platform.
- macOS target is a 2015 Intel Mac, expected to run Monterey; current Mac behavior is unverified.
- Networking is limited to Tailscale. There are no public relays, STUN/TURN/ICE, port forwarding, user accounts or passwords.
- H.264 only is required. Resolution is limited to 480p30–1080p30; frame rate is fixed at 30 fps.
- Audio and HDR are not supported. Clipboard scope is text only.
- Windows secure desktop, UAC and service/session behavior has not passed owner testing. Windows.Graphics.Capture fallback cannot see secure desktop.
- Mac hosting defaults to 720p30 until the 2015 machine passes sustained quality, permission and thermal checks. Settings now shows separate Screen Recording and Accessibility status with explicit System Settings buttons; Monterey behavior remains unverified.

## Build and check

For one-command local machine setup and installation, follow [SETUP.md](SETUP.md).

Install the pinned Rust toolchain from `rust-toolchain.toml`. Run from the repository root:

```powershell
scripts/check-all.ps1
cargo run -p racc-app
cargo run -p racc-app -- --fake
```

```sh
scripts/check-all.sh
cargo run -p racc-app
cargo run -p racc-app -- --fake
```

The normal launch starts Tailscale discovery. Use `--fake` for the deterministic test scenario.

Release packaging and checksums are described in [docs/PACKAGING.md](docs/PACKAGING.md). The Windows installer requires Inno Setup 6 installed by the human. The macOS app bundle must be built and checked on the Intel Mac.

## UI preview

A non-private screenshot of the current native window has not been captured into the repository. The owner cannot inspect screens during this work, so visual review remains HUMAN-PENDING; see [docs/UI.md](docs/UI.md). The dark UI has been restyled with original design tokens and branding. Do not substitute the M4a synthetic video test window for a current product screenshot.

## Project documents

- [User guide](docs/USER_GUIDE.md)
- [Development setup](docs/DEV_SETUP.md)
- [Project scope](docs/PROJECT_SCOPE.md)
- [Progress](docs/PROGRESS.md)
- [Hardware checklist](docs/HARDWARE.md)
- [Packaging](docs/PACKAGING.md)
- [Asset provenance](docs/ASSETS.md)
- [Third-party notices](THIRD_PARTY_LICENSES.md)
- [License options](docs/LICENSE_OPTIONS.md)
- [Protocol](docs/PROTOCOL.md)
- [Architecture decisions](docs/decisions/)

## License

The project license is undecided. No `LICENSE` file is present, and no reuse license is granted by this repository. See [docs/LICENSE_OPTIONS.md](docs/LICENSE_OPTIONS.md). This project status is informational, not legal advice.
