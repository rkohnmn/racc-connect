# M4b UI shell

This document describes the fake-data desktop shell in crates/app, its metadata boundary with racc-core, and current verification limits. The implementation uses iced 0.14.0 with its native wgpu renderer. M4b has no real network, capture, decode, or Tailscale integration.

## Layout and interaction

The app has four regions:

| Region | Width | Contents |
|---|---:|---|
| Device rail | 72 logical px | Home, online/offline device entries, discovery entry, and Session navigation |
| Device sidebar | 240 px open / 48 px collapsed | Selected peer, display list, control and system actions, and local session controls (compact icon rail when collapsed) |
| Session workspace | Flexible, at least 360 px | Session header with monitor selector, quality/fullscreen/capture/disconnect controls, letterboxed shader surface, and state overlays |
| Telemetry sidebar | 280 px open / 48 px collapsed | Session and host stats, recent event log, and collapse control |

The window starts at 1440 × 900 logical pixels, with a 1050 × 640 minimum. Region sizing and letterbox geometry are pure functions in crates/app/src/design.rs. Its tokens module defines the 72/240/280 px open widths, 48 px collapsed widths, 360 px minimum workspace, 1050 × 640 minimum window, 4/8/12/16 px spacing, 6/8/10 px corner radii, 1 px borders, 2 px focus rings, 14 px body text, 12 px section labels, and 20 px headers. It also centralizes the dark rail, secondary sidebar, main panel, raised card, selected row, primary/muted text, blue-purple accent, online/offline, and border colors. Device and telemetry sidebars can be collapsed and use custom scrollbar colors. Home lists known fake devices; its Connect buttons issue the dedicated Connect command. Settings provides hosting, allowlist, and default-quality controls. The local panel presents independent keyboard and mouse capture controls; when the device sidebar is collapsed, these remain available as compact buttons with tooltips. Audio remains a disabled “not supported” placeholder.

Action buttons have hover, pressed, selected, and focus-ring states. Offline or non-host-capable devices cannot be selected for streaming. Other overlays cover connecting, switching, paused, reconnecting, approval, and error states.

| Interaction | Result |
|---|---|
| Home | Opens the fake device overview. |
| Online host in device rail | Selects the host and opens its selected display in the session. |
| Home Connect | Sends Connect with the selected default quality; offline or unapproved hosts are disabled. |
| Offline or unapproved peer | Shows its state; the rail entry is disabled for streaming. |
| Discover | Requests a fake device rescan and re-emits known peer status. |
| Header monitor selector or sidebar display row | Sends SelectDisplay for an available display. The old frame remains through the 150 ms simulated switch and until matching reset/readiness metadata is polled. |
| Remote Desktop | Toggles keyboard and mouse capture together. |
| Keyboard or mouse capture controls | Toggle the two local capture states independently. Actual event forwarding is not part of M4b. |
| Session quality | Sets the active session preference; the fake host can clamp it to the display and platform tier. |
| Settings default quality | Sets the preference used when selecting a future peer. |
| Sidebar controls | Collapse or expand the device or telemetry sidebar. |
| Fullscreen and Hide | Toggle fullscreen or send SetVisible(false) and minimize the window. Escape releases both capture states. |
| Audio | Disabled; tooltip reads “not supported.” |

The keyboard map currently implemented is:

| Key | Behavior |
|---|---|
| Tab | Move focus to the next enabled action button |
| Shift+Tab | Move focus to the previous enabled action button |
| Enter or Space | Activate the focused action button |
| Escape | Release keyboard and mouse capture |

The custom focus wrapper adds focus traversal, activation, and a visible accent ring. Explicit accessible-name/role metadata is not assigned to controls in the current app. The visual text and tooltips are not a substitute for screen-reader labels. The availability and completeness of iced 0.14 accessibility APIs have not been established in this milestone.

## View model, core bus, and video handoff

crates/app/src/view_model.rs is the toolkit-independent reducer. ViewModel::reduce_action changes local UI state and returns racc_core::UiCommands; ViewModel::apply_event applies metadata CoreEvents from the core. Widgets render the resulting state and translate interactions into UserActions. The app polls events and a CoreSnapshot on its 250 ms telemetry timer.

CoreHandle, UiCommand, CoreEvent, and CoreSnapshot carry commands and metadata only. The core crate contains no iced types. Video pixels and frame metadata do not travel through the UI event bus: the renderer reads the latest frame directly from the separate FrameSource handoff. A producer publishes through FrameSink, replacing stale frames instead of growing a queue. The current fake source emits renderer-generated pattern metadata; the shader draws the synthetic animation in iced's native wgpu render context. This is not a real decoder or an NV12 upload test.

