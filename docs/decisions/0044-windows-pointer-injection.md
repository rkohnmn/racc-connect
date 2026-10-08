# ADR 0044: Keep both Windows absolute pointer injection methods selectable

## Status

Accepted provisionally. The default remains unverified on real hardware.

## Context

The host must map viewer coordinates to validated physical pixels and support either virtual-desktop absolute `SendInput` or `SetCursorPos`. M7 asks for a corner and center comparison on each monitor before selecting the more accurate default. No such hardware measurements are available in this run.

## Decision

Implement both methods in the Windows host input adapter behind the validated local `RACC_POINTER_INJECTION_METHOD` setting:

- `virtual-desktop-absolute` sends `SendInput` with `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`, using the existing topology conversion and full virtual-desktop bounds.
- `set-cursor-pos` passes the controller-validated host physical pixel to `SetCursorPos`.

The setting defaults to `virtual-desktop-absolute` for compatibility with the existing implementation. Unknown or non-Unicode setting values are errors; the host does not silently fall back when an explicitly supplied value is invalid. Both paths retain the same host session, epoch, display, coordinate, rate-limit, release, and helper input-desktop protections.

## Verification and follow-up

The accepted setting spellings have unit coverage, and the Windows target is compile-checked. This is not a real pointer accuracy measurement. The four-corners-plus-center comparison for every monitor remains HUMAN-PENDING. Keep the current virtual-desktop method provisional until the owner records results; change the default only from those results.