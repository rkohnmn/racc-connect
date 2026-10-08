# ADR 0007: Rely on Tailscale identity and peer allowlisting

## Status

Accepted

## Context

The native protocol runs only over the private Tailscale network.

## Decision

Bind only to the Tailscale interface address, rely on WireGuard for transport authentication and encryption, and do not add application-layer encryption. Check incoming peers with Tailscale-provided `whois` identity against a host-side allowlist. The CLI-first access method is specified in ADR 0022.

## Consequences

Unknown peers are rejected until approved by the host. There are no passwords or public certificate infrastructure.