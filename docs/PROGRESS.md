# Project Progress

## Environment

Probe date: 2026-10-06 (America/New_York).

- [VERIFIED-RUN] The agent environment is Microsoft Windows 10.0.19045 (Windows_NT), reported by the Windows OS-description command.
- [VERIFIED-RUN] rustc reports 1.95.0 (59807616e 2026-04-14); Cargo reports 1.95.0 (f2d3ce0bd 2026-03-21).
- [VERIFIED-RUN] The repository pins stable Rust 1.95.0 with rustfmt and clippy.
- [COMPILE-ONLY] Installed targets: x86_64-pc-windows-msvc and x86_64-apple-darwin. Both target-add commands exited 0; Windows was already current and macOS standard-library support was installed.
- [UNVERIFIED] Tailscale is not installed or available on PATH in this environment. No tailnet behavior was tested.
- [UNVERIFIED] The physical Windows PCs, GPUs, displays, and 2015 Mac were not accessible; no hardware measurements were made.

## Milestone status

| Milestone | Status | Evidence |
|---|---|---|
| M0 Workspace scaffold | Complete | [COMPILE-ONLY] Workspace, scripts, and documentation checks passed; see final report. |
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

- [UNVERIFIED] cargo-deny is not installed, so deny.toml parsing and an advisory database check could not be run. The scripts request warning-level advisory findings with cargo deny check --warn advisories; current behavior is documented in the official [cargo-deny advisory configuration](https://embarkstudios.github.io/cargo-deny/checks/advisories/cfg.html) and [check CLI](https://embarkstudios.github.io/cargo-deny/cli/check.html).
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

M1 should start from the rd-proto library stub in crates/proto and the table of contents in docs/PROTOCOL.md. Implement the bounded v0 header and control message parsing/serialization there, update docs/PROTOCOL.md in the same change, and add the M1 malformed-input and round-trip tests. No M1 protocol types or behavior were started during this session.