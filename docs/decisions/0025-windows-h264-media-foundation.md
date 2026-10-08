# ADR 0025: Windows Media Foundation H.264 with same-device NV12 conversion

## Status

Accepted

## Context

Windows hosts need a hardware H.264 path that accepts D3D11 surfaces while the capture backend produces GPU-resident BGRA textures. The encoder must remain replaceable behind the Rust `Encoder` trait and the project cannot ship GPL x264. The input conversion must not add CPU per-pixel color conversion or silently cross D3D11 devices.

## Decision

Use Windows Media Foundation hardware H.264 transforms as the Windows default. Bind the MFT device manager to the D3D11 device that owns the capture texture. Convert and scale BGRA to NV12 with the same-device D3D11 video processor into a fixed pool of three NV12 surfaces, retain leases while MFT output is pending, and return a typed cross-device error on an affinity mismatch.

Require the MFT to accept a zero B-picture count. Attempt the Media Foundation low-latency and real-time codec properties, plus a 250 ms rate-control buffer when available. The buffer property is set in bytes. Do not claim a distinct portable lookahead property is disabled: no separate portable lookahead setting is used. Expose the accepted property state for diagnostics. The force-keyframe request uses the 32-bit unsigned codec property and applies to the next input sample; output keyframe metadata is derived from H.264 Annex B slice NAL type 5; MFT clean-point metadata alone is not treated as an IDR.

Use Cisco OpenH264 under its BSD-2-Clause license as the software encoder for normalized I420 input. Allow only that exact license in the repository license policy. Do not link x264. The OpenH264 adapter maps force-keyframe requests to its public force-intra API and labels only IDR output as a keyframe. No CPU BGRA-to-I420 bridge is introduced in this decision; BGRA capture therefore requires the same-device hardware route, and hardware failure is returned explicitly until an input bridge is separately designed.

Add a synthetic test-pattern `.h264` probe and a separate human-only visible-capture probe. The visible probe requires explicit acknowledgement, bounds duration and frame count, writes Annex B directly, and is never run by automated tests or the agent.

## Consequences

- Capture-to-MFT stays on the owning GPU, with GPU color conversion and a bounded three-surface pool.
- MFT availability and behavior remain vendor/driver dependent. A Windows target compile does not verify hardware runtime, file playback, or display-mode recovery.
- OpenH264 is available as a bounded fallback for captured BGRA: the capture path converts on the GPU to NV12, stages one reusable surface, then copies plane bytes to fixed I420 buffers. Real capture performance remains HUMAN-PENDING.
- Human validation must identify the actual hardware MFT, play a real-capture output, test recovery after a mode change, and exercise software fallback where applicable.
- `openh264` brings native source-build requirements; the macOS cross-check remains unverified in environments without a compatible C++ compiler.
## Follow-up — bounded captured-frame OpenH264 fallback

The initial decision deferred a BGRA capture-to-OpenH264 bridge. M5b follow-up adds a same-device path that still performs color conversion on the D3D11 video processor: capture BGRA is converted to NV12, then a single reusable staging texture is mapped and its luma/chroma bytes are copied into fixed I420 planes. No CPU color conversion is introduced. The encoder-selection result reports the actual backend and fallback reason. This fallback adds one GPU-to-CPU staging readback only after the hardware MFT path is unavailable; its real capture performance remains HUMAN-PENDING.
