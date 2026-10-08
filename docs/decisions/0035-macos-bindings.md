# ADR 0030: macOS framework binding boundary

Date: 2026-10-07

Status: Accepted for the M9 compile-only foundation; Objective-C framework integration deferred.

## Context

M9 needs CoreGraphics display queries and preflight permission checks now, then Objective-C ScreenCaptureKit and VideoToolbox APIs. Unsafe must stay in narrowly scoped macOS modules, and the Windows dependency tree must not acquire Apple binding crates.

## Decision

For this foundation, call the C APIs from CoreGraphics, ApplicationServices, and CoreFoundation through direct `cfg(target_os = "macos")` FFI declarations. The declarations and unsafe calls are isolated in `crates/capture/src/macos/` and `crates/input/src/macos/`. Every call documents its pointer/value invariants; Create-rule CG/CF references are released. No third-party dependency was added, so there is no new crate license or release to record and Cargo.lock does not gain an Apple binding dependency from this M9 work.

When implementing Objective-C ScreenCaptureKit, VideoToolbox, or NSPasteboard classes, prefer the maintained `objc2` family plus RAII wrappers. Do not extend handwritten `objc_msgSend` dispatch. Resolve and record the exact binding crate versions, SPDX licenses, release dates, feature flags, and both target dependency trees in the follow-up ADR before adding those dependencies.

## Consequences

- Windows builds remain isolated from Apple system declarations.
- Capture permission and display metadata can be target-checked on a Windows host, but that check does not link or execute Apple frameworks.
- Capture streams, VideoToolbox sessions, and pasteboard are not implemented by this ADR.
