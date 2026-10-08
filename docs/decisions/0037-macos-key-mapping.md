# ADR 0032: Mac key mapping preserves physical modifier positions

Date: 2026-10-07

Status: Accepted for the initial cross-platform mapping.

## Context

The wire input model carries USB HID physical usages, while macOS identifies keyboard positions with virtual key codes. Command and Control have different semantic roles across operating systems, and automatically changing shortcut meaning can surprise users or leak into application-specific behavior.

## Decision

Map physical USB HID positions directly: HID Left/Right Control maps to macOS Control; HID Left/Right GUI maps to the corresponding Command key. Reverse mapping for local macOS viewer capture uses the same table. Do not silently swap Command and Control or translate shortcut meaning in this build. A future, explicit user preference may swap those physical modifier mappings for convenience.

## Consequences

- Ctrl+C forwarded from Windows remains a physical Control+C on a Mac; it does not become Command+C automatically.
- Common shortcuts, left/right modifiers, and international layouts require owner testing.
- The pure mapping table is unit-tested on Windows, while the macOS event-tap capture adapter and real injection remain unimplemented/HUMAN-PENDING.
