# M4a Slint toolkit spike

This isolated crate shows a native Slint window with a four-region mock layout, 1920×1080 synthetic NV12 Y/UV plane uploads, a fullscreen-triangle fragment shader that converts YUV to RGBA on the shared WGPU device/queue, an imported Slint image, and telemetry text updated at 4 Hz. The texture is re-uploaded and converted once per video timer tick. This is a toolkit experiment, not application code.

The run measured the `BeforeRendering` callback as a framework render proxy; it cannot confirm that frames reached the physical monitor. No desktop capture or screenshot was performed. Human visual smoothness confirmation remains outstanding.

## Run

From the repository root:

```powershell
cargo run --manifest-path spikes/m4a-slint/Cargo.toml --release -- --duration-seconds=60
```

The default run is 60 seconds; `--duration-seconds=N` overrides it (minimum 3 seconds). The run shuts its window when the timer expires. Its timer periods are exactly 33,333 μs for video and 250 ms for telemetry.

## Recorded measurement

Run on Windows 10.0.19045 on 2026-10-06. The exact command was:

```powershell
cargo run --manifest-path spikes/m4a-slint/Cargo.toml --release -- --duration-seconds=60
```

Console output:

```text
spike_started target_seconds=60 video_period_us=33333 telemetry_period_ms=250 resolution=1920x1080 nv12_bytes_per_frame=3110400
spike_finished duration_seconds=60.080
video_gpu_submitted_frames=1760 wall_clock_submissions_hz=29.29 active_interval_hz=29.70 intervals_ms=[min 31.84 / avg 33.67 / p95 34.68 / max 60.23 ms] cpu_upload_and_submit_ms=[min 0.55 / avg 0.84 / p95 0.98 / max 2.03 ms]
slint_before_render_callbacks=3702 average_render_callbacks_hz=61.62 intervals_ms=[min 5.53 / avg 16.06 / p95 28.89 / max 215.28 ms]
telemetry_updates=236 average_updates_hz=3.93 target_hz=4
presentation_note=BeforeRendering callbacks are a framework render proxy, not confirmed physical monitor presents; human visual stutter confirmation is still required
capture_note=no desktop capture or screenshots were performed
```

Submissions divided by full process runtime averaged 29.29 Hz and includes startup before the first frame. The active interval mean was 33.67 ms and the reported active rate was 29.70 Hz, so strict 30 fps was not established. The p95 interval was 34.68 ms and maximum was 60.23 ms. The telemetry counter averaged 3.93 Hz. Render callbacks averaged 61.62 Hz, but that is a framework callback proxy, not a physical present count. CPU upload, test-pattern refresh, and command submission averaged 0.84 ms; GPU completion time was not measured. These numbers do not establish physical presentation or absence of visible stutter.

## Dependency and license note

This spike pins the Slint 1.18 minor line and enables `unstable-wgpu-30`, which Slint documents as outside its normal API stability guarantee. The Slint framework is triple-licensed: GPLv3, royalty-free, or commercial. Its royalty-free license permits proprietary desktop applications at no cost with attribution through an accessible `AboutSlint` widget or an attribution badge on a public webpage. Review the chosen license and attribution obligations before introducing Slint into the shipped dependency tree; this spike does not change root manifests or `deny.toml`.
