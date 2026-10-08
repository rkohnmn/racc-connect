# Goal M5a: Windows screen capture (`racc-capture`)

Save as `docs/goals/M5a.md`.

**Run after:** M3a complete. Does not depend on the UI goals.
**Revisit before running:** none.

## Human checklist
1. Working tree clean on `main`.
2. Keep the PC unlocked and awake during the run. The agent will capture its own displays for a few seconds at a time; close or hide anything private that is open.
3. Afterwards run the PC #2 checks the goal adds to `docs/HARDWARE.md` (AMD GPU machine) and paste results back.

## Launcher
```
/goal Complete the work specified in docs/goals/M5a.md. Read AGENTS.md, docs/goals/COMMON.md, docs/DEV_SETUP.md and docs/PROGRESS.md first. Done only when every acceptance check in docs/goals/M5a.md passes with evidence, then stop. Do not start the next goal.
```

## Prompt
~~~text
GOAL: M5a. Implement Windows screen capture in crates/capture (package racc-capture): a platform-neutral Capture trait with a fake backend, and a Windows backend using DXGI Desktop Duplication that keeps frames GPU-resident, enumerates displays into racc-topology types, extracts cursor metadata, handles access-lost and device-lost recovery, and can migrate between displays without ending the capture thread. Verify it on this PC's real displays.

COMMON.md applies in full. Read AGENTS.md (sections 2.2, 2.3, 7, 8), docs/TOPOLOGY.md, docs/SESSION.md and docs/HARDWARE.md first.

PREFLIGHT
Clean tree; check-all green; M3a complete. Run the read-only probe and record, per display on this PC: which DXGI adapter owns it, adapter name, resolution, refresh, DPI scale. The laptop has an integrated AMD GPU and an NVIDIA GPU; record which adapter drives each of the three displays. This determines which encoder paths are available in M5b (capture and encode should share an adapter; cross-adapter use costs a GPU-to-GPU copy).

