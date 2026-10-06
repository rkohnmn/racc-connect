# DEV_SETUP.md

How to set up a development environment, what each environment can and cannot verify, and the commands every contributor (human or AI agent) uses. **Read this before `AGENTS.md` milestones.** Its main job is to stop "it compiles" from being reported as "it works".

Place this file at `docs/DEV_SETUP.md`.

---

## 1. Verification labels (mandatory vocabulary)

Every claim in `docs/PROGRESS.md`, commit messages and pull requests about platform behavior uses one of these labels:

| Label | Meaning |
|---|---|
| **VERIFIED-RUN** | The code was executed on the target OS and the behavior was observed. State the OS, machine and date |
| **COMPILE-ONLY** | The code builds (`cargo check` or `cargo build`) for the target, but was not executed there |
| **TESTED-FAKE** | Logic was tested with fake or loopback backends. Says nothing about real hardware |
| **UNVERIFIED** | Written but neither compiled for the target nor run |
| **HUMAN-PENDING** | Needs a human test listed in `docs/HARDWARE.md` |

Rules:
- Capture, encode, decode, input injection, service behavior, permissions and anything involving a real GPU or the Tailscale daemon can never be higher than **COMPILE-ONLY** unless run on the real machine.
- Never write "works", "fixed" or "done" for platform code without a label.
- Never record a measurement (latency, CPU, memory, frame rate) that was not actually measured. Write "not measured".

---

## 2. Toolchain

### 2.1 Rust

- Install via `rustup`. The repository pins the toolchain in `rust-toolchain.toml` (stable channel, with components `rustfmt` and `clippy`). Do not override it locally.
- Required targets for cross-checking from any machine:

```
rustup target add x86_64-pc-windows-msvc
rustup target add x86_64-apple-darwin
```

The 2015 Mac is Intel, so `x86_64-apple-darwin` is the macOS target. Apple Silicon (`aarch64-apple-darwin`) is optional and not required by the project.

### 2.2 Recommended cargo tools

| Tool | Purpose | Install |
|---|---|---|
| `cargo-nextest` | Faster, clearer test runs | `cargo install cargo-nextest` |
| `cargo-deny` | License and advisory checks (enforces the no-GPL rule) | `cargo install cargo-deny` |
| `cargo-udeps` (optional) | Find unused dependencies | `cargo install cargo-udeps` |
| `cargo-xwin` (optional) | Link Windows MSVC binaries from Linux or macOS | `cargo install cargo-xwin` |

`cargo-deny` configuration lives in `deny.toml`. It must reject GPL and AGPL licenses for anything linked into shipped binaries. Run it whenever a dependency is added.

---

## 3. Verification capability by environment

| Environment | Can do | Cannot do |
|---|---|---|
| **Linux or cloud sandbox (typical agent environment)** | Format, lint, unit and property tests for `proto`, `net`, `topology`, `session`, `telemetry`, `core` with fakes; loopback network tests; `cargo check` for Windows and macOS targets if toolchains resolve | Run Windows or macOS capture, encode, decode, input, service or UI-on-real-GPU behavior; talk to a real Tailscale tailnet (usually); measure latency |
| **Windows 10 PC** | Everything for Windows: capture, encode, decode, helper and service behavior, real GPU paths, UI rendering | macOS code beyond cross-check |
| **2015 Intel Mac** | Everything for macOS: ScreenCaptureKit or CGDisplayStream, VideoToolbox, permissions, thermals | Windows code beyond cross-check |
| **Tailnet with all three machines** | Real path (direct vs DERP), `whois` allowlisting, real-world latency and loss | n/a |

### 3.1 Cross-checking platform code

From any machine, platform-gated code can usually be type-checked:

```
cargo check --workspace --target x86_64-pc-windows-msvc
cargo check --workspace --target x86_64-apple-darwin
```

Caveats:
- `cargo check` does not link, so it usually works without the target's linker or SDK. Crates with build scripts that need native headers (for example bindgen-based crates needing Apple SDK headers) may fail on a non-macOS machine. If so, record the failure and mark that code **UNVERIFIED**, not **COMPILE-ONLY**.
- A passing cross-check proves types and syntax only. It proves nothing about runtime behavior.
- Prefer pure-Rust binding crates (for the `windows` API and the Objective-C/Apple frameworks) over crates that need external build tooling, to keep cross-checking viable.