The four shell regions use iced's `lazy` widget with independent cache keys. A telemetry-only change rebuilds the telemetry sidebar content; the device rail and device sidebar keys remain stable. The workspace key includes the live RTT shown in its subtitle, so a changed RTT rebuilds that region. A unit test verifies these dependency boundaries. Iced still calls the root view, lays out the window, and redraws the full window after an update; this cache avoids rebuilding and diffing every child subtree, not full-window rendering work.

## Fake scenario

Run with --fake to use the deterministic racc-testkit::FakeCore scenario (the app refuses normal launch in this milestone). It contains three peers: an online Windows host with three displays, an offline Windows peer, and an online Mac peer that is initially not host-capable. The fake Mac peer can produce the authorization prompt and becomes host-capable after approval.

The active Windows display starts with an animated 30 fps pattern. Its metadata changes with the displayed monitor and quality selection. Fake telemetry varies over time and retains scripted display-switch, decoder-reset, quality-adjustment, and packet-loss events. Display switching holds the prior frame for 150 ms, then keeps it until the matching StreamReset and DecoderReady events are polled; the fake source publishes the replacement frame at that point. The UI processes those metadata events together before rendering its next frame. The seed is fixed in the app; FakeCore also supports explicit seeds for deterministic tests.

## Running and measurement controls

    cargo run -p racc-app -- --fake
    cargo run -p racc-app -- --fake --telemetry-collapsed
    cargo run -p racc-app -- --fake --measure-secs=60
    cargo run -p racc-app -- --fake --telemetry-collapsed --measure-secs=60
    cargo run -p racc-app -- --fake --fake-idle --measure-secs=15

--measure-secs=N prints interval statistics and exits after the measurement duration. --telemetry-collapsed starts with that sidebar collapsed. --fake-idle disables synthetic frame advancement and frame sampling while leaving the app window and telemetry updates active; it is a measurement path, not the normal session view.

The app samples when the observed latest FrameSource frame ID changes, timestamps observations with a CPU monotonic clock, and computes median, p95, and count above 40 ms. This is a source-update cadence proxy: it does not measure GPU completion, monitor refresh, or physical presentation. The Release app was run visibly on Windows 10.0.19045 with a five-second process warm-up. Process CPU used the `TotalProcessorTime` delta divided by sampled wall time; private bytes were sampled about once per second. Frame cadence uses the app's own 60-second `--measure-secs` window.

### Release measurements

| Mode | Frame window | Frame-ID cadence samples | Median | p95 | >40 ms | Mean CPU (CPU-s/s) | Private memory mean / peak |
|---|---:|---:|---:|---:|---:|---:|---:|
| Sidebar open | 60 s | 1,683 | 33.23 ms | 34.97 ms | 16 | 0.065 | 307.0 / 307.1 MiB |
| Telemetry collapsed | 60 s | 1,683 | 33.28 ms | 34.70 ms | 15 | 0.044 | 304.0 / 304.9 MiB |
| Fake idle | 15 s | no frame samples | n/a | n/a | 0 | 0.063 | 305.5 / 305.7 MiB |

For this post-cache release rerun, app cadence used the stated 60-second frame window. A five-second process warm-up preceded external process sampling; open, collapsed, and idle CPU/private-memory windows were 59.12 s, 57.97 s, and 11.18 s, with private bytes sampled about once per second. Each run exited with status 0 and wrote the cadence summary. Measurements describe this Windows machine and this Release build only. They do not measure GPU usage, physical presents, or the 2015 Mac. The M4b human visual review remains required.
### Comparison with M4a

M4a's owner-observed iced run generated 1,884 synthetic frames at 33.334 ms mean and 34.289 ms p95. It uploaded 1,464 latest frames; active upload intervals averaged 33.469 ms, p95 35.453 ms, and maxed at 159.509 ms. The owner reported no visible stutter in that 60-second run. M4a's earlier run recorded 1,685 uploads over 61.868 seconds, with active intervals averaging 33.372 ms and p95 37.420 ms.

M4b's post-cache open-sidebar p95 is 34.97 ms and its >40 ms count is 16 in 60 seconds. M4a recorded a 35.453 ms p95 active upload interval and a 159.509 ms maximum. These are different measurements, not a direct before/after comparison; neither measures physical monitor presents. The human M4b visual review remains required.

## Known limitations

- --fake is the only implemented launch mode. Networking, capture, decode, real input forwarding, and native tray behavior are outside this milestone.
- The shader displays a synthetic test pattern; M4b does not validate the final decoder-to-renderer path or real NV12 uploads.
- The Release measurements above are frame-source update and process-resource proxies, not physical present or GPU completion measurements.
- The custom controls support keyboard focus traversal and visible focus indication, but explicit screen-reader names/roles are absent and iced 0.14's accessible-name coverage has not been audited.
- The no-op tray controller defines the boundary only. A real tray icon and hide-to-tray integration are later packaging work.

