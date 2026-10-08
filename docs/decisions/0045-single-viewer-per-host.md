# ADR 0045: One viewer per host in protocol v0

## Status

Accepted for M3b.

## Context

The host control state has one selected display, one capture/encoder pipeline, one active viewer, and no per-viewer stream fanout. Supporting a second simultaneous viewer would require separate viewer authorization, pacing, quality, and lifecycle state.

## Decision

A host accepts one active viewer. A valid Hello received while that viewer's session is active receives `HelloAck(Busy)`. A viewer retries Busy after five seconds. After control loss, the existing session may reconnect during its five-second stop grace period; other Hello requests remain Busy.

## Consequences

No multi-viewer bandwidth or authorization policy is implied. Multi-viewer support requires a separate protocol and host-resource decision.