### 3.2 First-session environment probe

Every agent session that does not already have a record must run these and write the results into `docs/PROGRESS.md` under "Environment":

```
rustc --version
cargo --version
rustup target list --installed
uname -a
cargo check --workspace --target x86_64-pc-windows-msvc   (record pass/fail)
cargo check --workspace --target x86_64-apple-darwin      (record pass/fail)
tailscale version                                         (record "not installed" if absent)
```

---

## 4. Per-machine setup (for the human)

### 4.1 Windows 10 PCs

1. Install the latest GPU driver (NVIDIA, Intel or AMD). Hardware H.264 encode depends on it.
2. Install **Visual Studio Build Tools** with the *Desktop development with C++* workload and a Windows 10/11 SDK.
3. Install Rust via `rustup` (MSVC toolchain, the default).
4. Install Git.
5. Install Tailscale and log in. Confirm with `tailscale status`.
6. Optional tools: FFmpeg (`ffprobe`, `ffplay`) to inspect `.h264` output; Wireshark for packet inspection.
7. Record in `docs/HARDWARE.md`: Windows build number, GPU model and driver version, monitors (resolution, refresh, scale).

For service and helper work (M6), you will test as an administrator and may need to test with a standard user account, lock screen, UAC prompts, sign-out and fast user switching. Use a test machine state you can recover from.

### 4.2 The 2015 Intel Mac

1. Run `sw_vers` and record the macOS version in `docs/HARDWARE.md`. The project assumes ScreenCaptureKit needs macOS **12.3 or newer**. If the machine is on an older version, the CGDisplayStream fallback path is used.
2. Install Xcode Command Line Tools: `xcode-select --install`. For signing and notarization you need a full Xcode; the newest Xcode your macOS version supports will be older than the current release, and Monterey typically tops out around Xcode 14.2. Verify against Apple's current compatibility table before relying on this.
3. Install Rust via `rustup` and add the `x86_64-apple-darwin` target (installed by default on an Intel Mac).
4. Install Tailscale and log in. Confirm with `tailscale status` (the command-line tool location depends on the install method).
5. Grant permissions when prompted: **Screen Recording** (capture) and **Accessibility** (input injection). Changes to these require restarting the app. Permissions are tied to the app's code signature, so expect to re-grant after rebuilds unless the binary is consistently signed.
6. Optional tools: FFmpeg, Activity Monitor for CPU and memory, `powermetrics` (needs `sudo`) for thermal and power observation.
7. Record in `docs/HARDWARE.md`: exact model (for example MacBook Pro or iMac, year), macOS version, GPU, RAM, and whether the machine is plugged in during tests.

### 4.3 Tailscale on every machine

- All three machines must be on one tailnet and show as online in `tailscale status`.
- Verify connectivity with `tailscale ping <peer>`. Note whether the path is `direct` or relayed through DERP.
- The project reads peer status and identity (`whois`) from the Tailscale **LocalAPI**. How that API is reached differs by platform and install flavor (for example, a named pipe on Windows, a Unix socket for the Linux daemon, and a different mechanism for the macOS GUI app). Milestone M2 or the `identity` crate must **verify the actual access method on each machine** and record it in `docs/decisions/`. Do not assume one mechanism works everywhere.

---

## 5. Everyday commands

Run from the repository root.

Dependency layering: lower crates must never depend on higher crates, and no library crate may depend on racc-app or racc-host-agent. racc-core and every crate below it must build and test with no UI crate present. Run scripts/check-layering.sh or scripts/check-layering.ps1 to check this.

| Task | Command |
|---|---|
| Format check | `cargo fmt --all -- --check` |
| Format | `cargo fmt --all` |
| Lint (must be clean) | `cargo clippy --workspace --all-targets -- -D warnings` |
| Test (all) | `cargo test --workspace` or `cargo nextest run --workspace` |
| Test one crate | `cargo test -p racc-proto` |
| Cross-check Windows | `cargo check --workspace --target x86_64-pc-windows-msvc` |
| Cross-check macOS | `cargo check --workspace --target x86_64-apple-darwin` |
| License and advisory check | `cargo deny check --warn vulnerability --warn unsound --warn unmaintained --warn notice --warn yanked` |
| Build release | `cargo build --release -p racc-host-agent -p racc-app` |

