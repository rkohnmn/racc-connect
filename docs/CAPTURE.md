# Windows capture foundation (M5a)

## Status

The crate now exposes a platform-neutral `CaptureBackend` contract, a deterministic fake, a bounded latest-wins texture pool, and a pure capture-recovery mapping. The Windows-gated backend uses DXGI Desktop Duplication and D3D11. The API is inert at construction; output enumeration is explicit, and desktop acquisition begins only after the caller explicitly calls `start` and `poll_event`.

- **[TESTED-FAKE]** Fifteen capture-crate tests cover pool exhaustion/reuse, bounded fake migration, aspect-preserving output bounds, recovery/backoff, HRESULT classification, negative-origin cursor mapping, and monochrome cursor conversion.
- **[COMPILE-ONLY]** Windows `x86_64-pc-windows-msvc` and Intel macOS `x86_64-apple-darwin` capture-crate checks pass. The macOS dependency tree contains `racc-capture`, `racc-topology`, and `racc-proto`; it does not contain the `windows` crate.
- **[VERIFIED-RUN]** On 2026-10-07, the redacted `capture_probe --list` ran on Windows PC #1 and enumerated adapter, geometry, active refresh, scale, and identity-source metadata only. It did not call capture `start`/`poll_event` or access pixels.
- **[HUMAN-PENDING]** Capture frames, cursor pixels, timing, drops, migration, and GPU copy completion were not measured because the visible screens are unavailable for capture review.

## API and ownership

`CaptureBackend` enumerates `CapturedDisplay` values and provides `start`, `migrate`, `stop`, and one-event `poll_event` operations. `GpuFrame` contains dimensions, BGRA format, display id, a monotonic session-relative timestamp, damage summary, acquire wait, CPU command-submission duration for copy/scaling, interval since the prior emitted frame, and cumulative texture-pool frame drops. The copy/scaling metric measures CPU time submitting the GPU command; it does not wait for GPU completion. Fakes report zero acquire and submission times and deterministic intervals. Pixel data stays in an opaque GPU resource; it does not pass through a UI/core event bus.

`TexturePool` owns a fixed set of preallocated texture resources. A frame lease returns its slot when the last owner drops the frame. Pool exhaustion is nonblocking: the newest frame is dropped and counted. The Windows backend allocates three D3D11 `D3D11_USAGE_DEFAULT` output textures with `CPUAccessFlags = 0`. Requested `CaptureParams` limits are always clamped to the product ceiling of 1920x1080, even if a caller asks for larger dimensions. When the source exceeds the effective limit, the backend uses an aspect-preserving D3D11 video processor pass to scale into bounded output textures; `CopyResource` is used only when source and output dimensions match. It does not map a streaming texture or create a CPU staging texture. `GetFrameDirtyRects` and `GetFrameMoveRects` use fixed buffers capped at 4,096 entries each; overflow is represented as full-frame damage. Cursor input is separate from video. Cursor source data is capped at 1 MiB.

The capture loop is owned by the caller. Each Windows `poll_event` waits at most the minimum of its requested timeout, the session timeout, and 16 ms. A static desktop's `DXGI_ERROR_WAIT_TIMEOUT` produces no frame. `migrate` releases the old duplication object and creates a D3D device/duplication object for the new output on the same caller-owned loop; this foundation does not create or terminate a thread.

## Windows backend

Output enumeration walks at most 16 DXGI adapters and 32 outputs per adapter. It records adapter LUID/name, virtual-desktop origin and size, primary status, effective DPI scale, and the active refresh rational from QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS). The source GDI name from DisplayConfig is matched to the DXGI output, then the target monitorDevicePath is used as the stable connector identity. The backend best-effort reads the matching monitor's bounded EDID base block from its read-only registry Device Parameters key and validates its header and checksum before hashing manufacturer, product code, serial, and connector together. If EDID cannot be read or validated, it hashes the connector path alone and records ConnectorPathOnly. The display label uses the monitor friendly name when supplied. DuplicateOutput1 is tried first with BGRA; DuplicateOutput is the compatibility fallback. The backend-local GDI name is retained for capture lookup.

DPI-awareness v2 can be requested with `set_process_dpi_awareness_v2`; the app/helper should call it before creating windows. Enumeration also makes a best-effort late request. Windows may reject a late process-awareness change, so startup ordering must be handled by the eventual host binary.
### Redacted adapter mapping — Windows PC #1, 2026-10-07

The metadata-only `cargo run --offline -p racc-capture --example capture_probe -- --list` reported all three outputs on the integrated AMD Radeon(TM) Graphics adapter. The RTX 3050 Ti is present in this PC's adapter inventory, but it does not drive these three active displays according to this output enumeration. Display IDs, monitor names, connector paths, and adapter LUIDs are intentionally omitted.

