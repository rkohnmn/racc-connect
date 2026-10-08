# M5b: Windows H.264 encoding and file probe

Implement the Windows encoding and encoded-file probe portion of M5, using the encoder requirements in `AGENTS.md` section 2.3 and the M5 acceptance criteria. Keep capture implementation in its separately scoped M5 prompt.

## Preflight

- Read `AGENTS.md` sections 2.3 and M5. Inspect the existing encoder trait, fake backends, feature gates, and related M5 prompts before editing.
- Check whether the Windows MSVC target and required Windows SDK/toolchain are available. Record unavailable platform verification honestly; a non-Windows build does not prove Windows Media Foundation works.
- Check licenses for encoder and platform-binding dependencies before adding them. Do not add or link x264. Use BSD-licensed OpenH264 for the software fallback.
- Confirm the probe output is an Annex B H.264 elementary stream in a `.h264` file, suitable for inspection in a standard player; do not substitute another codec or container.

## Implementation

- Implement the Windows Media Foundation H.264 encoder path with D3D11 texture input. Configure it for low latency: no B-frames, no lookahead, a small rate-control window, and low-latency mode through codec API properties where available.
- Keep the encoder behind the existing `Encoder` trait and platform feature gates. Preserve the fake encoder path for portable logic checks.
- Provide BSD-licensed OpenH264 as the software fallback behind the same encoder interface. Make fallback selection explicit; report which backend actually encoded the output.
- Provide an encode-to-file probe path using the existing M5 capture/probe scaffolding. It must write Annex B bytes to a `.h264` file. Keep this prompt scoped to encoding and the file probe; do not add networking, viewer, or UI behavior.

## Tests and checks

- Add or extend fake-backend/unit tests for the configured encoder parameters, frame-to-bitstream flow, file output, and fallback behavior that can be checked without Windows hardware.
- Check that software output is Annex B and that the probe writes the stream without a container wrapper. Tests must not claim to validate a real GPU encoder.
- Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`.
- Run the repository's shipped-feature and dependency-license checks. Run `cargo check --target x86_64-pc-windows-msvc` when the target/toolchain is available. Report exact commands and outcomes. Mark Windows platform behavior unverified if its target or SDK is unavailable; compilation alone does not establish hardware behavior.

## Documentation and evidence

- Add `docs/ENCODE.md` describing the Media Foundation and OpenH264 paths, low-latency configuration, fallback selection, feature gates, and how to create and inspect the Annex B `.h264` probe output. Separate automated evidence from human-only evidence.
- Add an ADR under `docs/decisions/` when this work introduces a significant encoder/platform-binding dependency or requires a significant implementation choice. Record the choice and license evidence without inventing measurements.
- Update `docs/PROGRESS.md` for M5b with completed work, exact verification results, platform limitations, and human-pending items. Keep M5 incomplete until human checks pass.
- Record this checklist in `docs/HARDWARE.md` for a human with the Windows host and GPU. Do not mark these items complete on the agent's behalf:
  - [ ] Identify the GPU and the Media Foundation H.264 hardware path available (NVENC, Intel QSV, AMD AMF, or unavailable); record which backend actually ran.
  - [ ] Run the encode-to-file probe from real capture and confirm the `.h264` file plays correctly in a standard player.
  - [ ] Change the display mode while capture/encoding is active; confirm capture recovers and a subsequent `.h264` output plays correctly.
  - [ ] If hardware encoding is unavailable or fails, confirm OpenH264 fallback produces a playable `.h264` file and record that software encoding was used.

## Acceptance evidence

M5b is complete only when the Windows Media Foundation H.264 path uses D3D11 texture input and the specified low-latency settings, the BSD OpenH264 fallback is available, the Annex B `.h264` probe/output path exists, fake-backend tests and standard workspace checks pass, feature and license checks are recorded, documentation and the hardware checklist are updated, and the Windows cross-target check either passes or is explicitly marked unverified with the reason.

The human-only real-GPU playback and display-mode recovery checks remain pending until a human reports results. Do not claim M5 complete based on compilation, fake tests, or a probe run without real hardware.
