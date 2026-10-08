# ADR 0033: macOS host agent uses a user LaunchAgent

Date: 2026-10-07

Status: Design accepted; implementation and Mac verification pending.

## Context

The host must run in the logged-in user session to access the desktop and privacy grants. macOS has no Windows-style service session that can capture the login window, and the owner must retain control over install and removal.

## Decision

Use a per-user LaunchAgent with `KeepAlive` under `~/Library/LaunchAgents/com.raccconnect.host-agent.plist`. Keep data under `~/Library/Application Support/RaccConnect`, create the IPC parent directory with mode `0700`, create the Unix socket with mode `0600`, and verify the peer socket owner matches the current UID. Rotate a capped host log. Provide human-run install/uninstall scripts and an app status surface; the host-agent never installs or edits its own LaunchAgent. Hosting is available only after user login. Signing/notarization and the distributable app bundle remain M10.

## Consequences

- There is no login-window hosting on macOS.
- Sleep/wake, screen-lock, process restart, socket ownership, and KeepAlive behavior require real-Mac checks.
- This ADR is a design only. No plist, scripts, service registration, socket server, or process launch was created by this M9 pass.
