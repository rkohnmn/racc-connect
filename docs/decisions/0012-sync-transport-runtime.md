# ADR 0012: Synchronous transport runtime

Status: accepted for M2.

`racc-net` uses `std::net`, `std::thread`, and `socket2`; it does not add an async runtime. `socket2` supplies UDP buffer sizing and TCP keepalive. The deterministic testkit implements its seeded xorshift64* generator directly and does not add a direct `rand` dependency. `proptest` remains the only direct `racc-net` development dependency. The `socket2` and platform-support crates used here declare permissive MIT OR Apache-2.0 licensing; those licenses are explicitly allowed in `deny.toml` with GPL and AGPL still excluded.
