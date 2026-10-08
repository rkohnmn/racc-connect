# ADR 0049: Initial quality-adaptation policy

## Status

Accepted as a starting policy; thresholds remain provisional pending real M7 network measurements.

## Decision

The host owns adaptation within 480p30 to 1080p30, capped by host and display capability. Use these initial thresholds:

| Signal | Initial response |
|---|---|
| Packet loss above 2% or frame loss above 5% for 3 seconds | Reduce bitrate in 10% steps, then step down one tier while pressure persists |
| RTT above 3x the established baseline for 5 seconds | Same staged reduction as sustained loss |
| Two sender-queue overflows within 2 seconds | In the same ordered action set, apply a 70% bitrate target then step down one tier immediately |
| Stable recovery: packet and frame loss below 0.5%, RTT at most 1.5x baseline, no recent overflow | Restore bitrate gradually; require 20 stable seconds before a tier step-up and at least 30 seconds between automatic step-ups |

Default tier bitrate targets are 1.5, 3.5, and 7 Mbps for 480p30, 720p30, and 1080p30. Preserve the user-selected fixed tier while still allowing bitrate trims. Do not infer an adaptation threshold from decoder time or encoder lag until real measurements support one.

## Evidence and limits

Sustained loss/RTT trims bitrate in 10% steps once per second to the 70% floor before a tier step on the next feedback sample; queue overflow takes the immediate same-cycle path above. The current policy and host wiring have deterministic synthetic/loopback coverage only. No threshold is calibrated from a real Tailscale route, and no real encoder application or frame pacing result is available. M7 real-network evidence is still pending; review these starting thresholds after the Windows-to-Mac and Windows-to-Windows sessions and record measured observations before changing them.