A milestone is not done until fmt, clippy and test pass. Keep a `scripts/check-all.sh` (and a Windows-equivalent `scripts/check-all.ps1`) that runs them, and list them in `AGENTS.md`.

---

## 6. Running without real infrastructure

### 6.1 Loopback and fake backends

- Each platform trait (`Capture`, `Encoder`, `Decoder`, `InputInjector`, `Clipboard`) has a fake implementation in `testkit` or its own crate. Use fakes for all unit and integration tests that do not need hardware.
- The `testkit` loopback harness runs a host and a viewer in one process over `127.0.0.1`, with a configurable impairment layer (loss, reorder, duplication, jitter, rate limit).

### 6.2 Test-only network configuration

The security rule is to bind only to the Tailscale interface. For tests, use an explicit **test-only configuration** (for example an environment variable such as `STREAMER_TEST_BIND=127.0.0.1` that is compiled out of release builds or refused unless a debug flag is set). Never weaken the production default to make testing easier, and never ship a build that honors the test override.

### 6.3 Optional real-network impairment

On Linux, `tc netem` can add loss and delay to a real interface for realistic testing. This is optional and requires root. The built-in impairment layer is the default tool.

---

## 7. Inspecting results

| What | How |
|---|---|
| Is a recorded `.h264` file valid? | `ffprobe file.h264`; play with `ffplay -f h264 file.h264` |
| What is on the wire? | Wireshark filtered to the Tailscale interface and the project's UDP port (WireGuard encrypts tunnel traffic, so capture on the `tailscale` interface, not the physical NIC) |
| Throughput and loss on a path | `iperf3` between two machines over Tailscale addresses |
| Direct vs DERP | `tailscale status` and `tailscale ping` |
| CPU and memory | Task Manager or Resource Monitor (Windows); Activity Monitor (macOS). Record *private* memory, not just total working set, because GPU drivers map large shared libraries |
| Latency | On-screen timestamp overlay compared against a camera, or a high-speed camera pointed at both screens. Record the method with every number |
| Logs | `RUST_LOG=info` normally, `debug` for sessions, `trace` for per-packet (very noisy). Never log per-packet at info |

---

## 8. Troubleshooting

| Symptom | Likely cause |
|---|---|
| Cross-check for macOS target fails with missing headers or SDK | A dependency needs Apple SDK headers. Mark the code UNVERIFIED, prefer a pure-Rust binding crate, or run the check on the Mac |
| Windows helper captures a black frame | Protected content, wrong session, or capture running in session 0 instead of the console session |
| `DXGI_ERROR_ACCESS_LOST` repeatedly | Mode change, UAC, lock, or sleep in progress; the state machine should back off and re-create capture |
| macOS capture returns black frames or no frames | Screen Recording permission missing or tied to an old signature; re-grant and restart the app |
| macOS input does nothing | Accessibility permission missing |
| Peer shows online in Tailscale but not in the app | The agent is not running, is bound to the wrong address, or the peer is not on the host's allowlist |
| High latency and a direct path | Check encoder settings (B-frames or lookahead enabled?), queue depths, and pacing before blaming the network |
| High latency and a DERP path | Expected to be higher; check `tailscale status` for why a direct path was not formed |
| Video smears or freezes after packet loss | Keyframe request not reaching the host, or rate-limiting too aggressive |
| Clipboard ping-pong | Loop prevention is not tagging updates the app itself wrote |
| Tests pass locally but fail in CI | Timing assumptions in loopback tests; use virtual time or generous tolerances, never fixed sleeps |

---

## 9. Environment record (fill in and keep current)

Copy this block into `docs/HARDWARE.md` and fill it in.

```
Windows PC #1: Windows build ____  GPU ____  driver ____  monitors ____
Windows PC #2: Windows build ____  GPU ____  driver ____  monitors ____
2015 Mac: model ____  macOS ____  GPU ____  RAM ____
Tailscale versions: PC1 ____  PC2 ____  Mac ____
Agent sandbox: OS ____  Rust ____  Windows cross-check ____  macOS cross-check ____
```
