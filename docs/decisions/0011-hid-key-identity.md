# ADR 0011: Use USB HID usage codes for key identity

## Status

Accepted

## Context

Keyboard input needs a cross-platform identity that Windows and macOS can map to and from. Unicode text injection for international layouts is a separate unresolved behavior.

## Decision

Encode key input using USB HID usage codes from usage page 0x07, plus pressed state and shift, control, alt, and meta modifier bits. Defer Unicode text injection.

## Consequences

InputEvent has a stable protocol representation for physical key identity. International text injection remains outside this v0 message and is tracked in docs/OPEN_QUESTIONS.md.
