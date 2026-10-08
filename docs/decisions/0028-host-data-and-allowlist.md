# ADR 0028: Host data directory and allowlist

- Status: Proposed
- Date: 2026-10-07

## Decision

The Windows service will own its settings, allowlist, and size-capped logs under `%ProgramData%\RaccConnect`. The directory ACL must permit SYSTEM and administrators and deny ordinary users write access. The allowlist stores stable Tailscale node IDs and owner-approved labels, never addresses, keys, or credentials. Incoming peers are checked with Tailscale `whois` on every connection and are rejected until their node ID is approved.

The pure allowlist and atomic JSON store are implemented, but the machine-wide path, ACLs, `whois` integration in the listener, and rotating logger are not wired into the host agent.

## Consequences

- State survives user logoff and is available to the service/helper.
- The implementation must verify the directory owner and ACL before reading or writing state.
- No agent should install the service or modify machine ACLs during repository implementation.
