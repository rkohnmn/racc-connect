# ADR 0016: Separate topology domain values from wire values

Status: accepted for M3a.

Use validated `Display`, nonzero `DisplayId`, and sorted `Topology` values in `racc-topology`, converting explicitly at the existing `racc-proto::TopologyAnnounce` boundary. Keep revision comparison in u32 serial arithmetic and ignore equal, stale, and half-range-ambiguous updates. This keeps protocol representation out of topology calculations while preserving the fixed v0 wire format.

The stable display identity uses canonical EDID/connector ingredients and deterministic collision resolution. Platform metadata stability remains a hardware-adapter concern.
