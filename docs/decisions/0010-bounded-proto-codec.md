# ADR 0010: Hand-write bounded protocol serialization

## Status

Accepted

## Context

Protocol input is untrusted. Decoding must enforce explicit field bounds, reject malformed data without panicking, and avoid unbounded allocation. The racc-proto runtime must have no dependencies.

## Decision

Implement serialization with a small internal bounded reader and writer. Keep racc-proto free of runtime dependencies. Use proptest as the only development dependency for generated round-trip and malformed-input tests.

## Consequences

Wire parsing and serialization bounds are explicit in the crate and docs/PROTOCOL.md. Property tests can exercise arbitrary and mutated byte inputs without adding a serialization dependency.
