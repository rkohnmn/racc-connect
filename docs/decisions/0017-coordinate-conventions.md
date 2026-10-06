# ADR 0017: Coordinate conventions and rounding

Status: accepted for M3a.

Keep coordinate functions pure and independent of host OS APIs. Use integer round-half-up arithmetic for letterbox geometry, normalized pointer values, host physical pixels, and Windows virtual-desktop absolute coordinates. Reject the right/bottom edge for pointer rejection; clamp maps it to 65535. Use the host's logical point geometry directly on macOS. Scale cursor geometry from physical pixels into the rendered video rectangle, and leave relative motion raw.

The Windows absolute formula and its DPI behavior are not verified on hardware. M3a records the required `MOUSEEVENTF_VIRTUALDESK` versus `SetCursorPos` corner comparison in `docs/HARDWARE.md`; choose the production path after that M6/M7 check.