DEPENDENCIES AND UNSAFE
Add the windows crate (Microsoft's official bindings, MIT/Apache) limited to the features needed, only inside racc-capture's windows module. unsafe is allowed only in crates/capture/src/windows/ with SAFETY comments and RAII wrappers for every COM object and handle. The trait, the fake backend and all pure logic stay unsafe-free and compile on all targets. Check cargo deny after adding.

DESIGN
1. Trait and types (platform-neutral, tested with the fake): CaptureBackend with enumerate_displays() -> Vec<CapturedDisplay> (racc-topology Display plus a backend-private handle id and adapter id), start(display_id, params), migrate(display_id), stop(), and an event stream (bounded channel or callback) of CaptureEvent: Frame(GpuFrame), CursorShape(...), CursorMoved(...), DisplayLost, AccessLost(kind), DeviceLost, Recovered, Error(kind). GpuFrame is an opaque handle carrying: the platform texture (Windows: a shared-or-device-local ID3D11Texture2D reference owned via RAII), width, height, pixel format, a monotonic capture timestamp in microseconds, a damage summary (full, rect count, or none), and the display id. Frames come from a small pool (3 textures); a frame returns to the pool when dropped. The capture thread never blocks on the consumer: if the consumer holds all pool frames, drop the newest and count it (latest-wins).
2. Windows backend: enumerate adapters and outputs (IDXGIFactory1, IDXGIAdapter1::EnumOutputs). For each output, resolution and origin from the desktop coordinates, refresh rate and scale from the display configuration APIs (QueryDisplayConfig and DisplayConfigGetDeviceInfo) and per-monitor DPI APIs. The process must be per-monitor-DPI-aware v2 (embed a manifest or set at startup; document which). Display identity for racc-topology::DisplayIdentity: use the monitor's EDID manufacturer, product code and serial where readable (EDID is available from the registry device parameters or the display config path) and the monitor device path as the connector string; if EDID cannot be read use the device path alone and record the limitation.
3. Capture loop: per-adapter D3D11 device; IDXGIOutput5::DuplicateOutput1 (fall back to DuplicateOutput if needed; record which was used); AcquireNextFrame with a short timeout; WAIT_TIMEOUT means no change and produces no frame (damage-driven). On a new frame copy the duplication surface into a pooled default-usage GPU texture (one GPU copy, no CPU access; CPUAccessFlags = 0), release the duplication frame immediately, and emit the GpuFrame with dirty and move rect counts from frame metadata. Extract pointer position, visibility and shape (monochrome, color, masked color: convert shapes to 32-bit BGRA with correct hotspot; document the conversion) and emit them as separate events; never composite the cursor into frames.
4. Error handling and recovery state machine (pure logic, unit-tested with fake error injection, then exercised for real where possible): DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_DEVICE_REMOVED/RESET, DXGI_ERROR_WAIT_TIMEOUT, DXGI_ERROR_UNSUPPORTED, DXGI_ERROR_SESSION_DISCONNECTED, E_ACCESSDENIED, mode changes, display removed. Map each to a CaptureEvent and a recovery action with the backoff list from AGENTS/M3b (50, 100, 200, 400, 800, 1000 ms). Protected content and the secure desktop produce black or errors: report them as AccessLost(SecureDesktop or Protected) rather than crashing. Document that the secure desktop needs a SYSTEM helper on the input desktop (M6) and is NOT handled here.
5. Migration: migrate(display) releases the current duplication and attaches to the new output (possibly on a different adapter, which re-creates the device) without terminating the capture thread; measure the time it takes.
6. WGC fallback: NOT implemented in this goal. Add a stub with a documented trait slot and an open question noting when it would be needed (for example if DDA fails on a hybrid-GPU configuration).

TEST TOOL
Add a small dev binary in racc-testkit or crates/capture/examples named capture-probe: lists displays and adapters; captures N frames from display k and prints per-frame timing (acquire wait, copy time, interval), damage stats, cursor events and drop counts; optionally writes ONE frame as an uncompressed BMP or PPM (hand-written writer, no new dependency) via a test-only staging readback to verify the content is not blank and the orientation and colors are right (compute a checksum and basic statistics; do not commit the file; add the output folder to .gitignore; delete captures afterwards). Make the probe able to run each of this PC's three displays.

REAL-HARDWARE VERIFICATION ON THIS PC (record real numbers; label VERIFIED-RUN with date and machine)
- Static screen: the number of frames delivered in 10 seconds with no activity (expect near zero if damage-driven) and with the mouse moving.
- Moving content: achieved frame rate on each display for 10 seconds; per-frame acquire-to-ready time p50 and p95.
- Copy cost per frame at 1920x1080.
- Cross-adapter behavior: capture each of the three displays and note which required a different adapter.
- Migration time between two displays.
- Cursor shape extraction: check a standard arrow, an I-beam and a busy cursor produce non-empty BGRA shapes with plausible hotspots (do not log pixels).
- Process private memory while capturing 1080p30.
Never capture the lock screen or elevate; those are HUMAN-PENDING.

DOCUMENTATION
docs/CAPTURE.md: trait, pooling and latest-wins policy, Windows backend details, error mapping table, adapter findings for this PC, measurements, limits (secure desktop, protected content, WGC fallback pending). ADRs: capture trait design and pooling; identity derivation for displays; DDA over WGC as primary (already in AGENTS.md; record the actual findings). Update PROGRESS.md, OPEN_QUESTIONS.md. Add human checks to docs/HARDWARE.md (HUMAN-PENDING): run capture-probe on PC #2 (Radeon RX 7900 GRE) and paste its output with the display list and adapter info (redact serials before pasting anywhere public); capture during a display mode change; capture while a UAC prompt appears (expect an AccessLost report) and while the PC is locked (expect AccessLost or black); unplug and replug a monitor if possible.

HARD CONSTRAINTS
Do not implement encoding, networking, services or the secure desktop path. No new unsafe outside the windows module. Never commit captured images. Do not start the next goal.

ACCEPTANCE CHECKS
B1 to B8, plus:
C1. capture-probe runs on this PC against every display and prints the data above (VERIFIED-RUN, redacted summary in docs).
C2. Frames are GPU-resident: show that the pool textures have no CPU access flags and that only the test readback path maps memory.
C3. Fake-backend tests cover the recovery state machine for every error mapping; the trait, pool and latest-wins behavior are tested without a GPU.
C4. Measurements recorded with method; adapter-per-display table recorded.
C5. racc-capture builds for the macOS target with the Windows module compiled out; no windows crate in the macOS dependency tree.
C6. docs/CAPTURE.md, ADRs and HARDWARE.md human checks exist.

FINAL REPORT: COMMON.md section 14, plus the adapter-per-display table and the capture measurements.
~~~
