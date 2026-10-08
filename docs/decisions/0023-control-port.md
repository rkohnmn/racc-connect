# ADR 0023: Reserve TCP 47473 for host discovery and control

## Status

Accepted

## Context

The host-capability probe and the project TCP control channel need one stable default port. The repository's protocol and transport stack already provide bounded length-prefixed framing and a bind policy restricted to Tailscale addresses.

## Decision

Use TCP port **47473** as `DEFAULT_CONTROL_PORT`. The IANA Service Name and Transport Protocol Port Number Registry showed port 47473 inside the unassigned range 47101–47556 when checked on 2026-10-07. The probe sends one project `Hello`, accepts a `HelloAck` as proof of host capability, then closes without starting a session.

## Consequences

- The port is unassigned by IANA as of the recorded check, but local collisions and future IANA assignments remain possible.
- Discovery failure or an unexpected peer response is `NotHost`; it does not bypass allowlisting or session authorization.
- The listener must still bind only to a validated Tailscale address and use the normal control protocol bounds.