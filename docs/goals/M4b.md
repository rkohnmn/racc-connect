# Goal M4b: UI shell on fake data (`racc-app`, `racc-core` bus types)

Save as `docs/goals/M4b.md`.

**Run after:** M3b and M4a complete.
**Revisit before running:** read `docs/decisions/0001-ui-toolkit.md`. If the chosen toolkit has constraints (for example needing raw OS input for physical keys), add them to the "Toolkit constraints" line in the prompt.

## Human checklist
1. Working tree clean on `main`.
2. After the run, launch the app yourself (`cargo run -p racc-app -- --fake`) and judge the look, hover states and smoothness against the style description. Tell me what to change; visual polish takes iteration.

## Launcher
```
/goal Complete the work specified in docs/goals/M4b.md. Read AGENTS.md, docs/goals/COMMON.md, docs/DEV_SETUP.md and docs/PROGRESS.md first. Done only when every acceptance check in docs/goals/M4b.md passes with evidence, then stop. Do not start the next goal.
```

## Prompt
~~~text
GOAL: M4b. Build the complete Discord-style UI shell in crates/app (package racc-app) driven entirely by a fake core, plus the platform-neutral UI-to-core message bus types in crates/core (package racc-core). No real networking, capture, decode or Tailscale. The app must run with `--fake` and show believable devices, displays, a synthetic live video stream, telemetry and events.

TOOLKIT CONSTRAINTS: ADR 0001 selects iced 0.14.0 with its native wgpu renderer. Keep the M4a caveat: its synthetic NV12 source and upload intervals are not physical monitor-presentation measurements. Render video in an iced::widget::shader custom Primitive in the same native wgpu context. The view reads the latest frame through the separate FrameSource/FrameSink handoff; frames and pixels must never enter CoreEvent, UiCommand, CoreSnapshot, or the UI event bus. Use the iced timer subscription for 4 Hz metadata refresh, independent from the 30 Hz frame source. Do not block frame presentation for telemetry work. Use no webview or overlapping native video window. M4b has only a synthetic pattern source, so do not describe it as a decoded NV12 upload path. The current custom focus controls provide Tab/Shift+Tab traversal and Enter/Space activation, but explicit accessible names and roles are not implemented; record that accessibility limitation.

COMMON.md applies in full. Read AGENTS.md (sections 5, 6, 7), docs/PROJECT_SCOPE.md section 5, docs/decisions/0001-ui-toolkit.md, docs/SESSION.md, docs/TOPOLOGY.md and docs/TELEMETRY.md first.

PREFLIGHT
Clean tree; check-all green; M3b and M4a complete. The toolkit named in ADR 0001 is the one to use; do not revisit that decision.

ARCHITECTURE
1. racc-core (bus types only in this goal): CoreEvent enum (at minimum the names in AGENTS.md section 6: DeviceDiscovered, DeviceOffline, TopologyChanged, DisplaySelected, StreamStarted, StreamReset, DecoderReady, ConnectionStatsUpdated, InputCaptureChanged, SessionEnded, plus PendingAuthorization, ClipboardStatus, Notification), UiCommand enum (SelectDevice, SelectDisplay, SetQuality, SetVisible, ToggleKeyboardCapture, ToggleMouseCapture, Disconnect, Connect, ApprovePeer, RejectPeer, SetHosting), and trait CoreHandle (send(UiCommand), poll_events(), snapshot access for telemetry). Video frames NEVER travel on this bus: define a separate FrameSink/FrameSource handoff trait that the render surface owns, and document the rule.
   racc-core depends on racc-proto, racc-topology, racc-telemetry, racc-session (paths). It must build and test with no UI crate. Keep it free of UI toolkit types.
2. A FakeCore in racc-testkit (or a fake module in racc-core behind a feature) implementing CoreHandle with a scripted scenario: three devices (two Windows, one Mac; one offline), each with 1 to 3 displays with varied resolution, refresh and scale; a synthetic video source (moving test pattern with frame counter at 30 fps, switching pattern per display); telemetry values changing believably; periodic events (display switch, decoder reset, quality adjustment, packet loss event); a simulated display switch that follows the real hold-then-atomic-replace behavior with a 150 ms delay; and an authorization prompt scenario. Deterministic when seeded.
3. racc-app: a pure view-model layer (state reducer: (state, CoreEvent or UiCommand) -> new state plus commands) that contains all UI logic and is unit-tested without any window. Widgets only render view-model state. No business logic inside widget code.

