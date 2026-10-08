# ADR 0039: VideoToolbox bindings and ownership boundary

Date: 2026-10-07

Status: Accepted for compile-only M9 implementation; Mac runtime validation remains pending.

## Context

ADR 0035 deferred Objective-C framework integration until the exact bindings, licenses, release dates, and target-specific dependency trees were checked. M9 now needs CoreMedia/CoreVideo C APIs and VideoToolbox compression/decompression sessions without hand-written Objective-C dispatch.

## Decision

Use the maintained `objc2` framework bindings with narrow features and target-specific Cargo dependencies. Keep FFI calls and unsafe blocks inside `crates/encode/src/macos.rs` and `crates/decode/src/macos.rs`. Use `CFRetained` for create-rule objects and invalidate VideoToolbox sessions before callback state is dropped. The decoder copies bounded NV12 planes into the existing owned-frame contract; zero-copy Metal sharing remains a later optimization.

Resolved binding metadata, checked with `cargo info` and the crates.io/docs.rs version records:

| Crate | Locked version | SPDX license expression | Version release date |
|---|---:|---|---|
| `objc2-core-foundation` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | 2025-10-04 |
| `objc2-core-media` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | 2025-10-04 |
| `objc2-core-video` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | 2025-10-04 |
| `objc2-video-toolbox` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | 2025-10-04 |
| `objc2` (transitive) | 0.6.5 | `MIT` | 2026-10-05 |
| `objc2-encode` (transitive) | 4.1.0 | `MIT` | 2025-01-22 |

The framework crates require `objc2 >=0.6.2, <0.8.0`; the lockfile resolves 0.6.5. Their 0.3.2 releases are from the same objc2 release on 2025-10-04. The objc2 0.6.5 release is current in this lockfile and was published 2026-10-05.

Feature sets are limited to CoreFoundation `CFArray`, `CFDictionary`, `CFNumber`, `CFString`, `std`; CoreMedia `CMBase`, `CMBlockBuffer`, `CMFormatDescription`, `CMSampleBuffer`, `CMTime`; CoreVideo `CVBase`, `CVBuffer`, `CVImageBuffer`, `CVPixelBuffer`, `CVReturn`; and VideoToolbox `VTBase`, the relevant compression/decompression session and property features, `VTErrors`, `VTSession`, and the CoreMedia/CoreVideo bindings.

Target dependency audit (`cargo tree -e normal`):

- macOS `racc-encode` and `racc-decode` include the four framework crates above, with `objc2`, `objc2-encode`, and `bitflags` transitively.
- Windows target trees exclude all four Apple framework crates. macOS target trees exclude Windows API crates. `racc-encode` also retains its separate BSD-licensed OpenH264 software fallback.

## Consequences

- The encoder sets H.264 Main, real-time mode, no frame reordering, 30 fps, a one-hour keyframe interval, average bitrate, a one-second hard data-rate limit at twice that bitrate, and per-frame forced IDRs. The decoder requests video-range NV12 output.
- The VideoToolbox backends use retained ownership wrappers and callback state whose lifetime extends through session invalidation.
- The decoder's straightforward first path copies NV12 planes to CPU-owned memory; no CPU color conversion is performed here.
- `racc-decode` passes `cargo check -p racc-decode --target x86_64-apple-darwin` on the Windows development host. The actual encoder module type-checks against the same Apple target and bindings in isolation. The full `racc-encode` target check is blocked before Rust compilation because the bundled OpenH264 source needs an Apple C++ toolchain unavailable on this host. This is compile evidence only; no Mac runtime, encoder performance, decoder output, or hardware acceleration is claimed.
- Real VideoToolbox behavior, hardware selection, and 2015 Intel Mac performance remain HUMAN-PENDING.
