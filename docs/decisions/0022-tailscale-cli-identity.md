# ADR 0022: Use the documented Tailscale CLI for identity queries

## Status

Accepted

## Context

Peer enumeration, local bind-address selection, and incoming-peer `whois` are needed on Windows, macOS, and Linux-compatible test builds. Tailscale desktop integration differs by platform and installation flavor. In particular, macOS GUI installs bundle their CLI and backend components and do not share an established cross-platform IPC path. The repository has not verified a supported direct LocalAPI pipe/socket endpoint on all target variants. The previous LocalAPI wording described the identity service but left its access mechanism unspecified.

## Decision

Use `tailscale status --json`, `tailscale whois --json <ip>`, `tailscale ip -4` / `-6`, and `tailscale version` through a directly spawned CLI process. Do not use a shell. Clear ambient environment, preserve only required Windows system-root values, and set `TAILSCALE_BE_CLI=1` for macOS. Bound each invocation to two seconds by default and 4 MiB combined output. Parse JSON tolerantly because the CLI documents its JSON output as subject to change.

On macOS, search the App Store bundle executable path before the documented standalone integration path. The latter requires Ventura 13+, while the target Mac is owner-reported as Monterey 12.7.6. Direct LocalAPI access remains deferred until a supported API and path for each desktop install flavor are documented and the measured CLI cost is shown to affect the user experience.

## Evidence

- [Tailscale CLI reference](https://tailscale.com/docs/reference/tailscale-cli): documents `status --json`, `whois --json` (flag before the address), the App Store bundle path, `TAILSCALE_BE_CLI=1`, and that standalone CLI integration requires Ventura 13+.
- [Tailscale daemon overview](https://tailscale.com/docs/reference/tailscaled): describes Windows service and macOS GUI/daemon process differences.
- A read-only probe on Windows PC #1 on 2026-10-07 verified Tailscale CLI version 1.102.4. Status returned 2/2 online peers (aggregate route counts: 0 direct, 2 DERP), self-bind validation succeeded, and 20/20 whois calls succeeded. Across 20 calls, status median/p95 was approximately 210/339 ms and whois median/p95 was approximately 217/439 ms. PC #2 and Mac live behavior remain unverified; no raw JSON or identifiers are recorded.

## Consequences

- The crate remains `#![forbid(unsafe_code)]` and avoids platform-specific IPC code.
- The local CLI/daemon must be installed, running, and logged in; typed errors report absent, stopped, and logged-out states.
- JSON field changes are handled conservatively and must be covered with fixtures when observed.
- Windows PC #1 live CLI verification and aggregate timings are recorded in docs/IDENTITY.md; Windows PC #2 and Monterey Mac checks remain human-pending.
- This ADR supersedes the access-method assumption in ADR 0007 and the previous LocalAPI transport question. Tailscale identity remains the authorization source; no application-layer credentials or network service are introduced.