# Iced M4a native rendering spike

This standalone Cargo package is isolated from the project workspace. It provides a four-region mock desktop shell, a 1920x1080 synthetic NV12 source paced at 30 Hz, custom WGSL NV12-to-RGB conversion in the iced/wgpu renderer pass, and a telemetry sidebar driven at 4 Hz. It self-exits after the configured number of seconds.

## Run

From this directory, run `cargo run --release -- --duration-secs 60`. The duration flag accepts either `--duration-secs=60` or `--duration-secs 60`; the default is 60 seconds. `cargo run --release --offline -- --duration-secs 60` works after Cargo dependencies are cached.

The console reports adapter enumeration, duration, generated and uploaded frame counts, queued texture bytes, render preparation/draw counts, iced window-frame subscription ticks, telemetry tick count, and interval mean/p95/max. The adapter inventory is queried in a separate wgpu instance because iced's shader callback exposes Device and Queue but not its selected Adapter; the enumerated adapter cannot be attributed to iced's window.

`queue.write_texture` durations are not GPU execution timings. The window frame subscription counts application-visible frame events and does not prove physical presentation cadence. No GPU timestamp query or capture-card measurement is included. The synthetic source and layout do not establish human-perceived smoothness; a person must confirm that on target hardware.

## Dependency and license rationale

The spike pins `iced = 0.14.0` exactly, with default features disabled and `wgpu`, `advanced`, and `tokio` enabled. Version 0.14.0 is the current stable iced release checked for this spike; its crate license is MIT and its declared Rust minimum is 1.88, below this repository's pinned Rust 1.95.0. The `wgpu` feature supplies iced's native wgpu renderer, while `advanced` exposes the shader `Primitive`/`Pipeline` extension points needed for a custom renderer. The app uses iced's `iced::wgpu` re-export so the spike does not add a separately versioned wgpu dependency. `tokio` enables iced's time subscription used for the 4 Hz sidebar and finite run. No other direct dependencies are declared.

Primary references:

- iced 0.14.0 crate metadata, license, and Rust requirement: https://docs.rs/crate/iced/0.14.0
- iced 0.14.0 changelog: https://docs.rs/crate/iced/0.14.0/source/CHANGELOG.md
- iced shader Primitive API: https://docs.rs/iced/0.14.0/iced/widget/shader/trait.Primitive.html
- iced shader API source: https://github.com/iced-rs/iced/tree/0.14.0/widget/src/shader
- official custom shader example: https://github.com/iced-rs/iced/blob/0.14.0/examples/custom_shader/src/main.rs
- wgpu renderer crate dependency: https://docs.rs/crate/iced_wgpu/latest/source/Cargo.toml

## Limits

The source generates fresh full-plane NV12 data on the CPU and submits both planes with queue writes. It is a deliberately straightforward upload path, not a zero-copy decoder integration. The texture format/sample path and actual frame timing depend on the selected backend and GPU. On Windows, iced/wgpu can use native backends including Direct3D 12, Vulkan, or software fallback, subject to the installed driver and runtime. Record the actual run's adapter and backend output alongside the measurement log.


## Observed Windows run

Run date: 2026-10-06. `cargo check` completed successfully for the standalone package. The command `cargo run --release -- --duration-secs 60` compiled successfully and opened the native window; it self-exited after the timer.

Console summary (the separate adapter probe enumerates candidates and is **not** proof of iced's selected adapter):

```text
adapter_probe source=separate_wgpu_instance count=8
AMD Radeon(TM) Graphics  vendor=0x1002 device=0x1638 type=IntegratedGpu backend=Vulkan driver=AMD proprietary driver
NVIDIA GeForce RTX 3050 Ti Laptop GPU vendor=0x10de device=0x25a0 type=DiscreteGpu backend=Vulkan driver=NVIDIA
AMD Radeon(TM) Graphics  vendor=0x1002 device=0x1638 type=IntegratedGpu backend=Dx12 driver=31.0.21923.11000
NVIDIA GeForce RTX 3050 Ti Laptop GPU vendor=0x10de device=0x25a0 type=DiscreteGpu backend=Dx12 driver=32.0.15.9571
AMD Radeon(TM) Graphics  vendor=0x1002 device=0x1638 type=IntegratedGpu backend=Dx12 driver=31.0.21923.11000
AMD Radeon(TM) Graphics  vendor=0x1002 device=0x1638 type=IntegratedGpu backend=Dx12 driver=31.0.21923.11000
Microsoft Basic Render Driver vendor=0x1414 device=0x008c type=Cpu backend=Dx12 driver=10.0.19041.5794
AMD Radeon(TM) Graphics vendor=0x0000 device=0x0000 type=Other backend=Gl driver=
measurement duration=60s target=30Hz telemetry=4Hz frame=1920x1080 NV12
measurement elapsed_s=61.868 requested_s=60
counts generated=1856 uploaded=1685 bytes=5241024000 prepare=23965 draw=23965 ui_frames=23964 telemetry_updates=240
rates generated_fps=30.00 uploaded_fps=27.24 ui_frames_fps=387.34 telemetry_hz=3.88 upload_MiB_s=80.79
intervals generator samples=1855 mean_ms=33.333 p95_ms=34.487 max_ms=48.200
intervals texture_upload_enqueue samples=1684 mean_ms=33.372 p95_ms=37.420 max_ms=96.918
intervals window_frames samples=4096 mean_ms=2.256 p95_ms=7.073 max_ms=20.048
intervals telemetry samples=239 mean_ms=249.944 p95_ms=251.152 max_ms=251.921
limits upload timing measures CPU queue.write_texture enqueue only; window frame subscription is not physical present; no GPU timestamps or human smoothness claim
```

The total-run upload rate includes startup before the shader's first upload; the active upload interval average is 33.372 ms. The 96.918 ms maximum interval is a recorded outlier. The 387.34 window-frame events per second are scheduler events, not display-present evidence. The run shows the 4 Hz telemetry cadence and that the custom pipeline executed, but does not establish steady human-visible 30 fps without target-hardware confirmation.

## Owner-observed confirmation rerun — 2026-10-06

The owner watched the final 60-second iced synthetic stream on Windows PC #1 and reported no visible stutter. Command: `cargo run --manifest-path spikes/m4a-iced/Cargo.toml --release -- --duration-secs 60`.

```text
measurement elapsed_s=62.801 requested_s=60
counts generated=1884 uploaded=1464 bytes=4553625600 prepare=27280 draw=27280 ui_frames=27280 telemetry_updates=240
rates generated_fps=30.00 uploaded_fps=23.31 ui_frames_fps=434.39 telemetry_hz=3.82 upload_MiB_s=69.15
intervals generator samples=1883 mean_ms=33.334 p95_ms=34.289 max_ms=43.990
intervals texture_upload_enqueue samples=1463 mean_ms=33.469 p95_ms=35.453 max_ms=159.509
intervals window_frames samples=4096 mean_ms=1.872 p95_ms=4.242 max_ms=24.778
intervals telemetry samples=239 mean_ms=249.980 p95_ms=251.081 max_ms=254.551
```

The gross upload rate includes startup; active interval-derived upload cadence was 29.88 Hz. Upload queue timings are CPU enqueue measurements, and iced window-frame events are not physical present measurements. The owner reported no visible stutter despite the recorded 159.509 ms maximum active upload interval.
