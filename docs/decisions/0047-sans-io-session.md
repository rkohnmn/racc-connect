# ADR 0047: Keep session transitions sans-I/O and independent of `racc-net`

## Status

Accepted for M3b.

## Context

Connection lifecycle, monitor switching, pause/resume, timeout recovery, and epoch changes need deterministic tests independent of socket scheduling or platform APIs. `racc-net` owns transport behavior and reports network outcomes as ordinary values.

## Decision

`racc-session` owns pure host and viewer transition policy. Its methods receive protocol values, adapter results, and explicit monotonic microseconds, then return typed actions. Runtime dependencies are limited to `racc-proto`, `racc-topology`, and `racc-telemetry`; transport signals arrive through the session API as plain inputs. The crate contains no sockets, threads, clocks, capture, encode, decode, UI, or unsafe code.

## Consequences

State logic can be tested without a live network. Adapters must pass monotonic timestamps for timer-sensitive events and execute returned actions. A bounded deterministic fake control/video-channel scenario harness now links `ViewerSession` and `HostSession`; its scenarios exercise delayed, dropped, and reordered controls without adding transport or I/O dependencies.