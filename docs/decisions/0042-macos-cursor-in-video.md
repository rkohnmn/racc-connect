# ADR 0042: Include the Mac cursor in captured video

Date: 2026-10-07

Status: Accepted; supersedes ADR 0040 for the M9 macOS host.

## Context

The M9 prompt explicitly requires the macOS pointer to be baked into captured video, asks the host to send cursor datagrams with `visible = 0`, and says the viewer must not draw a second pointer. AGENTS.md's separate cursor metadata rule is scoped to the Windows service/helper capture path. ADRs 0036 and 0040 deferred a Mac metadata source and left the pointer absent.

## Decision

Both the ScreenCaptureKit and CGDisplayStream paths set their cursor-in-video option to true. The Mac host continues to send hidden cursor metadata updates for each stream epoch so the viewer does not draw an overlay. Windows capture and pointer rendering remain unchanged.

## Consequences

- Mac pointer movement is visible as part of each captured frame, subject to the fixed 30 fps stream rate.
- No Mac cursor bitmap/hotspot transport is needed for M9 and no generic cursor shape is fabricated.
- Human testing on Monterey must confirm both capture backends include the pointer and the viewer does not render a duplicate.
