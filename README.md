# Native Low-Latency Multi-Computer Remote Desktop (working title)

A Rust desktop application for viewing and controlling displays across the owner's computers on Tailscale. Every supported machine can host and view a session.

## Current status

M0 workspace scaffold. The crates are compiling stubs; implementation has not started. See docs/PROGRESS.md for verification evidence and milestone status.

## Checks

From the repository root, run scripts/check-all.sh in a Bash environment or scripts/check-all.ps1 in PowerShell. The checks format, lint, test, verify dependency layering, and run cargo-deny when it is installed.

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