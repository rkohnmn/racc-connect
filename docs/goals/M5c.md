# Goal M5c: Tailscale identity and discovery (`racc-identity`)

Save as `docs/goals/M5c.md`.

**Run after:** M3a complete. Must run before M6.
**Needs from you first:** Tailscale installed and logged in on **this PC** (PC #1). The M0.5 probe reported it missing from PATH. Check with `where tailscale` or look for `C:\Program Files\Tailscale\tailscale.exe`, and make sure this PC shows as connected in your tailnet. Without it the agent can only write code against synthetic data.

## Human checklist
1. Tailscale running on PC #1; `tailscale status` works in a terminal (it is fine if the exe is not on PATH; the agent will look in the standard install location).
2. Optional but valuable: at least one other device online in the tailnet (PC #2 or the Mac) so there is a real peer to list.
3. Working tree clean on `main`.
4. **Privacy:** the agent will read your real tailnet to learn the JSON shapes. It must not commit any real names, IPs or emails. The prompt enforces this, but check `git diff` before the push if you want to be sure.

## Launcher
```
/goal Complete the work specified in docs/goals/M5c.md. Read AGENTS.md, docs/goals/COMMON.md, docs/DEV_SETUP.md and docs/PROGRESS.md first. Done only when every acceptance check in docs/goals/M5c.md passes with evidence, then stop. Do not start the next goal.
```

## Prompt
~~~text
GOAL: M5c. Implement crates/identity (package racc-identity): discover this machine's Tailscale identity and addresses, list tailnet peers with their online state and connection path, resolve which peer an incoming connection belongs to (whois), and manage a host-side allowlist. Determine and document, per platform, how to reach Tailscale from a program, and implement the most robust approach first.

COMMON.md applies in full. Read AGENTS.md (sections 4.1, 5.4), docs/DEV_SETUP.md section 4.3, docs/PROTOCOL.md and docs/TRANSPORT.md (bind policy) first.

PREFLIGHT
Clean tree; check-all green; M3a complete. Probe: is Tailscale reachable from this PC? Look for tailscale.exe on PATH, then in the standard install location, and run `tailscale version` and `tailscale status --json` (read-only). If Tailscale is not installed or not logged in, do NOT install it: implement everything against synthetic fixtures, mark all live checks BLOCKED-HUMAN with exact instructions, and continue.

PRIVACY (strict, the repository is public)
Real tailnet output may be read to learn the schema. NEVER commit it, never paste it into docs or tests, never include real hostnames, IPs, emails, node keys or tailnet names in any file or log committed. All fixtures are hand-written or mechanically anonymized (replace every identifying value with obvious placeholders such as host-a, 100.64.0.10 style documentation values). Add a hygiene test that scans the identity crate's fixtures for IPs in the Tailscale range and email-like strings.

DEPENDENCIES
racc-identity runtime: racc-proto only if needed; serde and serde_json (JSON parsing of tailscale output) are allowed in this crate only; nothing else new (use std::process::Command for the CLI approach). Keep #![forbid(unsafe_code)] unless a direct LocalAPI client is implemented through OS pipes (see below); if so, confine unsafe to one small module.

DESIGN
1. Approach A (implement first, default): drive the Tailscale CLI. Locate the executable (PATH, then standard install locations per OS: Windows Program Files\Tailscale; macOS the app bundle's command-line binary and any documented standard location; Linux PATH). Run `status --json` and `whois --json <ip>` and `ip -4` / `ip -6` with timeouts (default 2 s), bounded output size (for example 4 MiB), no shell, arguments passed as separate strings, environment minimal. Parse tolerantly: ignore unknown fields, treat missing fields as None, never panic, typed errors (NotInstalled, NotRunning, NotLoggedIn, Timeout, BadJson, Io). Cache status for a short TTL (default 1 s) to avoid spawning too often.
2. Approach B (evaluate and document, implement only if simple and safe): direct LocalAPI over the platform socket (Windows named pipe, Unix socket for the daemon, the macOS GUI app's mechanism). Determine from documentation and by observation what each platform uses; record findings in docs/IDENTITY.md; do not guess. If B is not implemented, record the reasons and the CLI performance numbers (time per call) so the human can decide whether B is worth it later.
3. Data model (stable domain types, independent of the JSON): SelfNode (display name, tailnet IPv4 and IPv6 addresses, online), Peer (stable id, host name, DNS name, addresses, OS string, online, path: Direct/Derp/Unknown with the relay region label when relayed, tags, owner login name), Whois result (peer id, owner login, tags, node name). Map the tool's JSON fields to these with documented rules (for example Direct when a current direct address is reported, Derp when only a relay is reported, Unknown otherwise).
4. Bind address: self_bind_addr() returns the Tailscale IPv4 (preferred) or IPv6 address of this machine and validates it with racc-net's bind policy function (add racc-net as a path dependency only if the layering rules allow; otherwise duplicate the pure validation in a shared place and test they agree; prefer depending on racc-net).
5. Peer resolution: resolve(remote_ip) -> Result<PeerIdentity> via whois, with a short cache; reject when whois fails, when the address is not a tailnet address, or when the result is ambiguous.
6. Allowlist: pure logic plus a persistence trait with a JSON file implementation in the user's app data folder (path decided by a small function; no Windows API calls: take the base directory as a parameter and let the caller supply it). Keyed by stable node id plus owner login; entries: label, added time, state (Approved, Pending, Rejected), last seen. Operations: check(identity) -> Approved/Pending/Rejected/Unknown, approve, reject, remove, with atomic writes (write temp then rename), bounded size, corruption handling (corrupt file is moved aside and an empty list used, with an event), and a 'pending approval' queue with a bound. Never store anything but ids, labels, timestamps and states.
7. Discovery of host-capable peers: probe(peer_addr, port, timeout) does a TCP connect to the agent's control port (DEFAULT_CONTROL_PORT constant, choose an unassigned port, document and record in an ADR) and performs the Hello/HelloAck exchange from racc-proto (without starting a session; send a probe flag or just close after HelloAck status). Mark peers HostCapable or NotHost. Implement the probe logic against a trait for the connection so it is testable with fakes; the real implementation can use racc-net's control connection (bind policy applies to listeners only).
8. Events for telemetry and UI: PeerOnline, PeerOffline, PathChanged. A pure diff function between two peer lists produces them.

TESTS
Anonymized fixture-based parsing tests for several shapes (online peer direct, relayed, offline, tagged node, missing fields, extra unknown fields, empty tailnet, logged-out state); timeout and oversize-output handling using a fake process runner; whois resolution and ambiguity tests; allowlist tests including corruption recovery and atomic write behavior using a temp directory; bind validation agreement tests; peer diff tests; hygiene test for fixtures; property test that arbitrary bytes as tool output never panic the parsers.

LIVE VERIFICATION (only if Tailscale is available on this PC; VERIFIED-RUN with date, no identifiers in the record)
- CLI located and version recorded (version string only).
- status parsed into the domain types: number of peers, how many online, path kinds seen (counts only).
- Time per CLI call (median and p95 over 20 calls).
- self_bind_addr returns an address accepted by the bind policy.
- whois on one peer address returns a result (record only that it succeeded).
If unavailable, mark BLOCKED-HUMAN and add the checks to HARDWARE.md.

DOCUMENTATION
docs/IDENTITY.md: platform access findings (what is verified, what is not), data model and JSON mapping rules, the allowlist file format and location, the discovery probe, privacy notes, performance numbers, open questions about the macOS access method. ADRs: CLI-first integration (supersedes the LocalAPI assumption in notes), default control port, allowlist storage. Update PROGRESS.md, OPEN_QUESTIONS.md (including resolving the 'LocalAPI access method per platform' item to the extent verified). HARDWARE.md human checks (HUMAN-PENDING unless verified): run the live checks on PC #2 and on the Mac; confirm the allowlist approve and reject flow end to end once M6 exists.

HARD CONSTRAINTS
No listeners, no sessions, no UI. Do not log or commit real tailnet data. Do not install or reconfigure Tailscale. Do not start the next goal.

ACCEPTANCE CHECKS
B1 to B8, plus:
T1. Fixture parsing tests pass; the hygiene test over fixtures passes; repository search shows no real tailnet data.
T2. Process runner tests prove timeouts, output caps and no shell use.
T3. Allowlist behavior tests pass including corruption recovery and atomic writes.
T4. Live verification results recorded, or BLOCKED-HUMAN with exact instructions.
T5. docs/IDENTITY.md and ADRs exist; the cargo tree for racc-identity shows only the allowed dependencies.

FINAL REPORT: COMMON.md section 14, plus what was verified live versus only against fixtures.
~~~
