# Input capture and injection

Input forwarding is opt-in per remote desktop session. View-only sessions send no input. The viewer reducer accepts physical USB HID usages and coordinates relative to the actual rendered video rectangle. It tracks held keys and buttons and emits releases when focus is lost, the window is hidden, the stream is paused, the session disconnects, or the stream target changes. Pointer hover outside the rendered image is rejected; a drag that began inside is clamped to the video edge.

## Live viewer capture

The live iced viewer listens for physical key codes and maps supported USB HID usages; it never forwards typed text. Keyboard and pointer capture are separate opt-in controls. Input is tagged with the selected peer, stream epoch, and display ID, passed through a bounded FIFO, and adjacent absolute mouse moves are coalesced. Key contents and input payloads are not logged or placed on the UI event bus.

Pointer capture uses a transparent Canvas stacked over the native video shader. Both widgets fill the same video-frame child layout, so the Canvas receives the exact shader bounds. It converts iced's window-global cursor position to that widget's local coordinates once, then uses the same rounded widget dimensions and `design::video_rect` letterbox math as the shader. Hover and button-down events in the letterbox bars are rejected; a drag that began inside is clamped by the reducer. Mouse buttons map to protocol IDs 1–5, and wheel deltas are converted to bounded protocol units.

The reducer releases held input when the app loses focus, hides or minimizes, pauses the stream, disconnects, changes stream target, or receives Ctrl+Alt+Shift+Escape. Real host injection and visual pointer behavior remain HUMAN-PENDING; these app checks use reducer and geometry fakes and do not verify a remote desktop session.

## Emergency release

Settings offers two vetted local release chords: Ctrl+Alt+Shift+Escape and Ctrl+Alt+Shift+F12. The selected chord is consumed by the viewer before normal forwarding, releases tracked remote keys/buttons, and returns to view-only mode. Ctrl+Alt+Shift+Escape always remains available as a built-in emergency fallback. The chord key is never sent to the host. Ctrl+Alt+Del is not supported: Windows reserves it for the secure attention sequence.

## HID mapping

The platform-neutral mapping uses USB HID Keyboard/Keypad page usages and maps supported usages to Windows scan codes. Unsupported usages are rejected instead of guessed. The Windows host adapter uses SendInput with scan-code events and explicit extended-key flags. USB usage definitions are maintained by the USB-IF HID Usage Tables (https://www.usb.org/hid); Windows injection semantics and integrity-level restrictions are documented by Microsoft's SendInput reference (https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput).

The mapping table covers the standard keyboard and keypad usages, the non-US key, F1–F24, navigation keys, modifiers, and the page-7 volume controls. Consumer Control page media usages cannot be represented by the current protocol field. USB usages are layout-independent physical identities, so non-US layouts and application behavior still need owner-run checks. The Mac viewer's Control/Command swap is a saved Settings preference; its physical key behavior remains HUMAN-PENDING. No key values or input payloads should be logged.

## Validation and rate limits

The host controller validates epochs, display IDs, key usages, modifier bits, button IDs, normalized pointer bounds, event shape, and per-session event rate before calling the injector. It tracks pressed state and releases held input when the session ends. The Windows API can reject injection across integrity levels or on a desktop where the helper has no access; the host must report that state instead of silently treating the event as successful.

The Windows helper supports two absolute pointer methods. The local setting `RACC_POINTER_INJECTION_METHOD` accepts exactly `virtual-desktop-absolute` or `set-cursor-pos`; an absent setting uses `virtual-desktop-absolute`, while any other value prevents the helper from starting instead of silently changing behavior. `virtual-desktop-absolute` uses `SendInput` with `MOUSEEVENTF_VIRTUALDESK`; `set-cursor-pos` calls `SetCursorPos` with the host pixel already mapped and validated by the input controller. Both remain behind the same session, epoch, display, coordinate, and rate validation.

The default is provisional only. No real key or pointer events were injected during these checks. Corner and center accuracy has not been measured, and there is no measured winning method.

## Pointer method comparison

**HUMAN-PENDING** — Run on each Windows monitor at its four corners and center, recording the target point, observed cursor point, and pixel error for both methods. Do not treat this table as evidence of measured accuracy until those runs are recorded.

| Method | Four corners | Center | Evidence/default status |
|---|---|---|---|
| `virtual-desktop-absolute` | HUMAN-PENDING | HUMAN-PENDING | Provisional default; measurement pending |
| `set-cursor-pos` | HUMAN-PENDING | HUMAN-PENDING | Available by setting; measurement pending |

## Verification state

- TESTED-FAKE: viewer capture reducer, release on state changes, letterbox-aware pointer normalization, local emergency chord, host-side validation, rate limiting, and fake injector behavior.
- COMPILE-ONLY: Windows input adapter target check.
- HUMAN-PENDING: real injection on the helper's interactive desktop, all monitor corners/center, mixed DPI, non-US keyboard layouts, shortcuts, drag/wheel, release on disconnect, secure desktop behavior, and two-PC sessions.

See docs/VIEWER.md, docs/HARDWARE.md, and docs/PROGRESS.md.
