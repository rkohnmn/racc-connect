# ADR 0027: Host local IPC boundary

- Status: Proposed
- Date: 2026-10-07

## Decision

The app and local host agent exchange bounded, length-prefixed JSON requests over a local-only transport. Portable request/response types live in `racc-core::ipc`; Windows will use a named pipe, and macOS will use a Unix-domain socket in a later goal. The Windows pipe must restrict access to the interactive user, administrators, and SYSTEM, and must validate the client token before serving requests.

The platform-neutral framing and in-memory tests exist. The named-pipe server/client and ACL verification do not.

## Consequences

- UI code depends on messages and snapshots, not service implementation details.
- Requests, strings, list counts, and serialized frame sizes are bounded before allocation.
- The local transport must not be exposed on a network interface.
