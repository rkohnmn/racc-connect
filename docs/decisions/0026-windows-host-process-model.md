# ADR 0026: Windows service and console-session helper

- Status: Proposed
- Date: 2026-10-07

## Decision

The Windows host will use an automatic-start LocalSystem service as a lifecycle supervisor and a separate helper process in the active interactive user session for capture, encoding, networking, and input (input is M7). The service will react to session change notifications and restart the helper with bounded backoff and crash-loop cooldown. The agent must not claim secure-desktop capture until a dedicated implementation is proven.

The helper-launch implementation is not yet present. The intended Windows approach is `WTSGetActiveConsoleSessionId`, `WTSQueryUserToken`, and `CreateProcessAsUserW` targeting `WinSta0\Default`, with owned handles/tokens. It provides an interactive user desktop, not a guarantee of access to the Winlogon/UAC secure desktop.

## Consequences

- Capture never runs inside session 0.
- Lock, UAC, sign-out, and fast user switching require explicit supervisor transitions and owner testing.
- Secure-desktop capture remains a known limitation unless a separate reviewed design is implemented.
- Service installation is deferred until a real `service` subcommand is available.
