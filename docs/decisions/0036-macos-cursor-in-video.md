# ADR 0036: macOS host cursor is included in video

Date: 2026-10-07

Status: Superseded by [ADR 0042](0042-macos-cursor-in-video.md).

## Context

The M9 prompt proposed baking the macOS cursor into captured frames. The decision was later reversed after checking the M9 prompt against the Windows-scoped cursor rule in AGENTS.md. M9 requires Mac cursor-in-video capture.

## Decision

This initial decision was superseded by ADR 0040, then ADR 0042 restored the M9 Mac-specific cursor-in-video requirement.

## Consequences

- Both ScreenCaptureKit and CGDisplayStream configure cursor exclusion.
- A real Mac cursor metadata source and transport remain an M9 implementation requirement.
- No generic arrow or guessed hotspot may be substituted for the active cursor.