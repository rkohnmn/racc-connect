# Tailscale Identity and Discovery

`racc-identity` owns local Tailscale peer status, address validation, incoming-peer identity resolution, host-side approval policy, and a bounded host-capability probe. Its production access path is the Tailscale CLI. It does not install or configure Tailscale, create a listener, start a session, or send application traffic outside the Tailscale interface.

## Access by platform

| Platform | Implemented CLI lookup | Evidence and limits |
|---|---|---|
| Windows 10 | Search PATH, then %ProgramW6432%, %ProgramFiles%, and %ProgramFiles(x86)% under Tailscale\tailscale.exe. | Windows PC #1 ran the CLI in a read-only probe on 2026-10-07; aggregate results are below. Windows PC #2 installation and executable path remain unverified. |
| macOS | Search `PATH`, then `/Applications/Tailscale.app/Contents/MacOS/Tailscale`, then `/usr/local/bin/tailscale`. Set `TAILSCALE_BE_CLI=1` on every invocation. | Tailscale documents the bundled App Store binary path and CLI mode environment variable. The standalone CLI integration at `/usr/local/bin/tailscale` requires Ventura 13+, so Monterey must use the app-bundle path if using the standalone integration is unavailable. The owner-reported Mac OS is Monterey 12.7.6; no Tailscale app was executed on that Mac. |
| Linux | Search `PATH`, then `/usr/bin/tailscale` and `/usr/sbin/tailscale`. | The implementation supports the common executable locations, but Linux is outside this product's supported host/viewer platforms and has not been tested here. |

The CLI is launched directly with `std::process::Command`; no shell is involved. Arguments are separate OS strings. The child inherits no ambient environment: only the Windows system-root variables, the macOS `TAILSCALE_BE_CLI=1` switch, and explicitly configured additions are passed. Each command has a 2-second default deadline, a 4 MiB combined stdout/stderr cap, and a successful-result cache (1 second for status and whois). Timeout, output overflow, missing executable, backend-not-running, logged-out, I/O, and malformed JSON errors are typed. The process runner kills and reaps the child and joins both output readers on timeout, cap overflow, and reader/child errors.

The supported commands are:

- `tailscale status --json` for self, peers, online state, users, and current route hints.
- `tailscale whois --json <ip>` for one incoming peer. Tailscale requires `--json` before the address.
- `tailscale ip -4` then `tailscale ip -6` to select a local bind address, with parsed status addresses as fallback.
- `tailscale version` for a displayable version string.

