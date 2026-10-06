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
| M0.5 Housekeeping and publish | Verification complete; publish pending | [VERIFIED-RUN] Identity, naming, policy, documentation, and local checks complete; origin verification/push pending. |
| M1 Protocol | Not started | [UNVERIFIED] |
| M2 Transport | Not started | [UNVERIFIED] |
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
- [HUMAN-PENDING] Real Windows, Mac, GPU, input, and Tailscale checks remain listed in docs/HARDWARE.md.

## Unverified

- [UNVERIFIED — M0 only] At the M0 run, cargo-deny was not installed, so deny.toml parsing and an advisory database check were skipped. This was resolved during M0.5; see the current M0.5 session report below.
- [COMPILE-ONLY] Cross-checks establish compilation only; they do not demonstrate platform behavior.
- [HUMAN-PENDING] Hardware and tailnet checks are not performed by this agent.

## Blocked

No implementation blocker. M0.5 publication is pending the required remote inspection and normal push.

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

M1 should start from the rd-proto library stub in crates/proto and the table of contents in docs/PROTOCOL.md. Implement the bounded v0 header and control message parsing/serialization there, update docs/PROTOCOL.md in the same change, and add the M1 malformed-input and round-trip tests. No M1 protocol types or behavior were started during this session.

### 2026-10-06 — M0.5 housekeeping and publish (verification complete; publish pending)

- [VERIFIED-RUN] Preflight began with a clean main worktree and `scripts/check-all.ps1` exited 0.
- [VERIFIED-RUN] Set repository-local Git identity to `rkohnmn <275230809+rkohnmn@users.noreply.github.com>`. Rewrote the four unpublished commits so both author and committer match; the post-rewrite audit shows all four correct and their messages contain no attribution trailers. No global Git config command was used.
- [VERIFIED-RUN] Cargo metadata reports 15 workspace packages and every package is prefixed `racc-`; crate directory names are unchanged. Current tree search for the former package prefix finds matches only in historical ADR 0002 and historical M0 text in this file.
- [VERIFIED-RUN] `cargo-deny` 0.20.2 installed with `cargo install cargo-deny --locked`. The initially requested shorthand `--warn advisories` was rejected by this release because `advisories` is a check name, not a lint. The supported command `cargo deny check --warn vulnerability --warn unsound --warn unmaintained --warn notice --warn yanked` exits 0 and reports `advisories ok, bans ok, licenses ok, sources ok`. Scripts and manual instructions use that verified command.
- [VERIFIED-RUN] The layering negative test temporarily added `racc-core → racc-app`; the PowerShell check exited 1 with `Forbidden application dependency found in library dependency tree: racc-core`. The temporary manifest addition was restored byte-for-byte and temporary library target removed. The normal Bash and PowerShell checks report `Layering check passed for 13 library crates.`
- [VERIFIED-RUN] This Windows PC was identified as PC #1 by its NVIDIA GeForce RTX 3050 Ti Laptop GPU. Read-only probes recorded build 19045, driver 32.0.15.9571, AMD integrated graphics driver 31.0.21923.11000, three active displays at 1920×1080, Rust/Cargo 1.95.0, and Tailscale not installed/on PATH. Details and owner-reported hardware are in docs/HARDWARE.md.
- [VERIFIED-RUN] Project name, brand rule, licensing notice, hardware record, questions, and ADR 0008 are updated. ADR 0002 is marked superseded. Human-only hardware items remain unchecked.
- [VERIFIED-RUN] `scripts/check-all.ps1` and `scripts/check-all.sh` each exited 0 and ran cargo-deny. `cargo check --workspace --target x86_64-pc-windows-msvc` and the Intel macOS target check both exited 0. Target checks are [COMPILE-ONLY].
- [VERIFIED-RUN] The working-tree and all four committed snapshots passed the public-hygiene scan for local paths, private usernames/machine names, disallowed email addresses, and secret-like material. The only email found was the approved GitHub noreply address.
- [VERIFIED-RUN] Added the requested origin and ran `git ls-remote origin`; it exited 0 with no refs. The remote is empty, so a normal push is the next step.
