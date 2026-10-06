# ADR 0013: Uniform video fragments

Status: accepted for M2.

A frame is split into `ceil(N / 1182)` payloads. Every non-final fragment is exactly 1182 bytes; the final fragment is 1 through 1182 bytes. The receiver validates that rule and consistent KEY, CONFIG, capture timestamp, and fragment count metadata within a frame. This fixes offsets at `frag_idx × 1182` and bounds a frame at `1024 × 1182` bytes. Because v0 is an undeployed draft, the clarification is made in place without a protocol version bump. UDP datagram boundaries and WireGuard authentication keep payload truncation out of the transport threat model.
