# ADR 0009: Add control keepalive and cursor-shape messages

## Status

Accepted

## Context

The v0 control message list needs round-trip-time telemetry and reliable cursor bitmap transport. Cursor position and visibility metadata fit in a small UDP datagram, while the bitmap can be larger.

## Decision

Add Ping (type 13) and Pong (type 14) to echo timestamps and nonces for RTT telemetry. Add CursorShape (type 15) to send bounded BGRA bitmaps over TCP. Keep cursor position, visibility, shape identifier, and epoch in the fixed cursor UDP datagram (kind 2).

## Consequences

The v0 protocol supports RTT measurement and cursor shape reuse without placing bitmaps in UDP. These additions are part of protocol version 0 and are specified in docs/PROTOCOL.md.
