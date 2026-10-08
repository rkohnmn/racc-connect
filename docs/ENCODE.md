# Windows H.264 encode and probe

This crate provides H.264 encoding behind racc_encode::Encoder, plus synthetic and opt-in visible-capture file probes. On 2026-10-07, a synthetic-only 1280x720 test on Windows PC #1 encoded 90/90 frames with WindowsMediaFoundationHardware and the Annex B file decoded successfully. This does not test real desktop capture, adapter affinity, or pacing.

## Input paths

### Windows hardware path

`MediaFoundationH264Encoder` enumerates hardware Media Foundation H.264 transforms and tries compatible transforms. Its device manager is initialized with the D3D11 device that owns the input texture. The capture path obtains the owner from `racc_capture::GpuFrame`, creates a same-device D3D11 video processor, and converts/scales BGRA to an NV12 texture. A fixed three-texture pool bounds in-flight surfaces; leases remain retained until corresponding output timestamps are emitted. Pixel conversion stays on the GPU. A device mismatch returns `EncodeError::CrossDevice`.

The transform receives progressive 30 fps NV12 and emits Main-profile H.264. B-picture count must be explicitly set to zero or that MFT is rejected. The code attempts `CODECAPI_AVLowLatencyMode`, real-time mode, and a 250 ms rate-control buffer when the MFT exposes those properties. The buffer size is expressed in bytes: `bitrate_bps * 250 / 8000`. `AppliedCodecSettings` reports which optional settings were accepted.

There is no separate portable lookahead property used by this implementation. The code does not claim that it independently disabled a vendor-specific lookahead control; MFT-specific controls may differ. The required B-picture setting and optional low-latency property are recorded separately.

The force-keyframe hook writes `CODECAPI_AVEncVideoForceKeyFrame` as a 32-bit unsigned value of 1. It requests a keyframe for the next `ProcessInput` only. Output metadata marks a keyframe only when the normalized Annex B bitstream contains a slice NAL with type 5. The MFT clean-point flag alone is ignored, and the first output is not assumed to be an IDR.

### OpenH264 software path

OpenH264 is configured for real-time screen-content encoding, Main profile and a 30-frame intra period. It consumes validated I420 planes directly. If captured BGRA cannot use a compatible Media Foundation transform, the capture path keeps color conversion on the GPU: BGRA is scaled to a pooled NV12 texture, copied to one reusable same-device D3D11 staging surface, and mapped into fixed Y/U/V buffers for OpenH264. The readback copies plane bytes and separates interleaved UV; it does not perform CPU color conversion. The single staging surface and fixed buffers are reused, and each frame is encoded synchronously while the buffers are borrowed. A device, format, pitch, mapping, or bounds error fails the probe instead of silently changing behavior.

`select_encoder` reports the backend that was actually selected, whether fallback occurred, and the fallback reason. The capture-to-file probe carries those fields in `ProbeReport`; the synthetic MF-preferred probe also reports OpenH264 if it falls back. Callers that already have normalized I420 can select OpenH264 directly. The OpenH264 adapter exposes an immediate IDR request through its public `force_intra_frame` API, and its output metadata is derived from NAL type 5; the periodic intra-frame interval remains a backstop.

No x264 code or dependency is included.

## Probe files

Both probes write raw Annex B access units directly into a `.h264` elementary-stream file, without a container. Output is bounded to 512 MiB. The synthetic probe never acquires desktop frames and supports OpenH264 on all targets and Media Foundation on Windows:

```powershell
cargo run -p racc-encode --example encode_probe -- --backend openh264 --output .\synthetic-openh264.h264 --width 1280 --height 720 --frames 90 --bitrate 4000000
cargo run -p racc-encode --example encode_probe -- --backend mf --output .\synthetic-mf.h264 --width 1280 --height 720 --frames 90 --bitrate 4000000
```

The visible-capture probe is a human-only Windows command. It requires an explicit display ID and deliberate acknowledgement, and is bounded to 1,800 frames or 60 seconds, whichever limit is reached first. It reads the selected visible display into GPU memory and writes a video that may contain private screen content. Do not run it while sensitive content is visible, on the lock screen, or during a UAC secure-desktop prompt. The agent did not invoke this command.

```powershell
cargo run -p racc-encode --example capture_encode_probe -- --acknowledge-visible-screen --display <display-id> --output .\capture.h264 --width 1280 --height 720 --frames 90 --bitrate 4000000
```

First list display metadata with `cargo run -p racc-capture --example capture_probe -- --list`, then use a listed display ID. The command fails if the display is missing, capture access is lost, or the hardware encoder/converter is unavailable. The probe does not restart capture after a mode change; M5a's recovery path must be tested separately, then a new file can be recorded. Inspect a completed file with `ffprobe .\capture.h264` and play it with `ffplay -f h264 .\capture.h264` or another standard H.264 player.

## Verification evidence

- **VERIFIED-RUN — SYNTHETIC ONLY:** cargo run --offline -p racc-encode --example encode_probe -- --backend mf --output target/m5b-mf-synthetic-90.h264 --width 1280 --height 720 --frames 90 --bitrate 4000000 reported 90/90 frames from WindowsMediaFoundationHardware. ffprobe reported H.264 Main, 1280x720, 90 frames, and the frame-type scan found 0 B-frames; ffmpeg decoded the complete file without errors. The raw elementary stream has no presentation timestamps, so this is not a 30 fps cadence measurement.
- **TESTED-FAKE:** encoder input validation, bounded GPU conversion plan and pool capacity, hardware-to-OpenH264 fallback selection, NV12 plane layout and bounds, Annex B normalization and bounded file output.

- **COMPILE-ONLY:** cargo check --offline -p racc-encode --target x86_64-pc-windows-msvc passed. **VERIFIED-RUN:** the synthetic-only Media Foundation probe is recorded above; this does not establish desktop capture or adapter affinity.
- **UNVERIFIED:** macOS cross-check could not build OpenH264 because this environment lacks a target C++ compiler (`c++`).
- **HUMAN-PENDING:** identify the actual NVIDIA/Intel/AMD MFT on Windows PC #1, record which one ran, play a file from real captured frames, test capture recovery after a display mode change and verify a subsequent file, and exercise the software fallback if hardware encoding is unavailable. See `docs/HARDWARE.md` for the owner checklist.
