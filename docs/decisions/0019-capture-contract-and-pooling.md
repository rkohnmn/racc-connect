# ADR 0019: Capture contract and bounded texture pooling

- Status: Accepted for the M5a foundation
- Date: 2026-10-07

## Context

Capture must remain platform-specific and testable without a GPU. Frames belong on the GPU and a slow downstream consumer must not create a growing queue or stall capture.

## Decision

`racc-capture` defines an unsafe-free `CaptureBackend` trait shared by the fake and platform backends. The caller owns the capture loop and polls bounded events. GPU resources are represented by `GpuResource` and carried by `GpuFrame` leases. The fixed texture pool returns a slot only when the frame's last owner drops it. If every slot is held, the backend drops/counts the newest frame rather than waiting.

Windows allocates three default-usage D3D11 textures with zero CPU access flags and copies from the duplication surface with `CopyResource`. CPU mapping and staging readback are not part of the streaming path.

## Consequences

- Fake tests can exercise pool exhaustion, recycling, migration, and recovery without GPU APIs.
- A caller that holds all three frames will see counted drops; it cannot force unbounded memory growth.
- The API is synchronous/polled; a higher-level host must own a dedicated capture thread and must not call `poll_event` on the UI loop.
- No capture runtime or pixel probe was run for this decision.
