# ADR 0030: Windows installer and portable package

- Status: Proposed for human build validation
- Date: 2026-10-07

## Context

The first intended audience is the project owner on two Windows PCs and one Intel Mac. The installer tool must be freely available, buildable by the owner, and kept separate from Rust runtime dependencies.

## Decision

Provide an Inno Setup 6 script at `packaging/windows/racc-connect.iss`, a human-invoked wrapper at `scripts/build-installer.ps1`, and a portable versioned ZIP. The installer stages the two binaries under Program Files, creates Start Menu and optional desktop shortcuts, offers a per-user Run-key task, invokes the existing M6 service registration script, installs firewall rules scoped to the Tailscale ranges, and removes the named service/rules and Run value during uninstall. User data remains outside Program Files and is preserved by default.

## Consequences and verification

Inno Setup is not installed in the agent environment, so the `.iss` has not been compiled. The M6 host binary does not yet expose a verified SCM entry point; service install/start is therefore a dependency, not a working claim. Clean-machine install/uninstall, SmartScreen, firewall, app startup and data-retention checks are HUMAN-PENDING. No install script was run.
