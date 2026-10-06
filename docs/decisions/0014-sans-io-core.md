# ADR 0014: Sans-I/O transport core and test binding

Status: accepted for M2.

The slicer, reassembler, pacer, queue, loss estimator, keyframe policy, and bind policy are deterministic core logic. Callers pass monotonic microseconds; only thin socket workers use system time, threads, and I/O. Returning completed frames as owned buffers is accepted for v0; frame pooling is deferred. Loopback is allowed only through the opt-in `test-bind` feature, enabled by `racc-testkit` and checked absent from `racc-app` and `racc-host-agent` by both feature-gate scripts.
