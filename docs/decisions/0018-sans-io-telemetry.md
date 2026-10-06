# ADR 0018: Sans-I/O telemetry snapshots

Status: accepted for M3a.

Keep telemetry as bounded in-memory state with injected timestamps, no locks, threads, or I/O. Publish immutable cheaply cloned snapshots to the UI at about 4 Hz. Use a fixed 256-event ring and process-wide monotonic event ids so clients can poll with an id cursor without coupling telemetry updates to rendering.

Rate windows, RTT samples, and outstanding pings have explicit fixed bounds. Capture-backend and encoder codes use the existing StatsReport wire values. Decoder is not carried by the current StatsReport, so decoder telemetry stays `Unknown` unless later wire/API work resolves that gap.