The [Tailscale CLI reference](https://tailscale.com/docs/reference/tailscale-cli) documents machine-readable status and whois output, the whois flag ordering, macOS CLI behavior, and warns that status JSON may change. The [daemon overview](https://tailscale.com/docs/reference/tailscaled) describes the Windows service and the distinct macOS GUI app variants. We therefore treat JSON as an evolving input: fields are read case-insensitively, unknown fields are ignored, missing fields remain absent or map to conservative defaults, and all strings, arrays, peer counts, command output, and caches are bounded.

### Direct LocalAPI evaluation

A direct LocalAPI socket/pipe client is not implemented. The supported CLI provides the needed queries without assuming an undocumented Windows named-pipe location or an internal macOS app/system-extension IPC contract. Tailscale's macOS GUI variants bundle the CLI, daemon, and GUI components differently; a direct IPC client would need per-install-flavor discovery and platform transport code. The identity crate remains safe Rust and uses the documented CLI path until a measured need justifies a direct API integration. The Linux container documentation mentions a Unix LocalAPI socket, but that does not establish a supported socket path for the Windows or macOS desktop variants.

## Domain model and mapping

The Rust domain types are independent of Tailscale JSON structures:

- `SelfNode`: display and DNS names, optional IPv4/IPv6, online state.
- `Peer`: optional stable node ID and names, Tailscale addresses, OS label, online state, path, tags, owner login, and project host-probe status.
- `PeerIdentity`: stable node ID, optional owner login, tags, machine name, and returned addresses.
- `ConnectionPath`: `Direct`, `Derp { region }`, or `Unknown`.
- `PeerEvent`: deterministic online, offline, and path-change diffs.

Status JSON mapping is deliberately tolerant:

| CLI field | Domain mapping |
|---|---|
| `Self.HostName` / `Self.Name`, `DNSName`, `TailscaleIPs` | Local machine display/DNS names and address families. `Online` is used when present; otherwise a `Running` backend state means the local node is online. |
| `Peer` object values | Each peer with an object value is retained; malformed peer entries are skipped. `ID`, `HostName`, `DNSName`, `TailscaleIPs`, `OS`, `Online`, `Tags`, and `UserID` map when present. |
| `Peer.CurAddr` / `CurrentAddress` | For an online peer, a nonempty current endpoint means `Direct`. |
| `Peer.Relay` / `DERP` | For an online peer with no current endpoint, a nonempty relay string means `Derp` with that region label. A direct endpoint takes precedence when the peer is online. |
| absent path fields or an offline peer | `Unknown`; route hints on an offline peer are ignored because they may be stale. Missing `Online` defaults to offline, so its route hints are also ignored. |
| `User[UserID].LoginName` / `Name` | Optional owner login. It is used as part of the allowlist identity key and is not emitted in logs by this crate. |
| `Version`, `BackendState` | Optional version and backend state. `NeedsLogin`, `LoggedOut`, and `NeedsMachineAuth` map to `NotLoggedIn`; `Stopped` maps to `NotRunning`. |

Whois parsing accepts the current top-level `Node` and `UserProfile` shape and the legacy `Machine` and `User` shape. When both node fields are present, `Node` takes precedence. The parser chooses the first nonempty identifier in `StableID`, `NodeID`, then `ID` order, so a stable identifier wins over a numeric ID; a missing identifier is rejected. Node `Addresses`/`TailscaleIPs` are parsed as IP addresses. `Hostinfo.Hostname` supplies the node name when available, with `Name`/`HostName` as fallback. `Hostinfo.Tags` is used when present, otherwise legacy node-level `Tags` is used. `Hostinfo.OS` is ignored because OS is not part of the authorization identity. Current `UserProfile.LoginName` supplies the optional owner; legacy `User.LoginName`/`Name` remains supported (tagged devices need not have a user). More than one entry in a `Machines` or `Matches` array, or more than one entry in the selected `Node`/`Machine` array, is rejected as `AmbiguousWhois`. `resolve` validates that the requested address is in Tailscale's IPv4 or IPv6 bind ranges before invoking whois, and it does not cache errors.

`self_bind_addr()` tries validated IPv4 first, then validated IPv6, and only returns an address accepted by `racc-net`'s `BindPolicy::Tailscale`. This keeps the identity crate and network listener policy in agreement.

## Allowlist persistence and authorization

The caller provides the per-user configuration root; the crate does not choose OS app-data directories or call platform APIs. `allowlist_path(root)` resolves to `root/RaccConnect/allowlist.json`. The versioned JSON contains only stable node ID, optional owner login, label, added/last-seen Unix timestamps, and `Approved`, `Pending`, or `Rejected` state. Addresses, tags, node keys, and secrets are never persisted.

The identity key is `(node_id, owner_login)`. A new identity is queued as `Pending`; callers must treat both `Pending` and `Unknown` as unauthorized until approval. The queue is capped at 128 and the whole list at 512 entries/256 KiB. Labels, IDs, and owners have byte limits. `approve`, `reject`, and `remove` update the persisted policy through an injected store interface.

Writes serialize and validate the entire bounded document, create a unique temporary file beside the target, flush it, then rename it over the destination. Missing files load as an empty list. Invalid JSON, an unsupported schema, invalid entries, or an oversized file is renamed to a `.corrupt-*` sibling; the active in-memory list becomes empty and `CorruptFileQuarantined` is returned for the caller to surface. File-system failures remain typed errors.

## Host-capability discovery

`probe_peer(address, port, timeout, transport)` accepts only a Tailscale address, sends a bounded project `Hello`, waits for a `HelloAck`, and closes the connection without starting a session. A valid `HelloAck` means `HostCapable`, even if its status is `Busy`, `NotAuthorized`, or `UnsupportedVersion`; timeout, connection failure, or another message means `NotHost`. The trait boundary is fakeable, and the TCP implementation uses `racc-net::connect_control` with `BindPolicy::Tailscale`.

The default TCP port is **47473**. The [IANA Service Name and Transport Protocol Port Number Registry](https://www.iana.org/assignments/service-names-port-numbers/service-names-port-numbers.xhtml) listed the containing range 47101–47556 as unassigned when checked on 2026-10-07. This does not prevent a local service or a future assignment from using the port; the application reports the probe unavailable in that case.

### Bounded refresh and app adapter

`PeerDiscovery::refresh()` reads the bounded Tailscale status result, rejects more than 256 peers, and probes at most 64 online peers using at most 8 workers. Connect, write, and read each use a timeout capped at 500 ms (250 ms by default). Only validated Tailscale peer addresses are probed. Probe failures and unprobed peers remain non-host-capable; only a valid project `HelloAck` marks a peer host-capable. The core adapter publishes peer appearance/presence and disappearance events and derives offline IDs from the prior published device snapshot, including when the Tailscale node ID is absent. It also returns validated local IPv4 and IPv6 addresses so the viewer can choose a same-family bind address.

The live app adapter keeps discovery on one worker with one-slot request and result channels, coalesces refresh requests, and refreshes every 15 seconds. The UI-facing event path carries peer metadata only; video remains on the separate frame source. Selection and connection replacement for arbitrary discovered peers still require app `main.rs`/view-model wiring.

## Privacy and verification

All committed fixtures are hand-written with placeholder names and documentation-only IPv4/IPv6 values. Tests scan them for the Tailscale address ranges and `@` markers, exercise missing/extra fields and tagged users, and feed arbitrary byte strings through both JSON parsers. The crate emits no command output or peer data in log messages. It does not send identity data to any service other than the local Tailscale CLI/daemon.

**TESTED-FAKE:** `cargo test --offline -p racc-identity -p racc-core` passes 31 identity tests and 40 core tests, exercising parsing, address validation, status/whois command construction, cache behavior, fake timeouts/caps, allowlist persistence and recovery, peer diffing, bounded fake Hello probes, local address-family filtering, core peer mapping, and disappearance events. `cargo clippy --offline -p racc-identity -p racc-core --all-targets -- -D warnings` passes. The production process runner is also exercised with a child process for timeout and oversized output, including killing/waiting and joining pipe readers. These tests do not prove the Tailscale daemon is installed or responsive.

### Live CLI verification

- [VERIFIED-RUN] Windows PC #1, 2026-10-07, read-only: Tailscale CLI 1.102.4; status returned 2/2 online peers, with aggregate route counts of 0 direct and 2 DERP; self_bind_addr() passed the Tailscale bind policy.
- [VERIFIED-RUN] Across 20 status calls, median/p95 latency was approximately 210/339 ms. Whois succeeded 20/20 times, with median/p95 latency approximately 217/439 ms.
- These are aggregate measurements only. No peer names, addresses, IDs, login names, raw JSON, or DERP region names are retained.

**HUMAN-PENDING:** Windows PC #2 and the Monterey Mac have not had live identity checks recorded. The end-to-end host approval flow also remains pending until M6 exists. Required manual checks:

1. On Windows PC #2, confirm Tailscale is installed and logged in; record version, aggregate online/path counts, successful self-bind validation, and status/whois median/p95 without identifiers.
2. On the Monterey Mac, confirm the app variant and invoke the bundled executable path with TAILSCALE_BE_CLI=1; confirm status --json, whois --json for a peer address, and ip -4 / ip -6, then record only aggregate results and timings.
3. After M6 exists, verify pending approval, approve, reject, and reconnect behavior end-to-end on the real tailnet.

