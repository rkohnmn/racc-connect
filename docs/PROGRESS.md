# Project Progress

## Environment

Probe date: 2026-10-06 (America/New_York).

- [VERIFIED-RUN] The agent environment is Microsoft Windows 10.0.19045 (Windows_NT), reported by the Windows OS-description command.
- [VERIFIED-RUN] rustc reports 1.95.0 (59807616e 2026-04-14); Cargo reports 1.95.0 (f2d3ce0bd 2026-03-21).
- [VERIFIED-RUN] The repository pins stable Rust 1.95.0 with rustfmt and clippy.
- [COMPILE-ONLY] Installed targets: x86_64-pc-windows-msvc and x86_64-apple-darwin. Both target-add commands exited 0; Windows was already current and macOS standard-library support was installed.
- [VERIFIED-RUN] The 2026-10-06 probe did not find Tailscale on PATH; the M5c probe on 2026-10-07 found the CLI and verified read-only live identity behavior on Windows PC #1. See the M5c session record below.
- [VERIFIED-RUN] This machine matches Windows PC #1 by its NVIDIA GeForce RTX 3050 Ti Laptop GPU. Windows build, GPU drivers, and three active 1920×1080 displays were probed read-only; see docs/HARDWARE.md. Windows PC #2 and the Mac remain owner-reported only.

## Milestone status

| Milestone | Status | Evidence |
|---|---|---|
| M0 Workspace scaffold | Complete | [COMPILE-ONLY] Workspace, scripts, and documentation checks passed; see final report. |
| M0.5 Housekeeping and publish | Complete | [VERIFIED-RUN] M0.5 checks passed; `main` was pushed to the empty requested origin. |
| M1 Protocol | Complete | [VERIFIED-RUN] Protocol tests, documentation, cargo-deny, scripts, and compile-only target checks passed; see the M1 report. |
| M2 Transport | Complete | [VERIFIED-RUN] M2 acceptance checks 1–13 passed on Windows 10.0.19045; see the final report below. Windows and Intel macOS target checks are COMPILE-ONLY. |
| M2.5 Transport audit | Complete | [VERIFIED-RUN] All 14 M2.5 acceptance checks passed; the audit and final evidence are recorded below and were pushed to origin/main. No M3 work was started. |
| M3a Topology, coordinate math, telemetry | Complete | [VERIFIED-RUN] All M3a checks pass; [COMPILE-ONLY] Windows and macOS cross-checks pass. The user authorized a normal push, and the commits were published as a fast-forward to origin/main. |
| M3b Host and viewer session lifecycle | Complete | [VERIFIED-RUN] 100 session tests passed, including 20 virtual-time multi-event scenarios and 10,000-case property testing; workspace format, strict Clippy, full tests, Windows target compile and repository layering/feature checks pass. See final integrated audit below. |
| M4a UI toolkit spike | Complete | [VERIFIED-RUN] iced selected and compared against Slint; [HUMAN-VERIFIED] owner reports no visible stutter on PC #1. See ADR 0001. |
| M4b UI shell | Best-judgment redesign implemented; owner visual review deferred | [TESTED-FAKE] app checks pass. The owner asked us to continue while screen review is unavailable; see `blocked.md`. |
| M5 Windows capture and encode | Partial — hardware acceptance pending | [VERIFIED-RUN] PC #1 metadata-only capture probe; [VERIFIED-RUN — SYNTHETIC ONLY] 90-frame 720p Media Foundation encode inspected and decoded; [HUMAN-PENDING] real capture, playback, mode recovery, and PC #2. See M5 report below. |
| M6 Windows host agent | Partial — service/helper, foreground host, named-pipe IPC and bounded probe sources are integrated; acceptance pending | [TESTED-FAKE] host-agent tests cover authorization, attempt limiting, IPC and input; control-server shutdown now drains accepted handlers through disconnect callbacks. [COMPILE-ONLY] Windows target passes. The app hosting toggle remains `Unavailable` because reversible runtime stop/rebind is not implemented; pipe/service/Tailscale/hardware checks remain pending. |
| M7 End-to-end viewer | Partial — discovery-first viewer, decoder factory, live runtime, input controls and wgpu NV12 renderer are source-integrated | [TESTED-FAKE] app/core reducer and runtime tests pass, including automatic reconnect copy, selected decoder telemetry and path metadata. [COMPILE-ONLY] full Windows workspace target passes; macOS app target strict Clippy passes. No real DDA→encode→decode→render session, DXVA path, two-PC input, or human measurements. |
| M8 Clipboard and telemetry | Partial — protocol v3 session control, Windows/macOS host/viewer text adapters, telemetry and events are source-integrated | [TESTED-FAKE] policy, loop prevention, opt-in/disable wire control, bounded queues, runtime/host loopback and telemetry tests pass. [VERIFIED-RUN] An explicitly invoked Windows OS clipboard test against a fake remote passed with fixed harmless text. No real PC-to-PC clipboard round-trip, live telemetry/pacing comparison, or Mac host StatsReport measurement. |
| M9 macOS host and viewer | Partial — Mac capture, VideoToolbox, IPC, clipboard, permissions, viewer, and machine-wide CPU telemetry are source-integrated | [COMPILE-ONLY] Apple-target app Clippy plus capture/decode/input/clipboard checks pass. Three deterministic CPU-sampler fake-counter tests and an isolated Apple-target probe pass. The full host-agent Apple target check is blocked before Rust compilation because OpenH264 cannot find target `c++`. Monterey behavior, LaunchAgent execution, permissions, and all Mac runtime tests remain HUMAN-PENDING; M9 is not complete until the Mac checklist passes. ADR 0042 specifies pointer-in-video on Mac. |
| M10 Polish and packaging | Partial — UI preferences, tray/window lifecycle, original artwork, notices, and Windows/macOS packaging sources are integrated | [TESTED-FAKE] settings/tray/geometry/autostart/single-instance tests pass. [VERIFIED-RUN] Windows release profile was measured and a checksummed portable ZIP was built. [HUMAN-PENDING] Inno installer, clean install/uninstall, native tray, Mac bundle/signing, idle memory, 24-hour soak and owner license/distribution decision remain open. `scripts/check-all.ps1` reaches cargo-deny and fails only on required OpenH264 BSD-2-Clause versus the unchanged allowlist. |

## Verified

- [VERIFIED-RUN — HISTORICAL CHECKPOINT] Both check-all scripts exited 0 at earlier M4b checkpoints. The current M10-integrated check now runs Windows/macOS-target cargo-deny and exposes the required OpenH264 BSD-2 allowlist conflict; see the latest session record.
- [VERIFIED-RUN] The two binary stubs printed their package names and versions.
- [VERIFIED-RUN] The layering check passed normally and rejected a temporary forbidden rd-core to rd-app dependency edge.
- [VERIFIED-RUN — 2026-10-07] Current format, strict workspace Clippy and offline workspace tests passed. Layering, feature-gate and Windows MSVC workspace checks passed. Apple app strict Clippy and Apple-target capture/decode/input/clipboard checks passed; the full Apple host check is blocked before Rust code by unavailable target `c++` for OpenH264.
- [VERIFIED-RUN] M1 bounded v0 protocol passed 20 racc-proto tests, the 10000-case property run, cargo-deny and both check-all scripts.
- [HUMAN-PENDING] Real Windows, Mac, GPU, input, and Tailscale checks remain listed in docs/HARDWARE.md.

## Unverified

- [UNVERIFIED — M0 only] At the M0 run, cargo-deny was not installed, so deny.toml parsing and an advisory database check were skipped. This was resolved during M0.5; see the current M0.5 session report below.
- [COMPILE-ONLY] Cross-checks establish compilation only; they do not demonstrate platform behavior.
- [HUMAN-PENDING] Hardware and tailnet checks are not performed by this agent.

## Blocked