| Redacted output | Desktop origin | Size | Active refresh | Scale | Adapter | Identity source |
|---|---:|---:|---:|---:|---|---|
| A | (0, 0) | 1920×1080 | 240,000 mHz | 1000 | AMD Radeon(TM) Graphics | Connector path |
| B | (-1920, 4) | 1920×1080 | 60,000 mHz | 1000 | AMD Radeon(TM) Graphics | Connector path |
| C | (-984, -1080) | 1920×1080 | 60,000 mHz | 1000 | AMD Radeon(TM) Graphics | Connector path |

These are **VERIFIED-RUN metadata observations**, not capture or GPU-performance measurements. Screen contents were never acquired.

**Metadata limitations:** On the 2026-10-07 PC #1 metadata-only run, all three enumerated outputs reported ConnectorPathOnly; the probe did not report serials or raw monitor paths. The registry lookup is implemented but did not provide a valid EDID base block for those outputs in that run. If active DisplayConfig lookup or the target device path is unavailable, identity falls back to the DXGI GDI device name and is marked GdiDeviceNameFallback; GDI names can change after topology or driver changes. If the active refresh rational is unavailable, refresh is 0 (unknown). The active mode is read at enumeration time and must be refreshed after topology changes. The PC #1 metadata-only probe verified which adapter owned the three enumerated outputs for that run; it did not create capture devices or establish capture/encoder affinity.

## Recovery mapping

| Condition | Event | Action |
|---|---|---|
| `DXGI_ERROR_WAIT_TIMEOUT` | None | No frame; no retry consumed |
| `DXGI_ERROR_ACCESS_LOST` | `AccessLost(DxgiAccessLost)` | Re-enumerate and retry with 50, 100, 200, 400, 800, then 1,000 ms backoff |
| Mode/topology change | `AccessLost(ModeChanged)` | Re-enumerate and retry with the same bounded backoff |
| Device removed/reset | `DeviceLost` | Recreate device/duplication with bounded backoff |
| Session disconnected | `AccessLost(SessionDisconnected)` | Re-enumerate and retry with bounded backoff |
| `E_ACCESSDENIED` | `AccessLost(AccessDenied)` | Retry with bounded backoff; caller reports access loss |
| Protected content masked | `AccessLost(ProtectedContent)` | Do not emit a frame; caller reports access loss |
| Unsupported | `Error(Unsupported)` | Stop this backend path |
| Display removed | `DisplayLost` | Re-enumerate and await a valid display |
| Other OS/driver error | `Error(DriverFailure)` | Retry with bounded backoff |

The pure mapping and retry ladder are tested without a GPU. The Windows retry path refreshes output metadata before recreating the duplication session. This does not verify driver recovery behavior.

## Cursor and privacy limits

Cursor shape, hotspot, position, and visibility remain separate from video. DXGI pointer coordinates are relative to the adapter output, so the backend adds the output's virtual-desktop origin with saturating arithmetic; negative-origin mapping has a pure test. Color shapes are copied as BGRA. Masked-color shapes retain their Windows blend mode and alpha/mask byte. Monochrome AND/XOR planes are expanded into BGRA channels while preserving the AND/XOR semantics for the eventual compositor; the hotspot is clamped to the returned shape bounds. Arrow, I-beam, busy cursor, and real hotspot checks remain human work.

The backend is not a secure-desktop solution. `E_ACCESSDENIED` is a generic access-denied HRESULT: it does not identify whether the cause is the secure/UAC desktop, session isolation, policy, or another permission failure, so the event is reported as generic `AccessDenied`. DXGI's `ProtectedContentMaskedOut` flag is a separate signal and is reported as `ProtectedContent`; it does not classify secure-desktop access. A black or blank frame cannot reliably be distinguished from an ordinary black desktop by this backend. As required by the process model, a SYSTEM helper on the input desktop belongs to M6 and is not implemented here. The Windows.Graphics.Capture fallback is only a documented slot; it is not implemented and also cannot capture the secure desktop.

The 2026-10-07 PC #1 run used only the redacted `--list` metadata path. It did not start capture, acquire desktop frames, inspect cursor pixels, write screenshots, elevate, or access a lock/UAC screen. Capture requires explicit calls from a host process; the crate does not auto-start it. No captured image files are produced by this foundation.

## Hardware data and remaining verification

