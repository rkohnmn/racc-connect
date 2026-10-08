# ADR 0021: DXGI Desktop Duplication as the Windows capture primary

- Status: Accepted for the M5a foundation
- Date: 2026-10-07

## Context

The Windows host requires damage-driven GPU-resident capture, monitor migration, and a recoverable path across mode/device changes. A Windows.Graphics.Capture fallback has different access and secure-desktop limitations.

## Decision

Use DXGI Desktop Duplication as the primary backend. Try `IDXGIOutput5::DuplicateOutput1` with BGRA, then fall back to `IDXGIOutput1::DuplicateOutput`. Handle access loss, device removal/reset, unsupported output, session disconnect, mode changes, protected-content reports, and display removal through typed recovery signals and bounded backoff. Keep a future Windows.Graphics.Capture implementation behind the same trait; do not silently switch to it in this milestone.

## Consequences

- Frames are copied once between GPU textures and stay GPU-resident for downstream encode.
- Hybrid-GPU adapter compatibility and all driver-specific behavior remain unverified until a human runs the probe on each display.
- Secure desktop capture is not implemented here; the M6 helper process owns that work.
- No WGC capture path or screen probe was run in this session.
