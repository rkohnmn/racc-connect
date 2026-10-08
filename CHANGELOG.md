# Changelog

User-visible changes are recorded here as release artifacts are prepared. No public release has been published.

## Unreleased

- Added an original, reproducible raccoon SVG mark and generated Windows/macOS/tray icon formats.
- Added versioned app settings, migration and corruption handling, native tray and close-to-tray wiring, monitor-aware geometry restore, single-instance activation, autostart controls, and their focused tests. Native runtime and install behavior remain HUMAN-PENDING; hosting preference waits on local agent IPC.
- Added release and notice-generation scripts, a portable ZIP packager, Windows firewall/autostart scripts, an Inno Setup source, and macOS app-bundle/notarization/LaunchAgent scripts. Install and signing steps have not been run.
- Added session-gated Windows and macOS text clipboard adapters and macOS Screen Recording/Accessibility Settings controls; native Mac behavior and cross-machine clipboard remain unverified.
- Project license remains undecided; no `LICENSE` file is included.
