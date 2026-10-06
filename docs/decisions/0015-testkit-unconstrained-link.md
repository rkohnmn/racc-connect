# ADR 0015: Unconstrained default for the transport simulator

Status: accepted for M2.5 measurement runs.

The testkit uses an explicit unconstrained default link of `max(10 x nominal video bitrate, 50 Mbps)` and at least 64 MiB of queue capacity. A scenario that supplies a bitrate limit keeps that rate and its configured finite queue unchanged. This makes the normal soak measure loss/reorder/recovery behavior without silently imposing a bottleneck close to the stream bitrate; constrained links remain separately labeled.

The virtual serializer delays arrival until packet transmission completes, and every duplicated copy consumes serialization capacity. This aligns simulated arrivals and queue-wait measurements with the configured link rate. The run matrix showed zero queue drops on the corrected default and tail drops only in explicitly constrained profiles.

No protocol or production transport constant changes are part of this decision.
