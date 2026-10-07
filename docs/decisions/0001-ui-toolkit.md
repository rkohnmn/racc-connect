# ADR 0001: UI toolkit

Date: 2026-10-06

Status: Accepted for M4b after Windows PC #1 owner observation on 2026-10-06.

## Context

The app needs a native Rust UI and native wgpu video surface in one graphics context. M4a compared iced and Slint with native four-region mock layouts, synthetic 1920×1080 NV12 input paced at 30 Hz, GPU YUV conversion, and a telemetry field updated at 4 Hz. Both spikes ran on the Windows 10 PC #1 development machine (Windows 10.0.19045, owner-reported NVIDIA GeForce RTX 3050 Ti laptop GPU). The iced shader API does not expose which wgpu adapter the window selected, so the separate adapter enumeration is not attributed to either run.

## Decision

Select iced 0.14.0 with its native wgpu renderer for the app. Put video in an iced shader widget and use its custom `Primitive` implementation to upload/sample the NV12 planes and convert them in the shared render pass. The spike used the iced-re-exported wgpu types and no separate wgpu version. Keep decoded frames on the rendering path; do not route them through the UI event bus.

The measured iced upload cadence was closer to 30 Hz than Slint at the required input period. Iced also draws NV12 directly in the existing UI render pass. Slint can share a wgpu device and queue, but its documented imported-image path in this spike needed a separate NV12-to-RGBA conversion texture/pass before displaying the frame. Iced's permissive MIT license avoids an app-level framework attribution obligation. Its overall project still describes itself as experimental software; keep it behind `racc-app` and pin the tested release deliberately.

## Spike evidence

Both native mock layouts displayed the moving test pattern. Each prototype generated a 1920×1080 NV12 frame (3,110,400 bytes) every 33,333 μs and updated telemetry on a 250 ms period. Commands, complete console output, source, and measurement limits are recorded in the isolated spike READMEs: [`spikes/m4a-iced/README.md`](../../spikes/m4a-iced/README.md) and [`spikes/m4a-slint/README.md`](../../spikes/m4a-slint/README.md).

### Iced

- `cargo check --manifest-path spikes/m4a-iced/Cargo.toml` passed.
- `cargo run --manifest-path spikes/m4a-iced/Cargo.toml --release -- --duration-secs 60` opened the native window and self-exited. The generated source measured 30.00 fps (1,856 frames over 61.868 s including startup). There were 1,685 GPU texture uploads over that full duration; active upload intervals averaged 33.372 ms (29.97 Hz), p95 37.420 ms, max 96.918 ms. Telemetry intervals averaged 249.944 ms (4.00 Hz; 240 updates overall).
- The 23,965 shader `prepare` and `draw` calls and 5,241,024,000 uploaded bytes confirm the custom path ran. Queue-write time is CPU enqueue time, not GPU completion. Iced window-frame subscription events are not physical presentation measurements.

### Slint

- `cargo check --manifest-path spikes/m4a-slint/Cargo.toml` and `cargo fmt --manifest-path spikes/m4a-slint/Cargo.toml -- --check` passed.
- `cargo run --manifest-path spikes/m4a-slint/Cargo.toml --release -- --duration-seconds=60` opened the native window and self-exited with the required 33,333 μs / 250 ms periods. There were 1,760 video submissions over 60.080 s; active intervals averaged 33.67 ms (29.70 Hz), p95 34.68 ms, max 60.23 ms. Telemetry averaged 3.93 Hz (236 updates). `BeforeRendering` callbacks averaged 61.62 Hz, but are not physical presentation evidence.
- Slint 1.18 requires its explicitly unstable `unstable-wgpu-30` integration for shared wgpu access. Its framework is triple-licensed; using its royalty-free terms for a proprietary desktop app carries attribution requirements.

The measured intervals show occasional scheduling outliers, especially in the iced run. Neither spike measured physical monitor presents or GPU completion time. The owner watched the final iced run and reported no visible stutter; this visual observation does not instrument physical presentation cadence.

### Iced owner-observed confirmation rerun

The owner watched the 60-second iced synthetic stream on Windows PC #1 and reported **no visible stutter**. The final visible-run command and full console counters are in `spikes/m4a-iced/README.md`. The source generator produced 1,884 frames (30.00 fps) with 33.334 ms mean, 34.289 ms p95, and 43.990 ms max generation intervals. The renderer uploaded 1,464 latest frames; active upload intervals averaged 33.469 ms (29.88 Hz), p95 35.453 ms, and max 159.509 ms. The gross upload count includes startup and is not a physical presentation count. Telemetry had 240 updates and 249.980 ms mean interval (4.00 Hz). Record the 159.509 ms upload outlier as measured; the owner did not observe visible stutter during this run.
## Owner confirmation

The owner watched the iced 60-second synthetic stream on Windows PC #1 and reported no visible stutter. This completes the M4a human visual check and permits M4b to start. The measured maximum active upload interval remains recorded above; future video integration should preserve latest-frame-wins and revisit it if stutter is observed again.

## Primary references

- [iced 0.14 shader `Primitive` API](https://docs.rs/iced/0.14.0/iced/widget/shader/trait.Primitive.html)
- [iced 0.14 official custom-shader example](https://github.com/iced-rs/iced/blob/0.14.0/examples/custom_shader/src/main.rs)
- [iced license](https://github.com/iced-rs/iced/blob/master/LICENSE)
- [Slint wgpu 30 integration](https://docs.slint.dev/latest/docs/rust/slint/wgpu_30/)
- [Slint unstable cargo features](https://docs.slint.dev/latest/docs/rust/slint/docs/cargo_features/)
- [Slint framework license choices](https://github.com/slint-ui/slint/blob/master/LICENSE.md)