- Normal Codex sandboxed PowerShell creation still fails before process start with `helper_unknown_error: setup refresh had errors`. Elevated PowerShell was used for repo checks. User config already contains the documented `unelevated` fallback, but it requires restarting Codex; no supported in-task app/helper restart control is exposed. See `blocked.md` and [Windows sandbox troubleshooting](https://learn.chatgpt.com/docs/windows/windows-sandbox).
- M8 peer-to-peer clipboard, live telemetry/pacing measurements and Mac runtime remain HUMAN-PENDING. Windows and macOS text adapters, telemetry and event feeds are source-integrated; the owner authorized enabled-session text sync and the fixed-string Windows OS clipboard/fake-remote test passed.
- M9 still requires 2015 Mac hardware/runtime checks. Same-user Mac app/host IPC, LaunchAgent sources, pointer-in-video capture policy, and a machine-wide CPU sampler are integrated, but not run on Mac. Full Apple host compilation remains blocked before Rust compilation by missing target C++ tooling for OpenH264. The permission panel and explicit Settings buttons are unverified on Monterey.
- M10's unchanged cargo-deny policy rejects the mandated OpenH264 BSD-2 fallback. The Windows portable artifact was built and measured, but private idle memory, clean install, native lifecycle behavior, Mac bundle and 24-hour soak remain HUMAN-PENDING. The project license and distribution scope remain owner decisions.
- M11a remains ordered after M8 and M10 acceptance; its prompt preflight is not met while real peer/hardware acceptance and the license/policy decisions remain open. See `blocked.md` and `docs/OPEN_QUESTIONS.md`.

## Session log

### 2026-10-06 — M0 workspace scaffold

- [VERIFIED-RUN] Read the supplied M0 objective, AGENTS.md, and the project scope and setup documents. The scope and setup documents were at the repository root, so they were placed under their documented docs/ paths without changing their existing text other than the permitted package-name and layering references.
- [COMPILE-ONLY] Created all 15 Cargo workspace members as minimal stubs, pinned Rust 1.95.0, retained Cargo.lock, and added the Rust ignore rules and license/advisory configuration.
- [VERIFIED-RUN] Added executable Bash checks and Windows PowerShell equivalents, plus project documentation seeds and ADRs.
- [VERIFIED-RUN] Committed the work locally on branch main. No remote was added and nothing was pushed.
- [HUMAN-PENDING] No human-only hardware or tailnet checks were claimed as complete.

### 2026-10-06 — Owner-reported hardware inventory

- [UNVERIFIED] The owner supplied the MacBook, laptop, and server PC specifications now recorded in docs/HARDWARE.md. The agent did not independently inspect the machines or verify drivers, operating-system builds, or monitor details.
- [HUMAN-PENDING] Hardware behavior tests and the missing machine details remain on the checklist.
#### Final M0 acceptance report

1. PASS — cargo fmt --all -- --check; exit 0, no formatting diff.
2. PASS — cargo clippy --workspace --all-targets -- -D warnings; exit 0, Finished dev profile with no warnings.
3. PASS — cargo test --workspace; exit 0; all workspace test harnesses reported 0 passed and 0 failed because the M0 stubs contain no tests or feature behavior.
4. PASS — scripts/check-layering.sh and scripts/check-layering.ps1; each reported “Layering check passed for 13 library crates.” Negative test temporarily added rd-core → rd-app and a temporary rd-app library target; the check exited 1 with “Forbidden application dependency found in library dependency tree: rd-core.” Both temporary changes were removed, cargo tree showed rd-core alone, and the final Git tree is clean.
5. PASS — scripts/check-all.sh, run through Git Bash, exit 0; output included the layering pass and a notice that cargo-deny is not installed. scripts/check-all.ps1 also exited 0 on Windows.
6. PASS — cargo metadata --format-version 1 --no-deps reported 15 workspace packages for 15 crate directories, all prefixed rd-: rd-app, rd-capture, rd-clipboard, rd-core, rd-decode, rd-encode, rd-host-agent, rd-identity, rd-input, rd-net, rd-proto, rd-session, rd-telemetry, rd-testkit, rd-topology.
7. PASS — Cargo manifest dependency scan found no [dependencies] sections; there are no third-party or inter-crate dependencies in M0.
8. PASS — #![forbid(unsafe_code)] is present in rd-proto, rd-net, rd-topology, rd-session, and rd-telemetry.
9. PASS — all 15 required documentation and decision files exist and are non-trivial; README.md exists.
10. PASS — docs/PROGRESS.md contains the Environment section, milestone table, verified/unverified/blocked sections, session log, and this final report.
11. PASS — cargo check --workspace --target x86_64-pc-windows-msvc and cargo check --workspace --target x86_64-apple-darwin both exited 0; each printed “Finished dev profile.” Label: COMPILE-ONLY.
12. PASS — local Git history contains small commits on main; git remote -v is empty. Final status is clean and no changes are pushed.

##### Created or changed

- Workspace manifest and lockfile; pinned toolchain; .gitignore; deny.toml.
- Minimal source and manifest files for 13 libraries and 2 binaries.
- Bash and PowerShell check-all and dependency-layering scripts.
- README.md; moved the existing scope and setup files into docs/; M0 objective copy; protocol, progress, questions, hardware, packaging documentation.
- ADR template, pending M4a placeholder, and accepted ADRs 0002 through 0007.
- AGENTS.md session-start and script references; DEV_SETUP package-name corrections and the dependency-layering rule.

##### UNVERIFIED, COMPILE-ONLY, and HUMAN-PENDING

- [COMPILE-ONLY] Windows MSVC and Intel macOS target checks passed; no platform behavior is implemented or verified.
- [UNVERIFIED] cargo-deny was unavailable, so deny.toml parsing and an advisory database run were skipped by both scripts.
- [UNVERIFIED] Tailscale was not installed or available on PATH.
- [HUMAN-PENDING] Real GPU paths, service/session transitions, multi-monitor switching, end-to-end viewer and clipboard behavior, latency, international keyboard layouts, Mac capture/encode performance, and Tailscale path/allowlist checks.

##### Open questions

1. Final project working title.
2. Project license.
3. Per-platform Tailscale LocalAPI access method.
4. M4a UI toolkit choice and supporting measurements.
5. The attached goal's branding restriction conflicts with the authoritative instruction in AGENTS.md and existing project-scope text; source documents were preserved and no new product-facing branding references were added.
6. AGENTS.md requires verification scripts to be listed there, while the goal otherwise limits AGENTS.md edits; the required script listing was added.
7. docs/DEV_SETUP.md's manual cargo deny check command remains strict while the check scripts explicitly downgrade advisory findings to warnings; reconcile that documentation command if the same warning-only policy is desired for manual runs.

##### M1 starting point

M1 should start from the `racc-proto` package in `crates/proto` and the table of contents in `docs/PROTOCOL.md`. Implement the bounded v0 header and control message parsing/serialization there, update `docs/PROTOCOL.md` in the same change, and add the M1 malformed-input and round-trip tests. No M1 protocol types or behavior were started during this session.

### 2026-10-06 — M0.5 housekeeping and publish

- [VERIFIED-RUN] Preflight began with a clean main worktree and `scripts/check-all.ps1` exited 0.
- [VERIFIED-RUN] Set repository-local Git identity to `rkohnmn <275230809+rkohnmn@users.noreply.github.com>`. Rewrote the four unpublished commits so both author and committer match; the post-rewrite audit shows all four correct and their messages contain no attribution trailers. No global Git config command was used.
- [VERIFIED-RUN] Cargo metadata reports 15 workspace packages and every package is prefixed `racc-`; crate directory names are unchanged. Current tree search for the former package prefix finds matches only in historical ADR 0002 and historical M0 text in this file.
- [VERIFIED-RUN] `cargo-deny` 0.20.2 installed with `cargo install cargo-deny --locked`. The initially requested shorthand `--warn advisories` was rejected by this release because `advisories` is a check name, not a lint. The supported command `cargo deny check --warn vulnerability --warn unsound --warn unmaintained --warn notice --warn yanked` exits 0 and reports `advisories ok, bans ok, licenses ok, sources ok`. Scripts and manual instructions use that verified command.
- [VERIFIED-RUN] The layering negative test temporarily added `racc-core → racc-app`; the PowerShell check exited 1 with `Forbidden application dependency found in library dependency tree: racc-core`. The temporary manifest addition was restored byte-for-byte and temporary library target removed. The normal Bash and PowerShell checks report `Layering check passed for 13 library crates.`
- [VERIFIED-RUN] This Windows PC was identified as PC #1 by its NVIDIA GeForce RTX 3050 Ti Laptop GPU. Read-only probes recorded build 19045, driver 32.0.15.9571, AMD integrated graphics driver 31.0.21923.11000, three active displays at 1920×1080, Rust/Cargo 1.95.0, and Tailscale not installed/on PATH. Details and owner-reported hardware are in docs/HARDWARE.md.
- [VERIFIED-RUN] Project name, brand rule, licensing notice, hardware record, questions, and ADR 0008 are updated. ADR 0002 is marked superseded. Human-only hardware items remain unchecked.
- [VERIFIED-RUN] `scripts/check-all.ps1` and `scripts/check-all.sh` each exited 0 and ran cargo-deny. `cargo check --workspace --target x86_64-pc-windows-msvc` and the Intel macOS target check both exited 0. Target checks are [COMPILE-ONLY].
- [VERIFIED-RUN] The working tree and all five committed snapshots passed the public-hygiene scan for local paths, private usernames/machine names, disallowed email addresses, and secret-like material. The only email found was the approved GitHub noreply address.
- [VERIFIED-RUN] `git ls-remote origin` exited 0 with no refs. `git push -u origin main` completed as a normal push and created `main` on the requested origin. `git status -sb` reported `## main...origin/main`.

#### Final M0.5 acceptance report

1. **PASS** — `git status --short --branch` was clean before changes (`## main`); the preflight `scripts/check-all.ps1` exited 0 before M0.5 edits.
2. **PASS** — `git config --local --get-regexp '^user\.(name|email)$'` reported `user.name rkohnmn` and `user.email 275230809+rkohnmn@users.noreply.github.com`. `git log --format='%h | author: %an <%ae> | committer: %cn <%ce>' --all` showed matching identity for every commit; `git log --format=%B --all` contained no attribution/tool trailers. No global Git config command was used. The final pushed M0.5 report commit was also verified with this identity.
3. **PASS** — `cargo metadata --format-version 1 --no-deps` summarized 15 packages, all `racc-*`, across 15 unchanged crate directories. `rg -n --hidden --glob '!target/**' --glob '!.git/**' '\brd-' .` found former-prefix mentions only in ADR 0002 and historical M0 material in this file.
4. **PASS** — `scripts/check-layering.ps1` and the Bash check each passed with `Layering check passed for 13 library crates.` The negative test temporarily added `racc-core → racc-app`; the PowerShell check exited 1 with `Forbidden application dependency found in library dependency tree: racc-core`. The original manifest was restored byte-for-byte and the temporary app library target removed.
5. **PASS** — `cargo deny --version` reported `cargo-deny 0.20.2`. `cargo deny check --warn vulnerability --warn unsound --warn unmaintained --warn notice --warn yanked` exited 0 with `advisories ok, bans ok, licenses ok, sources ok`. The policy allows no unused license yet, denies GPL/AGPL by omission, warns on duplicates, rejects wildcard requirements, permits crates.io only, and rejects unknown sources. The unsupported shorthand `--warn advisories` was tested and rejected by this installed version; all repo commands use the verified current syntax.
6. **PASS** — `scripts/check-all.ps1` and `scripts/check-all.sh` both exited 0 and actually ran cargo-deny; each ended with the 13-library layering pass and all four cargo-deny checks OK.
7. **PASS** — `cargo deny check --warn vulnerability --warn unsound --warn unmaintained --warn notice --warn yanked` is identical in `docs/DEV_SETUP.md` and both check-all scripts.
8. **PASS** — `docs/HARDWARE.md` has one Environment record, labels all owner-provided specs `[OWNER-REPORTED]`, marks the Mac model inference unverified, records this machine's read-only probe and leaves all `HUMAN-PENDING` boxes unchecked. `Test-Path LICENSE` returned `False`.
9. **PASS** — AGENTS.md and the project-scope branding rule are updated; ADR 0008 is accepted and ADR 0002 is marked superseded. `.gitattributes` defines LF for shell/Rust/Cargo files, CRLF for PowerShell, and `text=auto` otherwise; tracked files were renormalized in this commit. README names Racc Connect, reports M0.5 complete/M1 next, links docs, and contains the requested no-license notice. No LICENSE file was added.
10. **PASS** — The PowerShell here-string piped into `python -` ran the tree/history hygiene scanner over the working tree and every Git snapshot and reported clean: no absolute local paths, private usernames/machine names, non-approved emails, or secret-like content. It reported only the approved GitHub noreply email.
11. **PASS** — `git remote -v` showed `origin https://github.com/rkohnmn/racc-connect.git`; `git ls-remote origin` returned no refs before push. `git push -u origin main` returned `*[new branch] main -> main`. Post-push `git status -sb` showed `## main...origin/main`; `git log origin/main --format='%h %an <%ae>'` showed every author as `rkohnmn <275230809+rkohnmn@users.noreply.github.com>`.
12. **PASS** — `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` exited 0 inside both check-all scripts. Both `cargo check --workspace --target x86_64-pc-windows-msvc` and `cargo check --workspace --target x86_64-apple-darwin` exited 0; these are [COMPILE-ONLY].

##### Created or changed

- Renamed all 15 Cargo package names and lockfile references to `racc-*`; updated both dependency-layering scripts and cargo-deny invocation in the Bash/PowerShell check-all scripts.
- Added `.gitattributes`; replaced the cargo-deny configuration with the verified v0.20.2 policy.
- Updated README, AGENTS.md, project scope, setup, progress, hardware, open questions, and archived goal instructions; saved the M0.5 prompt at `docs/goals/M0.5.md`.
- Marked ADR 0002 superseded and added ADR 0008. Set repository-local Git identity and rewrote the four unpublished earlier commits before publishing.
- Added `origin` and pushed `main` to the requested GitHub repository.

##### UNVERIFIED, COMPILE-ONLY, and HUMAN-PENDING

- [COMPILE-ONLY] Windows and Intel macOS cross-checks passed; no platform runtime behavior was tested.
- [UNVERIFIED] The owner-provided PC #2 and Mac specifications were not probed locally. The Mac model/year remains an inference; NVENC/AMF/VideoToolbox and ScreenCaptureKit availability/performance remain untested. Tailscale is not installed on this PC, so no tailnet behavior was tested.
- [HUMAN-PENDING] All GPU, service/session, real capture/encode, multi-monitor switching, end-to-end input/clipboard, latency, international keyboard, Mac thermal/performance/permission, and real Tailscale checks remain unchecked in `docs/HARDWARE.md`.
- [BLOCKED-HUMAN] None. Publication completed.

##### Open questions resolved or remaining

- Resolved: project name; updated branding rule; verification-script inventory; cargo-deny manual command.
- Remaining: project license; per-platform Tailscale LocalAPI access method; M4a UI toolkit evidence; M10 raccoon emoji asset/license verification; owner confirmation of the Mac model/year.

##### M1 starting point

M1 starts from the `racc-proto` package stub in `crates/proto` and the protocol table of contents in `docs/PROTOCOL.md`. Implement only the M1 protocol types, strict bounded parsing/serialization, and malformed-input/round-trip checks next. No M1 work was started in this session.

### 2026-10-06 — M1 bounded protocol crate

- [VERIFIED-RUN] Read the active M1 objective and saved the exact prompt at docs/goals/M1.md. Implemented only in crates/proto and documentation/config files allowed by the objective.
- [VERIFIED-RUN] Implemented the v0 18-byte video header, 19-byte cursor datagram, 16 typed control messages, bounded TCP framing, and callback-based incremental frame decoder. Production protocol code contains no unsafe, unwrap, expect, TODO, or unimplemented paths.
- [VERIFIED-RUN] racc-proto has no normal dependencies. proptest 1.11.0 is the only development dependency. cargo deny reported advisories, bans, licenses and sources OK.
- [VERIFIED-RUN] Added 10 golden/boundary tests and 10 robustness tests. cargo test --workspace passed 20 racc-proto tests; other crate harnesses contain zero tests.
- [VERIFIED-RUN] PROPTEST_CASES=10000 cargo test -p racc-proto passed. Four property tests each ran 10000 generated cases; the robustness test harness completed in 7.26 seconds.
- [COMPILE-ONLY] Windows MSVC and Intel macOS target checks both passed. These checks establish compilation only.
- [VERIFIED-RUN] Both PowerShell and Git Bash layering and check-all scripts passed. The scripts reported 13 library crates layered correctly and all cargo-deny checks OK.
- [UNVERIFIED] UDP video has no payload-length field, so a shortened datagram with a valid header and nonempty payload cannot be distinguished from a valid shorter fragment. The limitation and resulting test boundary are documented in docs/PROTOCOL.md and question 13 in docs/OPEN_QUESTIONS.md.
- [HUMAN-PENDING] No hardware behavior was tested; M1 is protocol-only. Existing machine and hardware checklists remain unchanged.

#### Final M1 acceptance report

1. PASS — cargo fmt --all -- --check; exit 0.
2. PASS — cargo clippy --workspace --all-targets -- -D warnings; exit 0, Finished dev profile with no warnings.
3. PASS — cargo test --workspace; exit 0; racc-proto integration tests reported 20 passed and 0 failed (10 golden, 10 robustness); all other workspace harnesses reported 0 tests.
4. PASS — PowerShell set PROPTEST_CASES=10000 then cargo test -p racc-proto; exit 0; all four property tests completed at 10000 cases each; robustness harness 7.26 seconds.
5. PASS — PowerShell set RUSTDOCFLAGS=-D warnings then cargo doc --workspace --no-deps; exit 0, generated workspace docs.
6. PASS — cargo deny check --warn vulnerability --warn unsound --warn unmaintained --warn notice --warn yanked; exit 0, advisories ok, bans ok, licenses ok, sources ok. cargo tree -p racc-proto -e normal printed only racc-proto v0.1.0.
7. PASS — scripts/check-layering.ps1 and Git Bash scripts/check-layering.sh exited 0 with “Layering check passed for 13 library crates.” scripts/check-all.ps1 and Git Bash scripts/check-all.sh exited 0 and included clean cargo-deny results.
8. PASS — cargo check --workspace --target x86_64-pc-windows-msvc and cargo check --workspace --target x86_64-apple-darwin both exited 0. Label: COMPILE-ONLY.
9. PASS — constants_match_v0_wire_limits test asserts all eleven specified constants; maximal video datagram test asserts 1200 bytes.
10. PASS — all 16 control types have round-trip or golden tests and byte layouts in docs/PROTOCOL.md. Required golden vectors in tests and documentation match.
11. PASS — docs/PROTOCOL.md, ADRs 0009–0011, docs/OPEN_QUESTIONS.md and this progress report updated. The UDP truncation limitation is recorded as an open question.
12. PASS — all M1 commits use author and committer rkohnmn <275230809+rkohnmn@users.noreply.github.com>, have no attribution trailers, and were pushed with normal git push origin main; final status is main...origin/main.

##### Created or changed

- crates/proto/Cargo.toml and Cargo.lock: added proptest 1.11.0 as the only dev dependency; normal dependency tree remains empty.
- crates/proto/src/lib.rs, codec.rs, error.rs, framing.rs, messages.rs and video.rs.
- crates/proto/tests/golden.rs and robustness.rs.
- docs/goals/M1.md, docs/PROTOCOL.md, docs/OPEN_QUESTIONS.md, and ADRs 0009, 0010 and 0011.
- docs/PROGRESS.md with this M1 report.

##### Control message encoded size ranges

Sizes include the four-byte TCP length prefix, type byte, and payload.

| Type | Message | Minimum | Maximum |
|---:|---|---:|---:|
| 1 | Hello | 21 | 277 |
| 2 | HelloAck | 21 | 277 |
| 3 | TopologyAnnounce | 14 | 2510 |
| 4 | SwitchMonitor | 13 | 13 |
| 5 | StreamReset | 26 | 26 |
| 6 | SetQuality | 11 | 11 |
| 7 | RequestKeyframe | 7 | 7 |
| 8 | PauseVideo | 5 | 5 |
| 9 | ResumeVideo | 5 | 5 |
| 10 | InputEvent | 14 | 16 |
| 11 | ClipboardUpdate | 15 | 524303 |
| 12 | StatsReport | 25 | 25 |
| 13 | Ping | 21 | 21 |
| 14 | Pong | 21 | 21 |
| 15 | CursorShape | 21 | 65553 |
| 16 | Goodbye | 6 | 6 |

##### UNVERIFIED, COMPILE-ONLY, and open questions

- [COMPILE-ONLY] Both required platform target checks passed; no runtime or hardware behavior is claimed.
- [UNVERIFIED] UDP payload truncation after a valid header cannot be detected without a payload-length field; see open question 13.
- Open questions added for Unicode text input on international layouts, whether mouse movement should move to latest-wins UDP after M7 measurements, maximum frame-size policy after real encoder output, and the video fragment-length ambiguity.

##### M2 starting point

M2 starts from the bounded types, constants, and control framing in racc-proto and their wire contract in docs/PROTOCOL.md. Implement transport-specific UDP slicing/reassembly, pacing, TCP control I/O, and bind-to-interface logic in racc-net, with loss/reorder/jitter tests. No M2 code was started in this session.

### 2026-10-06 -- M2 transport implementation and acceptance report

- [VERIFIED-RUN] Implemented M2 transport in racc-net and deterministic impairment tools in racc-testkit on Windows 10.0.19045 (Windows PC #1), 2026-10-06.
- [TESTED-FAKE] Implemented a deterministic sans-I/O core using injected microsecond time, plus bounded std-thread/std::net/socket2 I/O workers: UDP slicing/reassembly, ordered latest-wins delivery with keyframe gap safety, retry backoff, bounded loss estimation, sender pacing/backpressure, framed TCP control I/O, and Tailscale-only bind validation.
- [VERIFIED-RUN] The seeded virtual network covers iid and Gilbert–Elliott burst loss, reorder, duplication, delay/jitter, bitrate limits and finite queues. The real loopback proxy test concurrently collects receiver events and is serialized against the CPU-heavy soak test; the 300-frame zero-loss assertion remains intact.
- [VERIFIED-RUN] The temporary test-bind dependency was added to racc-app for the negative gate check; both feature scripts rejected it with exit 1 and crates/app/Cargo.toml was restored byte-for-byte.
- [UNVERIFIED] No Tailscale daemon, second PC, Mac, GPU capture, encode or decode path was exercised. Windows/macOS target checks are compile-only and do not prove runtime behavior.

#### Final acceptance report

1. **PASS** — cargo fmt --all -- --check exited 0 in both final check-all runs; no formatting output.
2. **PASS** — cargo clippy --workspace --all-targets -- -D warnings exited 0 in both final check-all runs; finished with no warnings.
3. **PASS** — cargo test --workspace exited 0 in both final check-all runs. Counts: racc-net 30 passed, 0 failed, 1 ignored manual release benchmark; racc-proto 20 integration tests passed (10 golden, 10 robustness; library harness 0); racc-testkit 6 passed. The other 12 workspace crates each had 0 tests. Total: 56 passed, 0 failed, 1 ignored; doc-test harnesses had 0 tests.
4. **PASS** — $env:PROPTEST_CASES='10000'; cargo test -p racc-net exited 0: 28 passed, 0 failed, 1 ignored; 10,000 generated cases were requested for property tests.
5. **PASS** — $env:RUSTDOCFLAGS='-D warnings'; cargo doc --workspace --no-deps exited 0 and generated workspace documentation.
6. **PASS** — cargo deny check --warn vulnerability --warn unsound --warn unmaintained --warn notice --warn yanked exited 0: advisories ok, bans ok, licenses ok, sources ok. cargo tree -p racc-net -e normal showed racc-proto and socket2, with socket2 platform dependencies only.
7. **PASS** — scripts/check-layering.ps1 and scripts/check-layering.sh passed for 13 library crates. scripts/check-features.ps1 and scripts/check-features.sh passed for racc-app and racc-host-agent. scripts/check-all.ps1 and scripts/check-all.sh both exited 0. In the negative test each feature script exited 1 with forbidden test-bind identified in racc-app; the original app manifest was restored byte-for-byte.
8. **PASS — COMPILE-ONLY** — cargo check --workspace --target x86_64-pc-windows-msvc and cargo check --workspace --target x86_64-apple-darwin both exited 0; type-check results only.
9. **PASS** — D5–D9 constants are documented in docs/TRANSPORT.md and asserted by racc-net tests m2_constants_match_the_transport_contract, m2_socket_and_thread_constants_match_the_transport_contract, and m2_control_defaults_match_the_transport_contract. racc-proto asserts MAX_DATAGRAM, header and fragment limits. Control defaults asserted: 10 s keepalive, 2 s write/connect timeout, 5 s read timeout.
10. **PASS** — The 600-second virtual soak passed all three quality tiers at 0%, 0.5%, 1%, 2% and 5% iid loss plus Gilbert–Elliott burst loss. It asserts gap safety, bounded memory, >=200 ms request spacing, clean zero-loss delivery and stale-picture bound.
11. **PASS** — TCP loopback round-tripped all 16 control types and passed oversized-prefix, byte-at-a-time, peer-close-mid-frame, timeout and socket-option checks. UDP loopback delivered 300/300 byte-identical ordered frames at 0% loss with zero requests. At seeded 2% loss, proxy forwarded 2027 of 2061 and dropped 34; 64 frames were delivered in order, 25 partial frames and one frame gap were counted, and 17 keyframe requests were emitted. Sender/receiver/proxy close and join assertions stayed below 1 second.
12. **PASS** — docs/TRANSPORT.md contains architecture, state transitions, constants, pipeline, bind policy, counters and measurements A–D. docs/PROTOCOL.md records uniform 1182-byte fragmentation in place without a protocol version bump and the UDP/WireGuard boundary assumption. ADRs 0012–0014 exist; question 13 is resolved; questions 14–16 and M2/M7/Mac hardware checks are recorded.
13. **PASS** — Small logical commits were pushed to origin/main with normal git push, no force push. M2 commits: bafea84, 3cc20fd, b0fa66d, ebeac3b, 56a983f and 556b5a6; the final progress-report commit is also pushed normally. Local Git identity is rkohnmn <275230809+rkohnmn@users.noreply.github.com>; author and committer match and no attribution trailers are present. The only untracked path is the original user-supplied docs/goals/GOAL_M2.md, preserved unchanged; canonical docs/goals/M2.md is committed.

#### Measurements A -- ten-minute deterministic delivery simulation

[VERIFIED-RUN] Each profile simulated 600 seconds at 30 fps (18,000 frames), with the seeded network, finite 4 MiB queue and keyframe response after modeled feedback plus encode delay. Stale picture means time frozen by more than one frame interval.

| Tier | Loss profile | Frames delivered | Keyframe requests/min | Recovery median / p95 (ms) | Stale picture | Peak retained payload |
|---|---:|---:|---:|---:|---:|---:|
| 480p30, 1.5 Mbps | 0% iid | 100.000% | 0.00 | — | 0.001% | 49,644 B |
| 480p30, 1.5 Mbps | 0.5% iid | 8.278% | 61.70 | 963.64 / 3,673.65 | 80.118% | 54,728 B |
| 480p30, 1.5 Mbps | 1% iid | 3.639% | 66.20 | 929.34 / 2,886.60 | 85.620% | 59,796 B |
| 480p30, 1.5 Mbps | 2% iid | 1.183% | 67.20 | 1,281.23 / 6,551.91 | 86.182% | 65,366 B |
| 480p30, 1.5 Mbps | 5% iid | 0.156% | 63.00 | 9,819.12 / 41,086.57 | 77.240% | 63,682 B |
| 720p30, 3.5 Mbps | 0% iid | 100.000% | 0.00 | — | 0.001% | 115,836 B |
| 720p30, 3.5 Mbps | 0.5% iid | 1.761% | 62.20 | 1,590.10 / 2,678.18 | 93.378% | 143,421 B |
| 720p30, 3.5 Mbps | 1% iid | 0.283% | 62.50 | 3,333.46 / 7,265.79 | 94.873% | 156,468 B |
| 720p30, 3.5 Mbps | 2% iid | 0.139% | 61.70 | 7,724.45 / 17,769.12 | 92.100% | 155,286 B |
| 720p30, 3.5 Mbps | 5% iid | 0.000% | 60.50 | no recovery observed | 99.999% | 155,286 B |
| 1080p30, 7 Mbps | 0% iid | 100.000% | 0.00 | — | 0.001% | 231,672 B |
| 1080p30, 7 Mbps | 0.5% iid | 0.372% | 61.00 | 2,216.26 / 10,397.55 | 96.466% | 286,134 B |
| 1080p30, 7 Mbps | 1% iid | 0.000% | 60.40 | no recovery observed | 99.999% | 312,936 B |
| 1080p30, 7 Mbps | 2% iid | 0.000% | 60.40 | no recovery observed | 99.999% | 314,118 B |
| 1080p30, 7 Mbps | 5% iid | 0.000% | 60.20 | no recovery observed | 99.999% | 312,936 B |
| 720p30 burst | Gilbert–Elliott | 7.144% | 58.90 | 928.94 / 3,585.16 | 88.210% | 128,484 B |

#### Measurement B -- real sender pacing

[VERIFIED-RUN] Windows 10.0.19045 loopback sent a 233,328-byte keyframe in 198 datagrams (236,892 bytes including headers). Measured duration was 20,167 us against a 19,999 us target, exceeding it by 168 us. Maximum adjacent-datagram gap was 1,067 us; maximum observed sleep call was 1,063 us. This is one Windows run; Mac timer behavior is not measured.

#### Measurement C -- reassembler rate and memory

[VERIFIED-RUN] Release-mode micro-run processed 100,000 one-fragment datagrams in 29.088 ms: 3,437,844 datagrams/second on one worker thread on Windows PC #1. Peak in-flight payload during the soak was 314,118 bytes, below the 4 MiB budget. This is a local CPU micro-run, not an end-to-end stream-rate guarantee.

#### Conclusion D -- loss recovery choice

At tested 0.5% iid loss, v0 recovery is already unacceptable for interactive viewing: only 8.278% of 480p frames, 1.761% of 720p frames and 0.372% of 1080p frames were delivered; a 233 KB keyframe needs about 198 fragments. Since the adjacent measured point is 0%, the simulations do not establish a more precise failure threshold between 0% and 0.5%. Evaluate selective requests for missing keyframe fragments before M7; compare XOR parity only if measured retransmission delay is too high. No NACK or FEC was implemented in M2.

#### Created or changed -- M2

- crates/net: bind policy, typed errors, bounded loss estimator, UDP slicer/reassembler, sender pacer/queue and real UDP/TCP socket workers.
- crates/testkit: seeded xorshift impairment simulator, finite queue/rate limiting, loopback proxy, 10-minute profile soak and real 300-frame loopback tests.
- Cargo.lock, crate manifests, deny.toml, scripts/check-features.ps1 and .sh, check-all script integration, and AGENTS.md script inventory.
- docs/goals/M2.md, docs/TRANSPORT.md, docs/PROTOCOL.md, docs/OPEN_QUESTIONS.md, docs/HARDWARE.md, docs/DEV_SETUP.md and ADRs 0012–0014.

#### UNVERIFIED, COMPILE-ONLY and HUMAN-PENDING

- [TESTED-FAKE] Transport policy, impairment behavior and loopback sockets were exercised locally. This does not establish behavior on a tailnet or another OS.
- [COMPILE-ONLY] Windows MSVC and Intel macOS target checks passed; they are not runtime or hardware verification.
- [HUMAN-PENDING] docs/HARDWARE.md retains a conditional PC #1 loopback rerun if firewall or driver conditions differ, a two-Windows-PC Tailscale sender/viewer test after Tailscale is installed on both, and the 2015 Intel Mac pacing measurement.
- [UNVERIFIED] No capture/GPU/video-codec path was exercised in M2. No real Tailscale direct/DERP loss or Mac sleep granularity was measured.

#### Open questions added or resolved

- Question 13 is resolved with uniform 1182-byte non-final fragments and the documented v0 truncation limitation.
- Question 14 asks whether M7 should first evaluate selective NACK or XOR parity; measured results recommend selective keyframe-fragment NACK evaluation, but neither is implemented.
- Question 15 remains HUMAN-PENDING for the 2015 Intel Mac sleep pacing result.
- Question 16 remains HUMAN-PENDING until two Windows PCs stream across Tailscale and record direct/DERP, loss and recovery.

#### M3 starting point

M3 starts from the existing racc-topology, racc-session and racc-telemetry workspace stubs and the v0 display/control types in racc-proto. Implement display modeling and topology diffs, monitor-switch state and epoch handling with fake capture/encoder backends, coordinate math, counters and event types. Add the listed switch/loss/removed-display/rapid-switch/coordinate tests. No M3 work was started.

### 2026-10-06 — M2.5 transport audit; acceptance complete

- [VERIFIED-RUN] Completed the analytic model, deterministic trace, corrected default-link model, constrained scenarios, 8x/4x matrices, and simulator-only NACK/FEC comparison. `docs/TRANSPORT_AUDIT.md` and `docs/TRANSPORT.md` contain the seeded results and recommendation. No M3 work was started.
- [VERIFIED-RUN] The corrected 600-second default-link regressions passed for 480p/720p at 0.1%, 0.25% and 0.5% IID loss; the old 102%-bitrate default was temporarily restored and the no-tail-drop regression failed as expected, then the correction was restored and passed. The stale-picture accumulation regression also failed with the old cap and passed after fixing the additive accounting.
- [VERIFIED-RUN] Corrected headline delivery/stale values: 480p at 0.5% = 89.172% / 10.9744%, at 1% = 70.578% / 29.5388%, at 2% = 29.067% / 70.9794%; 720p at 0.5% = 61.939% / 38.1823%, at 1% = 22.117% / 77.9260%, at 2% = 2.383% / 97.6169%.
- [VERIFIED-RUN] The 720p/2%/1ms hybrid NACK-all+FEC-20 simulator row delivered 100% with 1.389% stale time, 8.742% overhead and 1.538ms added median latency, compared with baseline 2.467% delivered and 95.550% stale. At 1080p/2%/20ms its stale value was 17.875%; no all-tier 2% WAN claim is supported. Recommendation: evaluate the hybrid in M7 after real-path measurements; implement neither mechanism in `racc-net` or `racc-proto` now.
- [VERIFIED-RUN] `cargo fmt --all -- --check` exited 0 after formatting the revised loopback test. `cargo clippy --workspace --all-targets -- -D warnings` exited 0. `$env:PROPTEST_CASES='10000'; cargo test -p racc-net` exited 0: 30 passed, 0 failed, 1 ignored; doc tests 0.
- [VERIFIED-RUN] The first three loopback attempts exposed one real-path datagram loss and an invalid unconditional zero-request assertion; question 18 retains the historical counts and explains the correction. The successful final test condition distinguishes a clean run from observed UDP loss, and the proxy now reports successful sends separately from send errors. The corrected loopback test passed in the final standalone workspace run and both `check-all` scripts.
- [VERIFIED-RUN] `cargo test --workspace` and both check-all scripts passed on the final tree. Latest counts: racc-net 32 passed/1 ignored; racc-proto 10 golden + 10 robustness passed; racc-testkit 15 passed; every other workspace and doc-test harness had 0 tests. The shell check-all returned exit 0; PowerShell reached its final cargo-deny success output without an error.
- [VERIFIED-RUN] The report includes Phase 1 formulas and prediction tables, a 20-loss trace excerpt, H1-H7 verdicts, the root cause, full corrected Phase 4 results and seeds, all Phase 5 mechanisms/scenarios/metrics, and the recommendation. Corrected v0 meets the stated >=90% delivered and <=10% stale criterion at <=0.1% IID loss across tiers; 480p additionally passes at 0.25%. No real Tailscale or hardware limits are inferred.
- [VERIFIED-RUN] No protocol/wire change was made. NACK/FEC models remain under `racc-testkit`; no NACK/FEC implementation was added to `racc-net` or `racc-proto`. The hardware edit is limited to the requested [OWNER-REPORTED] Late 2015 MacBook statement and [UNVERIFIED] model inference; no other hardware entries were changed.
- [HUMAN-PENDING] Hardware checklist items and the owner's Mac model confirmation remain pending. The M3 starting point remains `racc-topology`, `racc-session`, and `racc-telemetry` stubs with protocol display/control types; implement only M3's topology, switching, coordinate math and telemetry state machines after this audit is accepted. Do not start M3 until M2.5 is accepted and the required push disposition is resolved.

#### M2.5 final acceptance report

1. PASS — `cargo fmt --all -- --check`; exit 0 on the final edited tree.
2. PASS — `cargo clippy --workspace --all-targets -- -D warnings`; exit 0 on the final edited tree.
3. PASS — Offline `cargo test --workspace` exited 0 in the final run and in both `check-all` scripts. Counts: racc-net 32 passed/1 ignored; racc-proto 20 passed; racc-testkit 15 passed; all other unit/doc harnesses had 0 tests.
4. PASS — `$env:PROPTEST_CASES='10000'; cargo test -p racc-net`; exit 0, 30 passed, 1 ignored.
5. PASS — Offline `scripts/check-all.ps1` and `scripts/check-all.sh` both completed successfully. They include fmt, clippy, full workspace tests, layering, feature gates, and cargo-deny; the shell script returned exit 0.
6. PASS — `cargo check --workspace --target x86_64-pc-windows-msvc --offline` and `cargo check --workspace --target x86_64-apple-darwin --offline` both exited 0. COMPILE-ONLY; no platform runtime claim.
7. PASS — formulas, prediction tables, trace excerpt, H1-H7 verdicts and root-cause statement are in `docs/TRANSPORT_AUDIT.md`.
8. PASS — regression evidence for the confirmed simulation-link and stale-time accounting defects is recorded in the audit; each test was observed failing with the old behavior and passing after the fix. Keyframe counter and stale-partial regressions pass in `racc-net`; the proxy send-result accounting regression also passes.
9. PASS — the unconstrained default, separately labeled constrained scenarios, and the factor-of-two envelope regressions are documented and passed.
10. PASS — corrected Phase 4 matrix, recorded seeds, and clearly superseded M2 tables are in `docs/TRANSPORT.md`.
11. PASS — all simulator-only recovery models, scenarios, overhead and latency metrics, and a numeric recommendation are documented.
12. PASS — no `racc-proto` or wire-format change; no NACK/FEC code in `racc-net`; models are in `racc-testkit`.
13. PASS — the requested Mac statement/inference is recorded; other hardware entries were left untouched.
14. PASS — local commit identity is `rkohnmn`; commits `8ebab12` (code/models/regressions), `7d72c07` (audit/docs), `beaf850` (acceptance status), `ee5d68e` (trace cleanup), `eac92b1` (proxy send accounting), `39d4354`, `979a2e9`, and `c49b04b` (final local evidence) were authored locally. After the user explicitly authorized overriding the no-network constraint for this required action, `git push origin HEAD:main` completed as a non-forced fast-forward from `d32a27d` to `c49b04b`. The final acceptance-status documentation was subsequently published with a second normal, non-forced fast-forward.

#### Root cause and hypothesis verdicts

The M2 result was distorted by an under-specified simulated link capped at 102% of the mean video bitrate, which left large keyframes serialized behind a deep queue; stale-picture accumulation also incorrectly capped cumulative freeze time. The corrected model removes the implicit bottleneck and computes stale intervals additively. The historical real-loopback failure was a separate OS UDP loss in a nominally loss-free localhost run; its exact send/receive stage remains inconclusive because the proxy counter at the time counted attempts, not successful writes. The proxy now distinguishes successful sends and send errors, and the corrected final loopback test passes.

| Hypothesis | Verdict | Evidence summary |
|---|---|---|
| H1 link capacity drove loss/staleness | CONFIRMED | Legacy seed 5716 had 18,174 queue drops and peak queue 4,194,302 B; corrected default uses >=50 Mbps and a 64 MiB queue. |
| H2 keyframe blocked by ordering/watermark/epoch | REFUTED | Trace shows correct epoch and keyframe completion/delivery; ordering holds do not explain the sustained low delivery. |
| H3 keyframe evicted/timed out/corrupted | REFUTED | Keyframe counters and trace show no timeout, cap eviction, or inconsistency. |
| H4 keyframe state/backoff failed to reset | REFUTED | Trace shows request state clears after keyframe delivery; the regression confirms an older partial cannot re-arm it. A new loss episode may start a fresh immediate request. |
| H5 sender response malformed, discontinuous, suppressed, or delayed | CONFIRMED (delay only) | The response uses KEY and CONFIG with continuous frame IDs and is fully delivered; the first-fragment and full-keyframe delays are attributable to the constrained link queue. |
| H6 delivery/stale metrics wrong | CONFIRMED | Stale intervals were incorrectly capped; old implementation failed the 666,667us regression, corrected additive total passed. |
| H7 current policy alone explains measurements | REFUTED | Corrected unconstrained results fit the analytic envelope at the required low-loss points. |

## M3a — topology, coordinate math, and telemetry (2026-10-06)

M3a implementation and pure-logic verification are complete in this worktree. No M3b session state machine work was started. The protocol and transport crates were not changed.

### Acceptance report

1. **PASS** — `cargo fmt --all -- --check`; exit 0, no formatting diff.
2. **PASS** — `cargo clippy --workspace --all-targets -- -D warnings`; exit 0, `Finished dev profile` with no warnings.
3. **PASS** — `cargo test --workspace --offline`; exit 0. Unit/integration totals: racc-net 32 passed / 1 ignored; racc-proto 20 passed; racc-telemetry 14 passed; racc-testkit 15 passed; racc-topology 28 passed; racc-app, racc-capture, racc-clipboard, racc-core, racc-decode, racc-encode, racc-host-agent, racc-identity, racc-input, and racc-session each 0 tests. Total: 109 passed, 1 ignored, 0 failed; doc-test harnesses 0 failures. The ignored test is the manual release-mode M2 reassembly benchmark.
4. **PASS** — `PROPTEST_CASES=10000 cargo test -p racc-topology -p racc-telemetry --offline`; exit 0, telemetry 14 passed and topology 28 passed. Every proptest ran with 10,000 cases. Telemetry property run took 25.42 s; topology property run took 0.47 s.
5. **PASS** — `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --offline`; exit 0, generated the workspace docs.
6. **PASS** — standard `cargo deny check --warn vulnerability --warn unsound --warn unmaintained --warn notice --warn yanked`; output: `advisories ok, bans ok, licenses ok, sources ok`. Normal dependency trees for both M3a crates show only `racc-proto`.
7. **PASS** — `check-layering.ps1` and `check-features.ps1` passed; Git Bash versions of both `.sh` checks passed. `check-all.ps1` completed with exit code 0 and `check-all.sh` completed with exit code 0; both ran workspace checks, soak tests, layering/features, and cargo-deny. Git Bash was invoked from `C:\Program Files\Git\bin\bash.exe` because Bash is not on PATH. Its feature script emitted only the existing Cargo `--all-targets` deprecation notices.
8. **PASS — COMPILE-ONLY** — `cargo check --workspace --target x86_64-pc-windows-msvc --offline` and the corresponding `x86_64-apple-darwin` command both exited 0. Neither establishes real OS input behavior.
9. **PASS** — explicit coordinate tables cover single display, negative-origin side-by-side, stacked, mixed-DPI, nonzero virtual origin, 480p/720p on 1080p/4K, letterbox/pillarbox, edge and one-pixel displays, macOS point mapping, and all four Windows virtual-desktop corners for two- and three-wide layouts. Generated properties cover host-pixel bounds and per-axis monotonicity, Windows absolute bounds/monotonicity, letterbox fit/centering/aspect, normalization error, topology diffs, and proto round trips.
10. **PASS** — stable-id canonical-byte vectors and FNV ids are asserted in `identity.rs` and documented in `TOPOLOGY.md`. The negative-origin TopologyAnnounce length-prefixed golden is asserted in `domain.rs` and reproduced byte-for-byte in the topology guide.
11. **PASS** — `TOPOLOGY.md`, `TELEMETRY.md`, ADRs 0016–0018, the M3a objective copy, hardware checklist, progress report, and open questions are present. ADR 0015 already records an accepted M2.5 decision, so the M3a topology/coordinate/telemetry ADRs use 0016/0017/0018 rather than overwrite it. The v0 `StatsReport` has no decoder field; `DecoderKind` has a safe local reserved-code map and host conversion leaves it `Unknown`. Both constraints are recorded in `OPEN_QUESTIONS.md`; no proto change was made.
12. **PASS** — `git push origin HEAD:main` completed as an authorized, non-forced fast-forward, publishing M3a implementation and documentation commits `b0c1fee`, `3cc3bf3`, and `cb606f6`; remote `main` advanced from `c144d11` to `cb606f6`. This evidence report was committed and published afterward by a separate normal fast-forward. No force push or workaround was used.

### Created and changed

- Implemented `racc-topology`: validated nonzero display ids and bounded topology/proto conversion; serial revision updates; stable FNV-1a display identity with deterministic collision probing; exact topology diffs and stream-reset predicate; checked virtual bounds; integer letterbox, pointer, host-pixel, Windows absolute, macOS point, and cursor mapping.
- Implemented `racc-telemetry`: bounded time-bucket rates; RFC 6298-style RTT/SRTT/RTTVAR and jitter; a 16-entry ping tracker; unknown-safe backend/encoder code conversion; session/host snapshots; bounded event ring and sans-I/O hub.
- Added property and table tests, including 10,000-case runs, golden identity vectors, and the negative-origin wire frame.
- Added `docs/TOPOLOGY.md`, `docs/TELEMETRY.md`, ADRs 0016–0018, three M3a coordinate checks to `HARDWARE.md`, M3a follow-ups to `OPEN_QUESTIONS.md`, and saved the supplied objective at `docs/goals/M3a.md`.
- Updated both crate manifests and `Cargo.lock`. No changes were made to `racc-proto`, `racc-net`, `docs/PROTOCOL.md`, session state machines, or runtime platform input code.

### Three-display coordinate evidence

For three `1920x1080` displays at `(-1920,0)`, `(0,0)`, `(1920,0)`, virtual bounds are `(-1920,0,5760,1080)`. The four corners map to Windows absolute `(0,0)`, `(65535,0)`, `(0,65535)`, `(65535,65535)`. Normalized center `(32768,32768)` maps to host physical pixels `(-960,540)`, `(960,540)`, `(2880,540)` on the left, middle, and right displays. Those map to Windows absolute `(10924,32798)`, `(32773,32798)`, and `(54622,32798)` respectively. These are arithmetic test results; Windows injection behavior remains HUMAN-PENDING.

### Verification limits and open questions

- **[VERIFIED-RUN]** Pure logic and generated tests ran on Windows 10.0.19045. They do not verify real monitor metadata or OS cursor injection.
- **[COMPILE-ONLY]** Windows and Intel macOS workspace checks passed; platform behavior was not run.
- **[HUMAN-PENDING]** Windows corner/center readback and DPI comparison, plus Mac built-in/external display checks, are listed in `HARDWARE.md`.
- The M2.5 simulator hybrid NACK/FEC recommendation was later superseded by the fixed v0 scope recorded in questions 14 and 17. If M7 selects a separately authorized recovery mechanism, telemetry may need bounded retransmit, recovery, and stale-frame counters (question 21).
- Added questions 19–23 for ADR numbering, missing StatsReport decoder field, M7 transport telemetry, the truncated supplied final instruction, and the rejected push review.

### M3b handoff

M3b should implement viewer and host session lifecycle state machines using fake capture/encoder dependencies, topology diffs, serial topology revisions, and stream epochs. It should cover connect/handshake, monitor switching, pause/resume, reconnect and recovery transitions with stale-epoch handling. This is only the high-level scope already stated in `AGENTS.md`; the supplied M3a request was truncated during its final M3b paragraph. No M3b code was started.

### 2026-10-06 — M3b host and viewer session lifecycle

- [VERIFIED-RUN] Implemented deterministic host and viewer lifecycle controllers in `racc-session`. They exchange typed protocol, capture, encoder, renderer-disposition, and event actions; no sockets, capture API, decoder, or UI dependency was added.
- [VERIFIED-RUN] Host transitions cover H.264 handshake validation, initial stream setup, monitor switching and topology updates, pause/resume, capture loss and retry, network-path quality signals, encoder rebuild with OpenH264 fallback and terminal failure events, Goodbye, and the owner-selected five-second control-disconnect timeout. Viewer transitions cover handshake/topology, switch request correlation, held-frame replacement on a matching keyframe, pause/resume across reconnect, packet-loss and decoder recovery, terminal encoder-failure events, bounded reconnect backoff, and close.
- [VERIFIED-RUN] Encoder configuration and active StreamReset metadata use matching aspect-preserving output dimensions within 1920×1080. The helper raises sources to 480 pixels high where the bounds allow it; an extreme aspect ratio can remain below 480 to preserve aspect within the width cap. Actual bitrate and tier selection remain with the host quality controller.
- [VERIFIED-RUN] Capture retries follow 50, 100, 200, 400, 800, then 1000 ms delays. Epoch and request checks discard stale completions, stale resets, and stale video frames. Textual wire protocol was unchanged; v0 loss recovery remains the existing rate-limited `RequestKeyframe` path without NACK or FEC.
- [VERIFIED-RUN] `docs/SESSION.md` documents the host/viewer contract. The owner-selected disconnect timeout is recorded in `docs/OPEN_QUESTIONS.md`; the truncated M3a-to-M3b handoff is resolved using only the high-level M3 scope in `AGENTS.md`.

#### M3b acceptance report

1. **PASS** — `cargo fmt --all -- --check`; exit 0.
2. **PASS** — `cargo clippy --workspace --all-targets -- -D warnings`; exit 0.
3. **PASS** — `cargo test --workspace`; 154 passed, 0 failed, 1 ignored. Counts: `racc-session` 45, `racc-net` 32, `racc-proto` 20, `racc-telemetry` 14, `racc-testkit` 15, and `racc-topology` 28; remaining unit and doc-test harnesses had no failures.
4. **PASS** — `cargo test -p racc-session`; 45 passed, 0 failed.
5. **PASS** — `scripts/check-layering.ps1`; 13 library crates passed.
6. **PASS — COMPILE-ONLY** — `cargo check --workspace --target x86_64-pc-windows-msvc` and `cargo check --workspace --target x86_64-apple-darwin`; both exited 0. These checks establish compilation only.
7. **PASS** — `git diff --check`; exit 0.

#### Verification limits

- [TESTED-FAKE] State transitions were exercised using deterministic operation results and test fixtures; this does not verify a real capture backend or hardware encoder/decoder.
- [COMPILE-ONLY] Windows and macOS target checks passed; neither establishes platform runtime behavior.
- [HUMAN-PENDING] Hardware capture/encode, GPU recovery, real monitor hot-switch, service/session transitions, keyboard input, real Tailscale direct/DERP paths, and end-to-end stream latency remain on `docs/HARDWARE.md`.
### 2026-10-06 — M4a UI toolkit spike

- [VERIFIED-RUN] Compared iced 0.14.0 and Slint 1.18.1 with isolated native Windows prototypes, synthetic 1920×1080 NV12 updates at the required 30 Hz, and telemetry at 4 Hz. Source, run commands, logs, dependency notes, and limits are in `spikes/m4a-iced/` and `spikes/m4a-slint/`.
- [VERIFIED-RUN] Selected iced for M4b: its custom shader uses the same wgpu renderer and render pass and samples NV12 directly; active uploads averaged 29.97 Hz with 33.372 ms mean, 37.420 ms p95, and 96.918 ms max intervals. Its telemetry intervals averaged 249.944 ms (4.00 Hz). Slint's exact-cadence run averaged 29.70 Hz for active video intervals (33.67 ms mean, 34.68 ms p95, 60.23 ms max) and 3.93 Hz telemetry; the tested imported-image path also needs an intermediate RGBA conversion texture.
- [VERIFIED-RUN] Both stand-alone spike packages passed formatting/compile checks as recorded in their READMEs, and both completed visible 60-second native runs. The root app/workspace dependency graph was not changed.
- [VERIFIED-RUN] Framework render callbacks and upload intervals do not measure physical monitor presentations. The final iced run recorded a 159.509 ms maximum active upload interval; the owner watched the stream and reported no visible stutter.
- [HUMAN-VERIFIED — OWNER] The owner watched the final 60-second iced run on Windows PC #1 and reported no visible stutter. M4a is complete; M4b may proceed.
#### M4a automated acceptance checks

1. **PASS** — `cargo fmt --all -- --check`.
2. **PASS** — `cargo clippy --workspace --all-targets -- -D warnings`.
3. **PASS** — `cargo test --workspace`; 154 passed, 0 failed, 1 ignored. The ignored test is the manual release-mode M2 reassembly benchmark.
4. **PASS** — Both isolated spike crates passed `cargo fmt --manifest-path … -- --check`, `cargo check --manifest-path …`, and `cargo clippy --manifest-path … --all-targets -- -D warnings`.
5. **PASS — MEASURED-RUN** — Both 60-second native synthetic-stream windows completed on Windows 10.0.19045. Exact inputs, run output, interval statistics, and limits are in each spike README and ADR 0001.
6. **PASS — HUMAN-VERIFIED** — On 2026-10-06, the owner watched the final 60-second iced synthetic stream on Windows PC #1 and reported no visible stutter. Physical present cadence was not independently measured; counters and limitations are recorded in ADR 0001 and the spike README.

### 2026-10-07 — M4b fake UI shell

- [VERIFIED-RUN] Implemented the `racc-core` metadata-only UI bus and separate `FrameSource`/`FrameSink` frame handoff, deterministic `racc-testkit::FakeCore`, pure app view-model reducer, iced native-wgpu shell, tokenized layout, fake home/settings flows, focus traversal, and no-op tray interface. No real networking, capture, decode, clipboard transport, or input forwarding was added.
- [VERIFIED-RUN] The release `racc-app --fake` window opened, emitted its measurement summary, and exited with status 0 in the open, collapsed, and idle runs. Logs confirm the mode ran; no screenshot was available to the agent, so visual layout review remains HUMAN-PENDING.
- [TESTED-FAKE] Display and device switching retain the old frame through the 150 ms fake delay and matching StreamReset/StreamStarted events; the separate latest-frame handoff changes only when the matching DecoderReady event is polled. Quality reset follows the same frame gate. Remote Desktop enables keyboard and mouse capture together; quality selection and stream dimensions update in the reducer.
- [VERIFIED-RUN] `scripts/check-all.ps1` completed workspace formatting, clippy, tests, layering, feature-gate, and cargo-deny phases successfully. The full workspace tests passed (including the long transport soak); app 10, core 6, and testkit 27 tests passed. `cargo deny` allowed the observed BSL-1.0, Zlib, CC0-1.0, ISC, and Unicode-3.0 dependencies; advisories report only existing duplicate-version and unmaintained-crate warnings.
- [VERIFIED-RUN] Core dependency tree contains only proto, topology, telemetry, and session beneath `racc-core`; it has no `racc-app` dependency. Search `rg -ni discord crates` returned no matches. Audio remains disabled as a “not supported” placeholder, and there is no GPU usage telemetry field.
- [VERIFIED-RUN] Release measurements and methods are in `docs/UI.md`: open sidebar 1,682 frame-ID cadence samples (33.20 ms median, 34.96 ms p95, 20 >40 ms), 0.046 CPU-s/s, 312.7/312.8 MiB private mean/peak; collapsed sidebar 1,711 samples (33.26/34.93 ms, 20 >40 ms), 0.046 CPU-s/s, 311.1/311.2 MiB; fake idle 0.009 CPU-s/s and 305.4/305.5 MiB. Cadence is a frame-source observation proxy, not physical presentation.
- [VERIFIED-RUN] `docs/UI.md`, `docs/HARDWARE.md`, `docs/goals/M4b.md`, `docs/OPEN_QUESTIONS.md`, ADR 0001, `deny.toml`, and this progress report were updated. The absent `docs/goals/COMMON.md` remains resolved per owner authorization to use AGENTS and prompt acceptance checks.

#### M4b acceptance report

1. **PASS — TESTED-FAKE** — View-model reducer and layout tests pass; fake scenario tests cover seeded event order, authorization, discovery, 30 fps frame source, telemetry, 150 ms switching, decoder-ready frame replacement, and quality reset.
2. **PASS — TESTED-FAKE** — `racc-core` builds and tests without `racc-app`; `scripts/check-layering.ps1` passed for all 13 library crates.
3. **PASS — VERIFIED-RUN** — Release fake app launched and exited normally for the requested measurement modes. Visual layout and control review remains HUMAN-PENDING because no agent screenshot was captured.
4. **PASS — VERIFIED-RUN** — Open/collapsed 60-second source-frame measurements, idle CPU/memory, sampling windows, and limits are documented in `docs/UI.md`; M4a figures are compared with their distinct measurement method.
5. **PASS — VERIFIED-RUN** — Search found no Discord branding in `crates`; audio is a disabled placeholder and GPU-usage telemetry is absent.
6. **PASS — VERIFIED-RUN** — UI and milestone docs are present, including the owner checklist in `docs/HARDWARE.md` and known screen-reader labeling limitation.
7. **HUMAN-PENDING** — Owner to launch `cargo run -p racc-app -- --fake`, review visual regions, hover/selected states, collapsible sidebars, resize/drag behavior, keyboard traversal and Escape, telemetry readability/jank, disabled audio, and accessibility. Record requested iterations before M4b is complete. Stop here; do not begin M5.

#### M4b follow-up — 2026-10-07

- [VERIFIED-RUN] Added a focusable header monitor selector with available-display choices, compact tooltip controls for the collapsed 48 px device sidebar, and an explicit `Connect` user action for Home. The Home action now emits `UiCommand::Connect`; selection actions remain distinct.
- [TESTED-FAKE] Added reducer tests for Home Connect command emission and offline rejection, alternate available monitor selection and unavailable-display rejection, and independent lazy view dependencies. Telemetry-only FPS changes retain the rail, device-sidebar, and workspace cache keys; RTT changes update only the workspace and telemetry keys.
- [VERIFIED-RUN] Enabled iced's lazy widget and keyed the four shell regions independently. This caches child view subtrees when their visible dependencies do not change. Root `view`, layout, and full-window redraw still run; the documentation does not claim partial rendering.
- [VERIFIED-RUN] After the Home Connect integration fix, the final `scripts/check-all.ps1` passed formatting, clippy, workspace tests (app 13; core 6; session 45; telemetry 14; testkit 28; topology 28; net 32 plus 1 ignored; proto 20), the 93.49-second testkit suite, 13-library layering, shipped-feature gate, and cargo-deny. Cargo-deny reported existing duplicate-version, `paste`, and `ttf-parser` warnings; advisories, bans, licenses, and sources passed.
- [VERIFIED-RUN] Latest Release samples: open sidebar 1,683 cadence samples (33.23 ms median, 34.97 ms p95, 16 >40 ms), 0.065 CPU-s/s, 307.0/307.1 MiB private mean/peak; telemetry-collapsed 1,683 samples (33.28/34.70 ms, 15 >40 ms), 0.044 CPU-s/s, 304.0/304.9 MiB; fake idle 0.063 CPU-s/s and 305.5/305.7 MiB. Sampling windows and caveats are in `docs/UI.md`; these are source cadence/process proxies, not physical present or GPU measurements.
- [HUMAN-PENDING] Owner visual review remains outstanding. The owner asked us to proceed on best judgment and defer screen review to `blocked.md`; this no longer halts coding.

### 2026-10-07 — Prompt inventory and dependency audit

- [VERIFIED-REVIEW] Confirmed the prompt files present: M3b, M4b, M5a, M5b, M5c, M6, M7, M8, M9, M10, and M11a. M3b is complete; M4b visual review remains pending, and the owner explicitly directed that coding continue without waiting for it.
- [VERIFIED-REVIEW] Read-only audits found M5a, M5c, M6, M7, and M8 are not implemented beyond existing lower-level foundations. Their prompt-specific code, docs, tests, and human checklists remain outstanding as detailed in the prompts.
- [VERIFIED-REVIEW] M6's M2.6 prerequisite is satisfied by the recorded v0 decision to keep incomplete-frame drops plus rate-limited keyframe requests and defer NACK/FEC (`docs/OPEN_QUESTIONS.md` items 14 and 17). M5b is present and has a synthetic-only Media Foundation implementation and probe evidence (see the M5 report) before M6.
- [HUMAN-PENDING] The M4b prompt asks the owner to inspect the UI and interaction checklist in `docs/HARDWARE.md`; the owner cannot inspect screens now. Apply best judgment, keep the review in `blocked.md`, and continue subsequent prompts as explicitly requested.
- [UNVERIFIED] M9, M10, and M11a remain unimplemented and have hardware, packaging, and ordering prerequisites. M9 asks to run on the Mac itself and requires its OS, Tailscale, Screen Recording, and Accessibility checks. M10 still has unfilled license and distribution choices in its prompt and must follow M8 and M9 human checks; M11a must follow M8 and M10.

### 2026-10-07 — M4b review window

- [VERIFIED-RUN] Started `cargo run -p racc-app -- --fake`; the process remains active and the terminal was queued in Codex so the owner can inspect the live window.
- [HUMAN-PENDING] Owner visual and interaction review is still required. The agent-launched window is for review convenience and does not count as owner acceptance.

### 2026-10-07 — M5c identity and discovery follow-up

- [VERIFIED-RUN] Windows PC #1 read-only Tailscale CLI probe: version 1.102.4; status reported 2/2 online peers, aggregate route counts 0 direct/2 DERP, and self-bind validation succeeded. No peer identifiers or raw JSON were recorded.
- [VERIFIED-RUN] Twenty status calls measured approximately 210 ms median / 339 ms p95. Whois succeeded 20/20 times at approximately 217 ms median / 439 ms p95.
- [TESTED-FAKE] Current Node/UserProfile and legacy Machine/User parsing, fixture privacy, and identity crate unit tests were verified; strict all-target clippy passed after removing a redundant PathBuf conversion in the probe example.
- [HUMAN-PENDING] Windows PC #2, Mac live identity checks, and M6 allowlist approval/rejection flow remain pending. M5 remains incomplete.

### 2026-10-07 — M5 capture, encode, and identity follow-up

- [TESTED-FAKE] `cargo test --offline -p racc-capture -p racc-encode -p racc-identity -p racc-input -p racc-clipboard` passed: capture 15, encode 20, identity 27, input 18, clipboard 9; all five doc-test harnesses passed. These are portable/fake tests and do not establish real capture, input, or clipboard behavior.
- [VERIFIED-RUN — SYNTHETIC ONLY] On Windows PC #1, the Media Foundation encoder wrote 90/90 synthetic 1280×720 H.264 Main frames. `ffprobe` found 90 frames and zero B-frames; `ffmpeg` decoded the stream without errors. The elementary stream has no timestamps; 30 fps cadence was not measured. Evidence and command are in `docs/ENCODE.md`.
- [COMPILE-ONLY] Windows capture and encode target checks were previously reported passing. The encoder macOS check is UNVERIFIED because OpenH264's cross build requires an unavailable C++ compiler.
- [VERIFIED-RUN] The read-only M5c Tailscale CLI probe on Windows PC #1 is recorded above; PC #2 and Mac remain HUMAN-PENDING.
- [HUMAN-PENDING] M5 remains incomplete: visible desktop capture and playback were not run; display-mode recovery, adapter affinity, PC #2, and real GPU paths still need owner checks in `docs/HARDWARE.md`.

### 2026-10-07 — M6/M7 implementation foundations

- [TESTED-FAKE] M6 foundations include the pure session supervisor policy, bounded platform-neutral IPC framing/messages, Tailscale allowlist state, and human-run install/uninstall scripts with dry-run support. The scripts were syntax-checked and dry-run only; no service configuration was changed. No SCM adapter, interactive helper launcher, live HostRuntime, named-pipe ACL adapter, or `racc-probe` is present yet.
- [TESTED-FAKE] M7 host input validation/injection controller and viewer input reducer passed 18 tests, including HID mapping, negative-origin pointer math, release-on-state-change, and the local Ctrl+Alt+Shift+Escape chord. No real input was injected.
- [UNVERIFIED] M7 decoder, ViewerRuntime, network integration, NV12 renderer path, cursor overlay, real app mode, and host integration are not present. M6 and M7 remain partial regardless of the isolated input results.
- [HUMAN-PENDING] M4b visual review, M5 capture, M6 service/security checks, and M7 two-PC/network/latency/input checks remain recorded in `blocked.md` and `docs/HARDWARE.md`.

### 2026-10-07 — M6 service/helper and M8 host-side audit

- [TESTED-FAKE] Host-agent tests passed: 26 passed, 0 failed. This includes the bounded SCM supervisor policy, service stop-event argument validation, session-change translation, host clipboard round-trip/echo/sequence/size behavior, authenticated control authorization, and fake frame dispatch.
- [COMPILE-ONLY] cargo fmt -p racc-host-agent -- --check, cargo clippy --offline -p racc-host-agent --all-targets -- -D warnings, and cargo check --offline -p racc-host-agent --target x86_64-pc-windows-msvc passed. No service command, helper, capture, or real clipboard operation was run.
- [COMPILE-ONLY] Windows service code enters the SCM dispatcher, watches bounded stop/session events, launches the active-console helper with WTS user-token APIs on WinSta0\Default, and stops a helper after a five-second grace period. It remains unverified at runtime; it cannot capture the Winlogon secure desktop.
- [COMPILE-ONLY] The foreground host connects Tailscale-only control authorization, DXGI capture, Media Foundation/OpenH264 H.264 encoding, paced UDP, the five-second control-disconnect lifecycle, Windows host clipboard adapters, and one-second StatsReport sends. StatsReport values have no live sample measurement. Host CPU comes from machine-wide GetSystemTimes; actual bitrate counts successful project UDP datagram bytes and excludes UDP/IP overhead.
- [TESTED-FAKE] Host clipboard policy has fake two-way round-trip, echo, size, retry, and sequence-wrap tests. The Windows listener/writer and viewer-to-host integration have not been exercised against a real OS clipboard.
- [HUMAN-PENDING] M6 service install/lifecycle, two-PC Tailscale streaming, resource readings, M8 real clipboard, and telemetry-vs-live measurements are listed in docs/HARDWARE.md. The M6 end-to-end checklist is blocked until a probe/host-loopback or verified viewer exists. App local IPC, approval UI, viewer clipboard OS adapter, and remote clipboard-disable signaling remain absent.
- [UNVERIFIED] The Windows Graphics Capture fallback and the required connection-attempts-per-minute limit are not implemented. The latter's unspecified threshold is recorded in docs/OPEN_QUESTIONS.md.

### 2026-10-07 — M9 macOS capture closeout

- [COMPILE-ONLY] Added a macOS-gated ScreenCaptureKit primary capture path with CGDisplayStream fallback, read-only Screen Recording preflight and typed permission failure, 420v NV12 output at fixed 30 fps, 720p30 default / 1080p30 ceiling, cursor included, and retained `CVPixelBuffer` frame handoff. The native Mac frame type remains separate from Windows D3D `GpuFrame`.
- [TESTED-FAKE] `cargo test --offline -p racc-capture` passed 16 tests on Windows, including the fake macOS lifecycle controller for start, migration/reconfiguration, access loss, recovery, and removal.
- [COMPILE-ONLY] `cargo check` passed for macOS capture, decode, and input crates; strict target Clippy passed for capture and input. Capture strict Clippy included all targets. `cargo metadata --offline --no-deps --format-version 1` passed, and the Windows capture dependency tree contains no objc2-family crates.
- [COMPILE-ONLY LIMIT] The macOS encoder target check could not finish because OpenH264's build script could not find the required target C++ compiler (`c++`). Strict decode Clippy currently fails on two `manual_is_multiple_of` warnings at `crates/decode/src/macos.rs:480-481`.
- [UNVERIFIED] No Apple linker or Mac runtime was used; ScreenCaptureKit/CGDisplayStream operation, permissions, NV12 behavior, fallback availability, VideoToolbox results, input, and performance remain unverified. The owner-reported model/OS in `docs/HARDWARE.md` is not confirmed on-device.
- [UNVERIFIED] M9 integration remains open: Mac capture is not wired to a host worker/VideoToolbox encoder; VideoToolbox decoder is not wired to a Mac wgpu viewer; permission UI, local input capture/session injection, NSPasteboard adapter, and a functional macOS host-agent/Unix IPC path are absent. Automatic topology watching/restart is also not implemented. The M10 `.app` build script exists but has not run on Mac.
- [HUMAN-PENDING] Mac runtime checks and required future integration tests are listed under M9 in `docs/HARDWARE.md`. Keep the Mac host default at 720p30 until sustained hardware testing supports a higher tier.
- M9 is **PARTIAL — COMPILE-ONLY / TESTED-FAKE**, not complete.

### 2026-10-07 — M7 host input worker integration

- [TESTED-FAKE] The foreground helper now routes authenticated InputEvent messages to a dedicated bounded input worker. The worker validates the authenticated connection, current reset epoch, selected display, and the latest topology announced by HostRuntime before mapping or injecting input. Queue saturation invalidates stale queued movement and still signals release of held keys/buttons without blocking the control callback.
- [TESTED-FAKE] Host-agent tests passed: 29 passed, 0 failed. Fake-injector coverage includes valid input, wrong connection, stale epoch, wrong display, release on deactivation and shutdown, reauthorization after reset, and queue saturation while injection is blocked.
- [COMPILE-ONLY] cargo fmt -p racc-host-agent -- --check, strict host-agent Clippy, and cargo check --offline -p racc-host-agent --target x86_64-pc-windows-msvc passed. No host command, helper, service, real capture, or OS input injection was run.
- [HUMAN-PENDING] Release is wired for connection replacement/close, stream reset or topology announcement, capture/encoder pause or rebuild, session end, and helper shutdown. The helper currently uses the last topology announced by HostRuntime; no live topology-change event source is connected. Real pointer mapping, keyboard layouts, viewer-to-host sessions, and release behavior still require hardware checks. M7 end-to-end acceptance remains incomplete.

### 2026-10-07 — M9 cursor separation correction

- [COMPILE-ONLY] Both ScreenCaptureKit and CGDisplayStream now use an explicit false cursor-in-video setting. The macOS host reports that cursor pixels are excluded and sends its hidden-cursor update, so the remote pointer remains unavailable instead of being embedded in the video.
- [COMPILE-ONLY] `cargo check --offline -p racc-capture --target x86_64-apple-darwin` passed; `cargo clippy --offline -p racc-capture --all-targets --target x86_64-apple-darwin -- -D warnings` passed. `cargo test --offline -p racc-capture --quiet` passed 16 tests on Windows; these do not exercise Apple frameworks.
- [UNVERIFIED] This implementation does not extract a real macOS cursor bitmap, hotspot, and position from the capture path. No generic cursor was fabricated. The supported metadata source remains an open M9 implementation question; the Mac capture exclusion behavior is still HUMAN-PENDING on hardware.
- [VERIFIED-REVIEW] Superseded the cursor-in-video ADR with ADR 0040, corrected `docs/CAPTURE.md`, `docs/VIEWER.md`, `docs/MACOS.md` and the M9 hardware checklist. M9 remains partial.

### 2026-10-07 — M10 native window lifecycle

- [COMPILE-ONLY] Added current work-area enumeration for Windows (`MONITORINFO.rcWork`, scaled with per-monitor effective DPI) and macOS (`NSScreen.visibleFrame`) and validate persisted geometry before the first window opens. The no-monitor fallback resets position to the origin and caps saved dimensions at 1280×800.
- [COMPILE-ONLY] Close now hides the native Windows or AppKit window after tray initialization. Tray Open and second-instance handoff show and focus it. If tray initialization or native visibility fails, the app falls back to minimize/restore, and the app's visibility event pauses viewer decoding.
- [TESTED-FAKE] `cargo test --offline -p racc-app` passed 73 tests, including geometry validation and fallback, tray menu, and second-instance handoff. `cargo clippy --offline -p racc-app --all-targets -- -D warnings` passed on Windows. `cargo check --offline -p racc-app` and `cargo check --offline --target x86_64-apple-darwin -p racc-app` passed. Mac strict Clippy is blocked by two `unused_mut` diagnostics in `crates/app/src/live.rs` (lines 146 and 345), outside this lifecycle adapter.
- [HUMAN-PENDING] Native tray/window behavior and geometry restoration still need observation on Windows and the Mac, including display changes and second-instance activation. No app window or installer was launched for this check.


### 2026-10-07 — M8/M9 integration and M10 release verification

- [VERIFIED-RUN] The owner authorized text clipboard sync only while enabled for a session and only with the selected Tailscale peer. The Windows real-OS integration test against a fake remote passed; it wrote two fixed harmless strings and left the known test value on the clipboard. It did not connect to another machine. Workspace clipboard policy tests passed.
- [COMPILE-ONLY] Added NSPasteboard text adapters for viewer and authenticated foreground host, gated by the enabled session. `cargo check --offline --tests -p racc-app --target x86_64-apple-darwin`, strict macOS app Clippy, and the Apple-target `racc-clipboard` check passed. The full macOS host target check stops in OpenH264 before compiling host Rust because target `c++` is unavailable.
- [COMPILE-ONLY] The macOS Settings panel now separately checks Screen Recording and Accessibility and provides explicit buttons to fixed Settings URLs. The URL-routing test and Windows/macOS app Clippy/checks passed; Monterey runtime behavior is HUMAN-PENDING.
- [TESTED-FAKE] Added a per-peer host connection-attempt limit of 20 per rolling minute with a 4,096-peer state cap. `cargo test --offline -p racc-host-agent` passed 39 tests; strict host-agent Clippy and the Windows target check passed.
- [VERIFIED-RUN] `scripts/build-release.ps1 -DryRun` passed after dependency changes settled. The default Windows release build measured app 13,117,952 B, host-agent 1,885,696 B, ZIP 5,700,501 B. Compared thin LTO, one codegen unit, symbol stripping, and unwind panic handling: app 12,058,624 B (8.08% smaller), host-agent 1,701,376 B (9.77% smaller), combined executable bytes 8.29% smaller. Clean profile build took 3m59s versus 2m20s default. Selected the smaller profile and retained unwind for the service FFI panic boundary; see ADR 0032.
- [VERIFIED-RUN — SUPERSEDED BY FINAL SOURCE REBUILD BELOW] Earlier Windows portable ZIP: 5,464,008 bytes, SHA-256 `5b2131213003545b13de172b86078558d1678ef4b2a9d5f8d12a1062d71192cf`; app 12,058,624 B; host-agent 1,701,376 B. It was not installed or run.
- [VERIFIED-RUN] `cargo fmt --all -- --check`, strict workspace Clippy, and workspace tests passed. The core loopback test's 3-second hot-spin deadline flaked twice under workspace checks; it now sleeps 1 ms between polls and has a 10-second deadline. The isolated test and a complete `check-all.ps1` run passed format, Clippy, workspace tests, layering, and feature gates. `check-all.ps1` then exited 1 only at cargo-deny because the unchanged policy excludes required OpenH264 BSD-2-Clause. One release benchmark is ignored; the real Windows clipboard test was separately run and passed.
- [HUMAN-PENDING] Real Windows/Mac host-viewer sessions, native tray and permission link behavior, real Tailscale route/telemetry measurements, private idle memory, clean install/uninstall, Mac bundle and signing, and the 24-hour soak remain unverified. M11a remains ordered after full M8/M10 acceptance. The normal Codex sandboxed shell still fails before startup; repository commands used the working approved elevated route.

### 2026-10-07 — Final integrated M3b/M7–M10 audit

- [VERIFIED-RUN] `cargo fmt --all -- --check` passed. `cargo clippy --offline --workspace --all-targets -- -D warnings` passed. `cargo test --offline --workspace` passed all crates; session has 100 passing tests, testkit 29, topology 28, and the app has 87 passing plus one intentionally ignored clipboard test (the separately authorized fixed-string OS clipboard run is recorded above). The two long simulated testkit soak cases each completed successfully.
- [VERIFIED-RUN] `scripts/check-layering.ps1`, `scripts/check-features.ps1`, and `cargo check --offline --workspace --target x86_64-pc-windows-msvc` passed. `scripts/check-all.ps1` passed format, Clippy, workspace tests, layering and feature gates, then exited 1 at cargo-deny because required `openh264` and `openh264-sys2` BSD-2-Clause licenses are excluded by the preserved allowlist. Duplicate-version and unmaintained-crate notices are warnings.
- [COMPILE-ONLY] `cargo clippy --offline -p racc-app --all-targets --target x86_64-apple-darwin -- -D warnings`, Apple-target checks for capture/decode/input/clipboard including tests, and strict Clippy for capture/input/clipboard passed. Full `cargo check -p racc-host-agent --target x86_64-apple-darwin` stops in `openh264-sys2` before host-agent compilation because target `c++` is unavailable.
- [TESTED-FAKE / COMPILE-ONLY] Mac CPU sampling now uses aggregate Mach host ticks, reports unknown until a valid delta, and has three deterministic fake-counter tests plus an isolated Apple-target check. Mac app and host Unix-socket IPC enforce same-user checks in source. ADR 0042 selects pointer-in-video for Mac; Windows keeps separate cursor metadata.
- [VERIFIED-REVIEW] Updated `docs/MACOS.md`, `docs/HARDWARE.md`, and `blocked.md` to remove obsolete claims that the Mac CPU sampler and local IPC are missing. No screen was launched or captured for this audit.
- [HUMAN-PENDING] M4b owner visual review, M5/M6 real Windows capture/service checks, M7 cross-machine viewer/input, M8 peer clipboard/live telemetry, M9 real Mac runtime, and M10 installer/Mac bundle/native behavior/24-hour soak remain pending. M11a stays behind its explicit M8/M10 preflight. The normal PowerShell launcher still requires Codex restart to load its already-saved fallback.
- [VERIFIED-RUN] Rebuilt the final Windows portable artifact after UI and host-control changes: app 12,371,456 B, host-agent 1,876,480 B, ZIP 5,644,455 B, SHA-256 `d8e42189f915329413cc17540be5850762427f4f200a717b5d0a31fdc6e2900e`. `Get-FileHash` independently matched the generated checksum file. The archive was built only; no installation or release runtime was performed.
- [TESTED-FAKE / COMPILE-ONLY] M6 shutdown now drains active remote control handlers and waits for disconnect callbacks before server stop returns. Safe hosting enable/disable remains unavailable until the platform owner loop can stop and reconstruct runtime resources and rebind the listener.
- [VERIFIED-REVIEW] M4b source polish adds a contextual first-frame state card and separates empty canvas, fake-pattern, and NV12 rendering. No app window or screenshot was used; visual fit and glyph rendering remain owner-review items.
- [VERIFIED-RUN] After these final source changes, Windows workspace target check, Apple-target app strict Clippy, 13-library layering check, and release feature-gate check all passed. A final normal sandboxed PowerShell retry still failed before startup with `helper_unknown_error: setup refresh had errors`; the approved elevated route was used.
- [VERIFIED-RUN] Final portable artifact sizes and SHA-256 are recorded in docs/HARDWARE.md; the archive was independently checked and was not installed.


### 2026-10-07 — Cross-platform local setup scripts

- [UNVERIFIED] Added root setup-windows.ps1 and setup-macos.sh with SETUP.md. Windows provisions build tools through winget, builds the existing release/installer flows, installs the app, service, scoped firewall rules and sign-in startup. macOS installs the pinned Intel Rust toolchain, builds the app bundle, places it in the current user's Applications folder, and installs the existing user LaunchAgents. Neither script installs or modifies Tailscale.
- [HUMAN-PENDING] The Mac script cannot grant Screen Recording or Accessibility permissions. The setup scripts have not been executed on any of the three machines; shell startup in this environment still fails with the setup refresh error. A successful script run will not resolve the known unavailable Hosting toggle or prove streaming.

### 2026-10-07 — Monterey VideoToolbox helper import correction

- [VERIFIED-REVIEW] The Mac-provided Monterey build log stopped because `macos.rs` imported `videotoolbox_avcc_to_annex_b` from the crate root, while the public helper is defined in `crate::macos_avcc`. Corrected the import to the module that owns the helper; this is a Rust path error, not a Monterey API/version incompatibility.
- [VERIFIED-RUN] `cargo fmt --all -- --check` passed.
- [UNVERIFIED] The Windows Apple-target `cargo check --offline -p racc-encode --target x86_64-apple-darwin` stopped in `openh264-sys2` before checking this crate because the environment has no target `c++` compiler. The Monterey build must be rerun after pulling this fix to confirm the next stage.

### 2026-10-07 — Monterey Rustup self-update network failure

- [VERIFIED-REVIEW] The owner-provided setup log shows the pinned Rust toolchain and target were already installed, then Rustup failed while checking its optional self-update manifest because the Mac connection reset. The macOS setup now passes Rustup's documented `--no-self-update` option to the pinned toolchain installation; it still resolves and installs the repository-pinned toolchain and Apple target.
- [VERIFIED-RUN] `bash -n setup-macos.sh` passed using Git Bash on Windows. The setup installer itself was not executed here.
- [HUMAN-PENDING] The updated script has not been run on Monterey. Pull or download the latest `main` source and rerun `setup-macos.sh`; later Cargo downloads/build steps may still need a stable network connection.
