# ADR 0040: Keep the macOS cursor separate from video

Date: 2026-10-07

Status: Superseded by [ADR 0042](0042-macos-cursor-in-video.md).

## Context

This decision followed a generalized reading of the Windows cursor rule and deferred Mac cursor metadata. M9 explicitly requires the Mac cursor to be baked into video with separate cursor datagrams hidden; see ADR 0042.

## Decision

This decision is superseded. Mac capture includes the cursor in frames as required by M9; Windows continues to carry cursor metadata separately.

## Consequences

- Captured Mac video follows the cross-platform cursor contract.
- The remote pointer remains hidden until actual metadata capture and transport are implemented; cursor pixels are not fabricated.
- Human testing on the Mac must verify both capture backends exclude cursor pixels.
- The M9 cursor-rendering acceptance item stays open until metadata works end to end.