| Item | Result |
|---|---|
| Per-display adapter model, mode, active refresh, and scale | [VERIFIED-RUN] PC #1 metadata-only mapping above; PC #2 remains HUMAN-PENDING |
| Static-screen frame count and mouse-motion rate | HUMAN-PENDING; no real capture measurements were run |
| Moving-content rate and acquire-to-ready p50/p95 per display | HUMAN-PENDING; no real capture measurements were run |
| 1080p copy/scaling cost and private memory | HUMAN-PENDING; no real capture measurements were run |
| Cross-adapter capture and migration time | HUMAN-PENDING; no real capture measurements were run |
| Real arrow/I-beam/busy shape and hotspot | HUMAN-PENDING |
| PC #2 (Radeon RX 7900 GRE) capture run | HUMAN-PENDING |
| Display mode change, UAC, lock, unplug/replug behavior | HUMAN-PENDING |
| Probe utility | Supplied at `crates/capture/examples/capture_probe.rs`; `--list` ran on PC #1 and enumerates redacted metadata only. `--capture` is capped at 300 frames; `--sample` runs for 1 to 60 wall-clock seconds and can optionally time a post-sample display migration. Both capture modes require explicit visible-screen acknowledgement. Neither capture mode was run. |

The probe's `--list` table columns are display ID, adapter model, virtual-desktop origin, size, active refresh in mHz, `scale_milli`, and identity source. It intentionally omits monitor/display names, connector paths, and adapter LUIDs. Both capture modes print cursor-shape metadata rows with shape width, height, hotspot x/y, BGRA byte length, and blend mode; they never print BGRA pixel bytes. The N-frame `--capture` mode prints one row per delivered frame with frame number, monotonic session-relative timestamp, acquire wait, CPU copy/scaling command-submission duration, frame interval, damage summary, cumulative cursor-shape and cursor-position event counts, and cumulative dropped frames. The fixed-wall-time `--sample DISPLAY_ID --seconds N --acknowledge-visible-screen` mode accepts 1 to 60 seconds and prints aggregate frame/cursor/drop counts plus nearest-rank p50/p95 for acquire wait, copy/scaling submission duration, and frame interval. Percentile samples are capped at 120,000 values per metric; the summary says when that cap is reached. A static desktop can report zero frames and `NA` percentiles. Add `--migrate-to DISPLAY_ID` to time one migration after sampling. Migration duration includes the synchronous backend migration call. These are probe measurements only; every real capture measurement remains HUMAN-PENDING until run on approved hardware. The probe does not read pixels back to CPU or write image files. Its capture mode still acquires visible desktop frames into GPU memory, so only run it when the visible-screen privacy checklist is cleared; never capture a lock or UAC screen.

## macOS capture backend (M9) — COMPILE-ONLY

The macOS backend is a separate native path; it does not produce the Windows `GpuFrame`, which wraps D3D resources. A caller explicitly starts `MacCaptureBackend` for a `MacDisplay`; construction and enumeration do not start a stream or read pixels. The backend checks Screen Recording permission without prompting and returns a typed `PermissionMissing` error when access is absent.

- ScreenCaptureKit is primary. It selects the requested display, caps output at 1920×1080, defaults to 1280×720, fixes the rate at 30 fps, requests 420v video-range NV12, disables audio capture, includes the cursor in video as required by M9, and bounds queued frames to three.
- If ScreenCaptureKit setup fails, the backend tries deprecated CGDisplayStream. The fallback requests the same bounded 420v output and includes the cursor in captured video. Its IOSurface is wrapped into a retained `CVPixelBuffer` for the native encoder boundary.
- `MacCapturedFrame` owns a retained CoreVideo pixel buffer and timestamp. The latest frame replaces any older unconsumed frame. It is not copied into the generic D3D `GpuFrame` contract.
- Lifecycle notices cover start path, explicit display migration/reconfiguration, stream access loss, a recovered frame, and display removal in the fakeable controller. The real adapter does not yet watch system display topology or automatically restart after sleep/wake/access loss; the caller must handle the notices and request migration/restart.

**Verification:** `cargo check --offline -p racc-capture --target x86_64-apple-darwin` and strict target Clippy passed on the Windows coding host. `cargo test --offline -p racc-capture` passed 16 tests, including the fake lifecycle test; those tests do not run Apple frameworks. These are **COMPILE-ONLY / TESTED-FAKE**, not Mac runtime verification. No Apple linker, Screen Recording prompt, or screen content was accessed. The target-specific objc2 dependencies are absent from the Windows capture dependency tree.

**Integration limits:** `MacCaptureBackend` is wired into the foreground host and VideoToolbox encoder source. The app has a Screen Recording status panel and explicit Settings link; native behavior is unverified. M9 captures the cursor in video and sends hidden-cursor metadata to prevent a duplicate viewer overlay. The full Apple host target check fails before host-agent Rust compilation because OpenH264's target C++ compiler is unavailable. Capture output, permission links, fallback availability, FPS, latency, thermals, and memory remain HUMAN-PENDING on the 2015 Mac. See `docs/MACOS.md` and the M9 checklist in `docs/HARDWARE.md`.
