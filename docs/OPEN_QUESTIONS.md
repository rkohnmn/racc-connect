# Open Questions

Record unresolved product, architecture, environment, or instruction questions here. Resolve them with the project author before making assumptions that affect implementation.

1. [RESOLVED] The project name is Racc Connect.
2. Which project license should apply? No license has been chosen.
3. Which Tailscale LocalAPI access method must the identity crate use on each supported platform and installation flavor?
4. Which UI toolkit will the M4a spike select, based on measured evidence?
5. [RESOLVED] Documentation may mention Discord only to describe design inspiration. Product code, identifiers, UI strings, window titles, branding, package or file names, and shipped assets must not use its name, logo, or assets.
6. [RESOLVED] The verification scripts are listed in AGENTS.md as required by section 10.3.
7. [RESOLVED] docs/DEV_SETUP.md and both check-all scripts use `cargo deny check --warn vulnerability --warn unsound --warn unmaintained --warn notice --warn yanked`.
8. At M10, select and verify the open license for the raccoon emoji artwork (for example, Google Noto Emoji); do not use Apple's emoji artwork or Discord logo shapes.
9. [OWNER CONFIRMATION] Confirm the Mac's exact model and year in About This Mac. Its supplied specifications are consistent with a mid-2015 15-inch MacBook Pro, but that identification is an unverified inference.
10. Which Unicode text-injection behavior is needed for international keyboard layouts?
11. After M7 measures input latency and loss, should mouse motion move from the reliable TCP control channel to latest-wins UDP?
12. After observing real encoder output, what maximum protocol frame-size policy is required?
13. Because the UDP video header has no payload-length field, can v0 treat every datagram with a valid header and nonempty payload as a valid fragment even when it is shorter than an earlier fragment? Tests can detect truncation only before the header completes or when payload is empty.
