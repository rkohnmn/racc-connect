# Project Progress

## Environment

Probe date: 2026-10-06 (America/New_York).

- [VERIFIED-RUN] The agent environment is Microsoft Windows 10.0.19045 (Windows_NT), reported by the Windows OS-description command.
- [VERIFIED-RUN] rustc reports 1.95.0 (59807616e 2026-04-14); Cargo reports 1.95.0 (f2d3ce0bd 2026-03-21).
- [VERIFIED-RUN] The repository pins stable Rust 1.95.0 with rustfmt and clippy.
- [COMPILE-ONLY] Installed targets: x86_64-pc-windows-msvc and x86_64-apple-darwin. Both target-add commands exited 0; Windows was already current and macOS standard-library support was installed.
- [VERIFIED-RUN] Tailscale is not installed or available on PATH on this Windows PC. No tailnet behavior was tested.
- [VERIFIED-RUN] This machine matches Windows PC #1 by its NVIDIA GeForce RTX 3050 Ti Laptop GPU. Windows build, GPU drivers, and three active 1920×1080 displays were probed read-only; see docs/HARDWARE.md. Windows PC #2 and the Mac remain owner-reported only.

## Milestone status

| Milestone | Status | Evidence |
|---|---|---|
| M0 Workspace scaffold | Complete | [COMPILE-ONLY] Workspace, scripts, and documentation checks passed; see final report. |
| M0.5 Housekeeping and publish | Complete | [VERIFIED-RUN] M0.5 checks passed; `main` was pushed to the empty requested origin. |
| M1 Protocol | Complete | [VERIFIED-RUN] Protocol tests, documentation, cargo-deny, scripts, and compile-only target checks passed; see the M1 report. |
| M2 Transport | Complete | [VERIFIED-RUN] M2 acceptance checks 1–13 passed on Windows 10.0.19045; see the final report below. Windows and Intel macOS target checks are COMPILE-ONLY. |
| M3 Topology and session logic | Not started | [UNVERIFIED] |
| M4a UI toolkit spike | Not started | [UNVERIFIED] |
| M4b UI shell | Not started | [UNVERIFIED] |
| M5 Windows capture and encode | Not started | [UNVERIFIED] |
| M6 Windows host agent | Not started | [UNVERIFIED] |
| M7 End-to-end viewer | Not started | [UNVERIFIED] |
| M8 Clipboard and telemetry | Not started | [UNVERIFIED] |
| M9 macOS host and viewer | Not started | [UNVERIFIED] |
| M10 Polish and packaging | Not started | [UNVERIFIED] |

## Verified

- [VERIFIED-RUN] Both check-all scripts ran on Windows 10.0.19045 and exited 0.
- [VERIFIED-RUN] The two binary stubs printed their package names and versions.
- [VERIFIED-RUN] The layering check passed normally and rejected a temporary forbidden rd-core to rd-app dependency edge.
- [COMPILE-ONLY] Workspace clippy and both platform cross-checks passed.
- [VERIFIED-RUN] M1 bounded v0 protocol passed 20 racc-proto tests, the 10000-case property run, cargo-deny and both check-all scripts.
- [HUMAN-PENDING] Real Windows, Mac, GPU, input, and Tailscale checks remain listed in docs/HARDWARE.md.

## Unverified

- [UNVERIFIED — M0 only] At the M0 run, cargo-deny was not installed, so deny.toml parsing and an advisory database check were skipped. This was resolved during M0.5; see the current M0.5 session report below.
- [COMPILE-ONLY] Cross-checks establish compilation only; they do not demonstrate platform behavior.
- [HUMAN-PENDING] Hardware and tailnet checks are not performed by this agent.

## Blocked

None.

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
