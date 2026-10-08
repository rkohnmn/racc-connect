# ADR 0031: Native tray behavior and iced event polling

- Status: Accepted; runtime implementation present, platform observation pending
- Date: 2026-10-07

## Context

The iced/wgpu app needs a native tray on Windows and macOS. The current iced application API does not expose winit's event-loop proxy, while the selected tray library requires creation on the event-loop thread (and, on macOS, after the event loop has started).

## Decision

Use `tray-icon` 0.24.2, licensed MIT OR Apache-2.0, behind a Windows/macOS target dependency. Create the icon in the first iced update tick after a window ID is observed. Poll tray and menu receivers from the existing 250 ms app tick; the small dispatch delay is acceptable for menu actions and keeps all UI state updates on iced's application thread. Use the original generated color raccoon icons on Windows and the generated monochrome template on macOS. Menu actions are Open, Hosting preference, and Quit; left-click opens the app. Tooltip text contains connection state only.

## Consequences and verification

The tray object is retained for app lifetime. Iced 0.14 has no hide command, but `window::run` exposes the live raw window handle on the UI thread. The app now calls `ShowWindow(SW_HIDE/SW_RESTORE)` on Windows and uses AppKit `NSWindow::orderOut` / `makeKeyAndOrderFront` on macOS. The viewer visibility event is applied before the native call, so hidden windows stop decoding; if tray initialization or native visibility fails, the app falls back to the taskbar or Dock. A second launch sends the existing process a local show request; its tray path shows and focuses the window.

Startup geometry is checked against Windows `MONITORINFO.rcWork` (converted with each monitor's effective DPI) or macOS `NSScreen.visibleFrame` data, with AppKit coordinates converted to the app's top-left desktop convention. These work areas are expressed in the app's logical point coordinates. If monitor enumeration is unavailable, the pure geometry helper bounds saved dimensions and places the window at the origin. The raw-handle window adapter compiles for Windows and macOS; pure geometry and menu tests pass. Native tray, AppKit/Win32 calls, monitor arrangement, and second-launch activation have not been observed on hardware and remain HUMAN-PENDING. Hosting changes remain persisted preferences until local host-agent IPC exists; they do not start or stop the service. The app polls tray events rather than waking iced through a native event proxy.
