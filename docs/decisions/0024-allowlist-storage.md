# ADR 0024: Persist the host allowlist as bounded per-user JSON

## Status

Accepted

## Context

The host needs a persistent explicit approval state keyed by a stable Tailscale node ID and owner login. The identity crate must not call Windows APIs to discover application-data paths and must retain a testable persistence boundary.

## Decision

The caller supplies a per-user config root. Store the versioned JSON at `RaccConnect/allowlist.json` beneath that root. Persist only node ID, optional owner login, user-facing label, added/last-seen timestamps, and Approved/Pending/Rejected state. Bound the file to 256 KiB and 512 entries, with at most 128 pending approvals. Write to a unique temporary sibling, flush, then rename into place. On malformed, oversized, or unsupported content, quarantine the file as a `.corrupt-*` sibling, load an empty list, and emit a recovery event.

## Consequences

- Application code chooses the platform's per-user config root and can inject a fake store for tests.
- A new identity remains unauthorized in Pending state; queue overflow returns Unknown and must fail closed.
- Addresses, tags, and secrets are not persisted.
- The file is local policy state, not a source of authentication; every incoming connection still requires a fresh successful whois identity resolution.