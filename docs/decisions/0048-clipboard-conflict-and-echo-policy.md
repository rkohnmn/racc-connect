# ADR 0048: Clipboard conflict ordering and echo suppression

## Status

Accepted for the initial text-only clipboard implementation; real Windows and macOS round trips remain pending.

## Decision

Clipboard updates are scoped to the explicitly enabled, authenticated session and carry an origin, a wrapping sender sequence, and a u64 logical clock. Resolve competing values by newer logical clock first; equal clocks are ordered by greater stable origin ID, then by newer wrapping sequence if origin IDs also match. Reject stale or duplicate sequences and updates from any origin other than the current peer. No wall clock is used.

After applying remote text, suppress the matching local clipboard notification by exact UTF-8 byte equality. The content fingerprint is metadata only and is never sufficient by itself to accept an echo. Clear pending text, sequence/conflict state, and echo state when the session ends or sync is disabled. Never write clipboard content to logs, telemetry, events, or status metadata.

## Evidence and limits

Portable tests cover conflict convergence, clock and sequence ordering, wraparound, echo suppression, and session cleanup. These are synthetic tests; native clipboard behavior and cross-machine ordering have not been verified on the Windows PCs or the Monterey Mac.