UI TO BUILD (follow AGENTS.md section 5 exactly)
- Device rail (72 px): Home, device icons with online/offline state and the selected-device left indicator, Add/discover button. Tooltips with device names. Offline devices are visibly dimmed and not selectable for streaming.
- Device sidebar (240 px): device name header; STREAM section listing displays (name, resolution, refresh, scale, availability); CONTROL section (Remote Desktop, Clipboard); SYSTEM section (Performance, Connection, Settings). Selecting a display issues SelectDisplay; unavailable displays are disabled. Sections collapsible if cheap.
- Session workspace: header (Device / Display name; subtitle like "144 Hz display • streaming 720p30 • H.264 • 8 ms"; monitor selector, quality selector with 480p, 720p, 1080p, Auto; fullscreen; keyboard capture; disconnect), native video surface with letterbox using racc-topology, and an overlay layer for states: Connecting, Switching (holds last frame), Paused, Reconnecting, Error, "Waiting for approval".
- Telemetry sidebar (280 px, collapsible): SESSION, HOST and EVENTS sections with the exact fields from AGENTS.md section 5.1 (no GPU usage). Updates at about 4 Hz from snapshots without recreating the UI tree.
- Local session panel (bottom of the left column): local machine name, connection state, buttons: keyboard capture, mouse capture, audio (disabled placeholder with tooltip "not supported"), settings.
- Settings and Home views: minimal pages (hosting on/off, allowlist list with approve/remove, quality default, about). Keep them simple.
- Visual style: design tokens in one module (colors, radii 6 to 10 px, spacing, font sizes 14 to 16 px body and 18 to 22 px headers, border 1 px) following the palette in AGENTS.md section 5.2 (very dark charcoal rail, lighter secondary sidebar, dark gray main, lighter gray cards and selected rows, near-white primary text, muted gray secondary text, blue-purple accent). Hover, pressed, selected and focus states; smooth but minimal animation; custom scrollbars. No Discord name, logo, icons or assets anywhere; use original simple vector icons you draw or an openly licensed icon set recorded in an ADR.
- Window behavior: SetVisible(false) emitted on hide, minimize and (if supported) occlusion; SetVisible(true) on restore. Tray: define the interface (TrayController trait) and a no-op implementation; the real tray is M10.
- Input capture UI: keyboard capture and mouse capture toggles update view-model state and emit commands; the actual forwarding is M7. Esc-chord or a documented hotkey to release capture (decide and document; it must always be possible to release).
- Keyboard navigation and focus order for all interactive elements; accessible names where the toolkit supports them.
- App icon: placeholder only (do not make the final logo; M10).

PERFORMANCE RULES (measure and record)
Telemetry updates must not rebuild the whole tree or disturb video pacing. Record with the fake stream running: frame interval median, p95 and count over 40 ms for 60 seconds with the telemetry sidebar open and collapsed; CPU time per second; private memory idle and playing. Compare against the M4a spike numbers.

TESTS
1. View-model reducer: device selection, display selection, switch lifecycle (Switching overlay then replaced), offline devices, topology change removing the selected display, pause on hide, reconnect overlay, authorization prompt, quality change, capture toggles, command emission for each interaction.
2. Layout logic that is pure (sidebar collapse widths, minimum window size behavior, letterbox rect) tested without a window.
3. FakeCore determinism with a seed; event ordering rules.
4. Dependency rule check: racc-core and everything below it still build with no app crate; scripts/check-layering passes.
5. Smoke test: the app starts in --fake mode headlessly if the toolkit supports it, otherwise record that this is verified only by the human run.

DOCUMENTATION
docs/UI.md (regions, view-model design, message bus and frame handoff rule, tokens, interaction table, keyboard map, known limitations). Update ADR list with the icon-set decision if used. Update PROGRESS.md and OPEN_QUESTIONS.md. Add to docs/HARDWARE.md human visual review checklist: look and feel against the style description; hover and selected states; collapse and expand; window resize; drag between displays; keyboard navigation; telemetry readability; anything janky. Optionally commit 2 to 4 small screenshots of the fake UI under docs/img/ (synthetic content only).

HARD CONSTRAINTS
No real network, capture or decode code. Video never on the bus. No business logic in widgets. Dependencies limited to the chosen toolkit and its required crates plus anything justified in an ADR. Do not start the next goal.

ACCEPTANCE CHECKS
B1 to B8, plus:
I1. cargo run -p racc-app -- --fake launches and shows all regions (agent observes via logs or a screenshot it captures of its own window if tooling allows; otherwise HUMAN-PENDING).
I2. View-model and layout tests pass; racc-core builds and tests without racc-app.
I3. Measurements recorded with methods.
I4. No GPU usage field, no audio functionality, no Discord branding anywhere (repository search evidence).
I5. docs/UI.md written; human visual checklist added.

FINAL REPORT: COMMON.md section 14, plus the measurement table and a list of everything the human should look at first.
~~~

