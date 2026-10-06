# Racc Connect

Racc Connect is a native Rust desktop application for viewing and controlling displays across the owner's computers over Tailscale. Any supported machine can host or view a session; video is rendered through a native wgpu surface, while capture, encode, decode, input, clipboard, networking, and telemetry are planned as separate Rust components.

## Current status

M0.5 housekeeping is complete. The workspace still contains compiling stubs; M1, the protocol crate, is next. See [progress](docs/PROGRESS.md) for verification evidence and milestone status.

## Checks

From the repository root, run `scripts/check-all.sh` in Bash or `scripts/check-all.ps1` in PowerShell. Both run formatting, lint, tests, dependency layering, and `cargo deny check --warn vulnerability --warn unsound --warn unmaintained --warn notice --warn yanked`. Install Rust using the pinned `rust-toolchain.toml`; `cargo-deny` is required for the full check.

## Project documents

- [Project scope](docs/PROJECT_SCOPE.md)
- [Development setup](docs/DEV_SETUP.md)
- [Progress](docs/PROGRESS.md)
- [Open questions](docs/OPEN_QUESTIONS.md)
- [Hardware checklist](docs/HARDWARE.md)
- [Protocol draft](docs/PROTOCOL.md)
- [Packaging notes](docs/PACKAGING.md)
- [Architecture decisions](docs/decisions/)
- [M0 goal](docs/goals/M0.md)
- [M0.5 goal](docs/goals/M0.5.md)

## License

No license has been chosen yet. Until one is added, all rights are reserved by the author; viewing and forking on GitHub is permitted by GitHub's terms, but no other reuse rights are granted.
