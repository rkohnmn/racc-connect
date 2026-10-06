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
13. [RESOLVED — M2] Uniform fragment sizing is enforced: non-final payloads are exactly 1182 bytes and the final payload is 1–1182 bytes. UDP datagrams preserve boundaries and WireGuard authenticates them, so in-transit payload truncation is outside the threat model. A shortened final payload cannot be distinguished from a valid shorter final fragment because v0 carries no full-frame byte length.
14. [M2.5 RECOMMENDATION] Corrected simulation recommends evaluating bounded NACK-all in M7 first: at 720p/2%/1 ms it delivered 99.956% with 6.457% stale time and 2.460% overhead, versus baseline 2.467% / 95.550%. The measured v0 all-tier range under a 90%-delivered / 10%-stale criterion was 0-0.1% IID loss; 480p also passed at 0.25%. Should M7 proceed with this recommendation after real-path measurements? No NACK or FEC is implemented.
15. [HUMAN-PENDING — M2] How does `std::thread::sleep` pace the 233 KB frame on the 2015 Intel Mac? Windows 10 PC #1 measured 20,167 µs versus the 19,999 µs target; Mac timer behavior is not measured.
16. [HUMAN-PENDING — M7] Once Tailscale is installed and confirmed on both Windows PCs, what do two-machine loss, direct/DERP path, and end-to-end frame recovery measurements show? This machine had no Tailscale installation, so M2 tested loopback only.

17. [M2.5 WIRE FOLLOW-UP] If M7 proceeds with NACK-all, finalize bounds and wire semantics for a control message carrying epoch, frame ID, missing-fragment ranges, and a maximum two-round counter. The M2.5 model reused existing video fragment headers and changed no wire format.
18. [RESOLVED — M2.5 LOOPBACK CHECK; HISTORICAL LOSS STAGE INCONCLUSIVE] The original real-UDP smoke test failed three times with one fewer receiver datagram than the proxy's old `forwarded` counter reported, despite zero configured proxy loss. It also required zero keyframe requests after that loss, which contradicted the expected UDP loss-recovery path. The test now asserts exact delivery when no loss is observed and otherwise requires a keyframe request and recovery at the trailing IDR. The proxy now distinguishes successful sends from failed/short `send_to` calls. The corrected test passed in the final full workspace and both `check-all` runs. Historical send-versus-receive loss stage remains unknown; no protocol or transport-wire issue was found.
Final evidence: offline `cargo test --workspace` passed with racc-net 32 passed/1 ignored, racc-proto 20 passed, racc-testkit 15 passed, and all other unit/doc harnesses at 0 tests. Both offline `scripts/check-all.ps1` and `scripts/check-all.sh` completed successfully. The separate `PROPTEST_CASES=10000` racc-net run and both platform compile-only checks also passed. The no-network restriction still prevents the required normal push.
