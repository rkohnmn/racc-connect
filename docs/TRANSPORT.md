# M2 transport

Status: implemented and verified with deterministic and loopback tests on Windows 10.0.19045 (Windows PC #1), 2026-10-06. The test evidence is recorded in `docs/PROGRESS.md`. No Windows or Mac hardware streaming and no Tailscale path were exercised.

## Architecture

`racc-net` separates a deterministic sans-I/O policy core from thin socket workers. The core takes caller-supplied monotonic microseconds for reassembly, deadlines, loss windows, keyframe-request backoff, and packet schedules. It does not read the system clock, sleep, open sockets, or depend on a runtime. `VideoSender` and `VideoReceiver` are the I/O shells; they use `std::net`, `std::thread`, and `socket2`. `racc-net` has no unsafe code.

Normal dependencies for `racc-net` are `racc-proto` and `socket2`; `proptest` is its only direct dev dependency. `racc-testkit` depends only on `racc-net` and `racc-proto`. The simulator owns a seeded xorshift64* generator; it does not add a direct `rand` dependency. The permitted `proptest` dev dependency brings its own transitive test-only random crates, which are absent from the normal `racc-net` tree.

## Wire fragmentation and consistency

The video header is 18 bytes, `MAX_DATAGRAM` is 1200 bytes, and each payload is at most 1182 bytes. A nonempty frame of at most `1024 × 1182 = 1,210,368` bytes is sliced into `ceil(frame_len / 1182)` fragments. Every non-final fragment has exactly 1182 payload bytes; the final fragment has 1–1182 bytes. The receiver derives offsets as `frag_idx × 1182` and rejects a short non-final fragment or an empty final fragment.

Within one `(epoch, frame_id)`, `frag_cnt`, KEY, CONFIG, and `capture_ts_us` must agree. Any mismatch drops the complete frame and increments `dropped_inconsistent`. Datagram parsing borrows payload bytes. The caller supplies datagram buffers to the slicer, so it does not allocate one buffer per fragment.

UDP preserves datagram boundaries. WireGuard authenticates tunnel datagrams, so truncation in transit is outside the v0 threat model. A shortened final payload remains structurally valid because v0 has no full-frame byte length; this transport does not claim to detect that condition.

## Reassembler state and transitions

The reassembler retains the current epoch, a processed frame watermark, the last delivered frame, the `need_keyframe` flag and retry timer, up to four partial/reorder-held frames, and rolling loss buckets. `reset(epoch, now)` clears epoch-local frames and watermarks, enters keyframe-wait state, resets request backoff, and emits a keyframe request. Other-epoch packets are counted and dropped.

1. A parsed fragment is rejected as late if its serial `frame_id` is at or before the processed watermark. Frame comparison uses wrapping `u32` serial arithmetic with the half-range rule.
2. A new valid fragment creates a partial frame. Fragment payload storage is allocated only as fragments arrive. Repeated fragment indexes are counted and ignored.
3. Metadata disagreement, an invalid uniform fragment size, a 100 ms idle timeout, a frame-cap eviction, or a byte-budget eviction drops the affected partial and requires a keyframe. The oldest serial frame is evicted first.
4. A complete frame is retained in the ordered-completion set. Delivery is in increasing frame ID order. A newer completed frame waits up to the 8 ms reorder window while an older partial is still active. An older partial with no activity inside that window is declared lost. A whole-frame gap is declared when no older partial explains it.
5. Declaring a gap requires a keyframe. A predictive frame after a gap is dropped; a keyframe can re-establish decode state. While waiting for a keyframe, predictive frames are counted and dropped. Delivering a keyframe clears older state, advances the watermark, exits keyframe-wait state, and resets request backoff.
6. The first request is immediate when a keyframe is needed and none is already in flight. Unanswered retries are at least 200 ms apart and back off 200, 400, 800, then 1000 ms; 1000 ms is the cap. Delivering a keyframe resets the timer. The event carries the epoch for the caller to encode as `RequestKeyframe`.

The reassembler has a maximum of four in-flight frames and a configurable payload budget capped at 4 MiB. During completion, the additional assembly buffer is included in the budget check. The frame cap and byte cap are asserted under random hostile headers. Reassembled output is an owned `Vec<u8>` in v0; frame pooling is deferred.

The rolling loss estimator reports missing-fragment fraction for frames with a known fragment count and whole-frame loss fraction for identifier gaps. Its default window is 2 seconds. To keep memory bounded under an unusually high datagram rate, observations are aggregated into at most 256 time buckets (plus the partially overlapping boundary bucket).

Counters include received datagrams/bytes, parse errors, wrong epochs, duplicates, late fragments, completed/delivered frames, incomplete/waiting-key/inconsistent/evicted/gap drops, emitted keyframe requests, and current in-flight frame/byte totals.

## Sender and backpressure

The pure slicer fills caller-owned buffers. The pacer places datagrams evenly across at most 60% of the supplied frame interval (33,333 µs by default). A new frame starts at the later of its arrival time and the prior schedule end, so a faster arrival does not compress the previous schedule. Keyframes use the same pacing rule.

The socket sender has one active frame and at most two unstarted frames queued. On overflow, it removes the oldest unstarted frame, returns `QueueDropped` to that submitter, emits `ForceKeyframe` with the dropped frame identity, and queues the new frame. It does not silently abandon an active frame. The worker sleeps through the injectable `Sleeper` trait; the production implementation calls `std::thread::sleep` and does not busy-spin.

## TCP control connection

`ControlListener::bind` and `connect_control` validate addresses before socket I/O. The connection enables `TCP_NODELAY`, TCP keepalive with a 10-second idle value, a 2-second default write timeout, and a configurable read timeout (5 seconds by default). `send` uses `racc-proto`'s bounded encoder. `recv` uses its incremental `FrameDecoder`, so an oversized four-byte length is rejected before its body is buffered. `split` clones the stream into independent read and write halves. Errors are typed as I/O, protocol, timeout, closed, or bind-policy errors; heartbeat policy remains outside M2.

## Bind and feature policy

Production sockets accept only IPv4 `100.64.0.0/10` and IPv6 `fd7a:115c:a1e0::/48`. Unspecified, loopback, link-local, private LAN/ULA outside the Tailscale prefix, and all other addresses have distinct errors. `test-bind` is off by default and adds the loopback-only `TestOnlyLoopback` policy. Only `racc-testkit` enables it. `scripts/check-features.sh` and `.ps1` inspect feature trees for `racc-app` and `racc-host-agent`; both are wired into `check-all`.

## Real I/O and MTU

`VideoSender` uses a UDP send buffer requested at 1 MiB. `VideoReceiver` requests a 2 MiB receive buffer, reuses one fixed 2048-byte receive array, and polls at most every 5 ms. It filters by the expected host IP and sends events through a bounded 64-item channel. Receiver and sender worker threads are explicitly closed and joined; the loopback tests measured each shutdown below the 1-second bound. The testkit proxy uses one loopback UDP socket and its seeded loss/duplicate/reorder profile.

The 18-byte header plus at most 1182 payload bytes keeps every video datagram at or below 1200 bytes, under the 1280-byte tunnel MTU. DF/PMTU probing is deferred.

## Constants and rationale

| Constant | Value | Purpose |
|---|---:|---|
| `MAX_DATAGRAM` | 1200 bytes | Safe working datagram ceiling under the Tailscale MTU |
| Video header | 18 bytes | v0 wire header |
| `MAX_VIDEO_PAYLOAD` | 1182 bytes | 1200 minus 18 |
| `MAX_FRAGMENTS_PER_FRAME` | 1024 | Bounds one encoded access unit |
| `MAX_INFLIGHT_FRAMES` | 4 | Limits partial and reorder-held frames |
| `MAX_INFLIGHT_BYTES` | 4 MiB | Bounds payload and assembly retention |
| `REASSEMBLY_TIMEOUT_MS` | 100 ms | Drops an idle partial frame |
| `REORDER_WINDOW_MS` | 8 ms | Tolerates short path reordering |
| Keyframe retry minimum / cap | 200 / 1000 ms | Limits repeated IDR requests |
| `LOSS_WINDOW_US` | 2,000,000 µs | Default rolling loss window |
| `MAX_LOSS_BUCKETS` | 256 | Bounds loss-history memory |
| Frame interval / pacing fraction | 33,333 µs / 60% | Default 30 fps spread |
| `SEND_QUEUE_MAX_FRAMES` | 2 waiting frames | Latest-wins sender backpressure |
| Sender / receiver socket buffers | 1 MiB / 2 MiB | Requested kernel buffering |
| Receiver poll / buffer | 5 ms / 2048 bytes | Bounded receive wakeup and fixed storage |
| Receiver event channel | 64 events | Bounded handoff to the caller |
| Force-keyframe event channel | 2 events | Bounded sender backpressure signals |
| TCP_NODELAY | enabled | Disables Nagle for control messages |
| TCP keepalive idle | 10 s | Starts operating-system keepalive probes |
| TCP connect / write timeout | 2 s / 2 s | Bounds connection setup and complete writes |
| TCP read timeout | 5 s default, configurable | Bounds each blocking receive attempt |
| Worker join bound | 1000 ms | Explicit close budget |
| Testkit simulated queue | 4 MiB | Finite virtual-network retention |

The constant values and protocol header relationships are asserted by tests in `racc-net` and `racc-proto`.

## Measurements

Labels use the `docs/DEV_SETUP.md` vocabulary. All measurements below are `[VERIFIED-RUN]` on Windows 10.0.19045, this Codex machine matching Windows PC #1, on 2026-10-06. They are deterministic simulation or loopback measurements, not tailnet or GPU results.

### A. Corrected ten-minute deterministic delivery simulation

[VERIFIED-RUN] Each row represents 600 virtual seconds at 30 fps (18,000 source frames). Unconstrained rows use a link rate of `max(10 x tier bitrate, 50 Mbps)` with a 64 MiB queue; no rate or queue drops occurred. IID profiles used 1 ms one-way delay, +/-100 us jitter, 3% packet reordering with one-packet displacement, and 0.1% duplication. The burst row uses the recorded Gilbert-Elliott profile and seed. Explicit constrained cases retain their configured rate and queue. Keyframe-response request path uses the profile one-way delay, 10 ms encode time, and the next 30 fps encode opportunity. The stream simulator records stale time beyond one 33.333 ms frame interval. Freeze columns are median / p95 / mean stale interval lengths. Queue delay is mean / max serializer waiting time; queue drops are included for constrained rows. Each seed is shown.

| Link case | Tier | Loss | Keyframe | Seed | Delivered frames | Requests/min | Freeze median / p95 / mean (ms) | Stale picture | Peak reassembler bytes | Queue delay mean / max (ms) | Queue drops |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| unconstrained | 480p30 | 0.00% iid | 8x | 8409 | 100.000% | 0.00 | 0.065 / 0.900 / 0.124 | 0.1838% | 49,644 | 0.0001 / 0.1920 | 0 |
| unconstrained | 480p30 | 0.10% iid | 8x | 9009 | 98.306% | 12.00 | 0.064 / 0.965 / 1.260 | 1.8707% | 59,796 | 0.0002 / 0.1920 | 0 |
| unconstrained | 480p30 | 0.25% iid | 8x | 10525 | 95.811% | 30.70 | 0.065 / 1.067 / 2.990 | 4.3570% | 66,548 | 0.0002 / 0.1920 | 0 |
| unconstrained | 480p30 | 0.50% iid | 8x | 13137 | 89.172% | 57.80 | 0.068 / 66.561 / 8.008 | 10.9744% | 60,638 | 0.0002 / 0.1920 | 0 |
| unconstrained | 480p30 | 1.00% iid | 8x | 1993 | 70.578% | 113.00 | 0.074 / 66.827 / 26.576 | 29.5388% | 60,638 | 0.0002 / 0.1920 | 0 |
| unconstrained | 480p30 | 2.00% iid | 8x | 28409 | 29.067% | 136.00 | 0.089 / 700.021 / 146.148 | 70.9794% | 65,706 | 0.0002 / 0.1920 | 0 |
| unconstrained | 480p30 | 5.00% iid | 8x | 58249 | 1.950% | 81.10 | 0.188 / 13501.005 / 2711.034 | 98.0491% | 66,192 | 0.0002 / 0.1920 | 0 |
| unconstrained | 720p30 | 0.00% iid | 8x | 10409 | 100.000% | 0.00 | 0.063 / 0.903 / 0.122 | 0.1816% | 115,836 | 0.0002 / 0.1920 | 0 |
| unconstrained | 720p30 | 0.10% iid | 8x | 11073 | 96.072% | 27.20 | 0.065 / 1.043 / 2.843 | 4.0946% | 143,067 | 0.0003 / 0.3480 | 0 |
| unconstrained | 720p30 | 0.25% iid | 8x | 8557 | 87.333% | 64.50 | 0.070 / 66.685 / 9.725 | 12.8327% | 143,067 | 0.0005 / 0.1920 | 0 |
| unconstrained | 720p30 | 0.50% iid | 8x | 15137 | 61.939% | 122.50 | 0.075 / 266.626 / 39.912 | 38.1823% | 156,468 | 0.0006 / 0.2630 | 0 |
| unconstrained | 720p30 | 1.00% iid | 8x | 4025 | 22.117% | 129.60 | 0.090 / 1499.873 / 222.328 | 77.9260% | 157,251 | 0.0007 / 0.3470 | 0 |
| unconstrained | 720p30 | 2.00% iid | 8x | 26249 | 2.383% | 81.60 | 0.147 / 13499.900 / 2460.930 | 97.6169% | 157,251 | 0.0006 / 0.3230 | 0 |
| unconstrained | 720p30 | 5.00% iid | 8x | 60409 | 0.044% | 60.90 | 66566.102 / 409974.851 / 133776.770 | 89.1845% | 157,251 | 0.0004 / 0.1920 | 0 |
| unconstrained | 1080p30 | 0.00% iid | 8x | 15965 | 100.000% | 0.00 | 0.069 / 0.932 / 0.138 | 0.2051% | 232,854 | 0.0017 / 7.3250 | 0 |
| unconstrained | 1080p30 | 0.10% iid | 8x | 15797 | 92.717% | 43.90 | 0.072 / 1.160 / 5.651 | 7.8852% | 288,498 | 0.5955 / 7.4630 | 0 |
| unconstrained | 1080p30 | 0.25% iid | 8x | 14233 | 63.344% | 115.90 | 0.080 / 108.128 / 39.377 | 37.5263% | 316,482 | 1.2603 / 7.6010 | 0 |
| unconstrained | 1080p30 | 0.50% iid | 8x | 11733 | 21.294% | 131.60 | 0.103 / 1507.208 / 247.417 | 79.2560% | 316,482 | 1.3627 / 7.4630 | 0 |
| unconstrained | 1080p30 | 1.00% iid | 8x | 6477 | 2.567% | 80.50 | 0.151 / 13540.376 / 2397.581 | 97.1020% | 316,482 | 0.9388 / 7.4630 | 0 |
| unconstrained | 1080p30 | 2.00% iid | 8x | 28797 | 0.122% | 62.80 | 13271.151 / 118564.990 / 31045.021 | 67.2642% | 316,482 | 0.7425 / 7.3910 | 0 |
| unconstrained | 1080p30 | 5.00% iid | 8x | 64781 | 0.000% | 60.20 | 599994.000 / 599994.000 / 599994.000 | 99.9990% | 316,482 | 0.6302 / 7.0490 | 0 |
| burst_GE | 720p30 | burst GE (738 profile losses) | 8x | 65261 | 96.672% | 21.00 | 0.066 / 1.024 / 2.429 | 3.5054% | 151,740 | 0.0003 / 0.1920 | 0 |
| unconstrained | 720p30 | 0.50% iid | 4x | 15141 | 79.206% | 110.80 | 0.074 / 66.750 / 16.722 | 20.9329% | 85,503 | 0.0002 / 0.1920 | 0 |
| unconstrained | 720p30 | 1.00% iid | 4x | 4029 | 46.239% | 172.90 | 0.090 / 300.042 / 70.403 | 53.8350% | 98,919 | 0.0002 / 0.1920 | 0 |
| unconstrained | 720p30 | 2.00% iid | 4x | 26253 | 14.411% | 153.70 | 0.133 / 2499.887 / 326.743 | 85.6067% | 99,687 | 0.0002 / 0.1920 | 0 |
| unconstrained | 1080p30 | 0.50% iid | 4x | 11729 | 48.617% | 165.20 | 0.093 / 299.929 / 64.194 | 51.4730% | 200,172 | 0.0002 / 0.1380 | 0 |
| unconstrained | 1080p30 | 1.00% iid | 4x | 6473 | 13.250% | 145.50 | 0.138 / 2500.010 / 359.574 | 86.7172% | 200,202 | 0.0002 / 0.1380 | 0 |
| unconstrained | 1080p30 | 2.00% iid | 4x | 28793 | 1.900% | 88.90 | 299.982 / 12499.858 / 2452.987 | 94.8488% | 199,818 | 0.0002 / 0.2100 | 0 |
| constrained_2x_100KB | 720p30 | 0.50% iid | 8x | 7009505 | 0.000% | 60.10 | 599994.000 / 599994.000 / 599994.000 | 99.9990% | 140,658 | 15.1791 / 96.1790 | 9373 |
| constrained_2x_100KB | 720p30 | 2.00% iid | 8x | 6990153 | 0.000% | 60.10 | 599994.000 / 599994.000 / 599994.000 | 99.9990% | 140,658 | 15.1531 / 96.1790 | 8459 |
| constrained_5Mbps_150KB | 720p30 | 0.50% iid | 8x | 5009505 | 26.411% | 99.90 | 0.132 / 2724.139 / 517.764 | 78.7002% | 156,468 | 97.1231 / 221.1000 | 13098 |
| constrained_5Mbps_150KB | 720p30 | 2.00% iid | 8x | 4992457 | 2.106% | 76.60 | 5719.049 / 19733.220 / 7343.064 | 97.9075% | 156,468 | 70.1460 / 221.3290 | 5204 |

The 4x keyframe sensitivity rows are explicitly labeled `4x`; the primary matrix uses `8x`. Zero-loss rows show about 0.18-0.21% stale time from timer phase and packetization, while all 18,000 frames arrive. The constrained 2x / 100 KB case drops the full stream under both tested loss rates: the keyframe burst exceeds the link service rate and repeatedly overfills its queue. At 5 Mbps / 150 KB, 720p 0.5% delivers 26.411% with 78.7002% stale time, and 2% delivers 2.106% with 97.9075% stale time; mean serializer queue delay is 97.1231 ms and 70.1460 ms, respectively. These cases remain separate from the unconstrained baseline.

### Superseded M2 original results (flawed link model)

Each run simulated 600 seconds at 30 fps (18,000 frames), a startup keyframe, and an IDR response at the next encode opportunity after a modeled 20 ms RTT/2 plus 10 ms encode delay. Average P-frame size was `bitrate / 8 / 30`; a keyframe was modeled at 8× that size. For an unset rate limiter the simulator allowed 102% of the video bitrate for UDP/protocol headers and duplicate copies. The profiles used 1 ms one-way delay, ±100 µs uniform jitter, 3% reordering with one-packet displacement, 0.1% duplication, a 4 MiB finite queue, and seeded xorshift64*. The iid runs used these exact configured packet-loss rates:

| Tier | Loss | Delivered frames | Keyframe requests/min | Recovery median / p95 (ms) | Stale picture (%) | Peak reassembler bytes |
|---|---:|---:|---:|---:|---:|---:|
| 480p30, 1.5 Mbps | 0% | 100.000% | 0.00 | — | 0.001 | 49,644 |
| 480p30, 1.5 Mbps | 0.5% | 8.278% | 61.70 | 963.64 / 3,673.65 | 80.118 | 54,728 |
| 480p30, 1.5 Mbps | 1% | 3.639% | 66.20 | 929.34 / 2,886.60 | 85.620 | 59,796 |
| 480p30, 1.5 Mbps | 2% | 1.183% | 67.20 | 1,281.23 / 6,551.91 | 86.182 | 65,366 |
| 480p30, 1.5 Mbps | 5% | 0.156% | 63.00 | 9,819.12 / 41,086.57 | 77.240 | 63,682 |
| 720p30, 3.5 Mbps | 0% | 100.000% | 0.00 | — | 0.001 | 115,836 |
| 720p30, 3.5 Mbps | 0.5% | 1.761% | 62.20 | 1,590.10 / 2,678.18 | 93.378 | 143,421 |
| 720p30, 3.5 Mbps | 1% | 0.283% | 62.50 | 3,333.46 / 7,265.79 | 94.873 | 156,468 |
| 720p30, 3.5 Mbps | 2% | 0.139% | 61.70 | 7,724.45 / 17,769.12 | 92.100 | 155,286 |
| 720p30, 3.5 Mbps | 5% | 0.000% | 60.50 | no recovery observed | 99.999 | 155,286 |
| 1080p30, 7 Mbps | 0% | 100.000% | 0.00 | — | 0.001 | 231,672 |
| 1080p30, 7 Mbps | 0.5% | 0.372% | 61.00 | 2,216.26 / 10,397.55 | 96.466 | 286,134 |
| 1080p30, 7 Mbps | 1% | 0.000% | 60.40 | no recovery observed | 99.999 | 312,936 |
| 1080p30, 7 Mbps | 2% | 0.000% | 60.40 | no recovery observed | 99.999 | 314,118 |
| 1080p30, 7 Mbps | 5% | 0.000% | 60.20 | no recovery observed | 99.999 | 312,936 |
| 720p30 burst profile | Gilbert–Elliott | 7.144% | 58.90 | 928.94 / 3,585.16 | 88.210 | 128,484 |

At 0% iid loss, all three tiers delivered all 18,000 frames and emitted zero keyframe requests; measured stale time was 0.001% or less, inside the test's 0.01% bound. All profiles had zero gap-safety violations, bounded frame/byte state, and keyframe-request intervals of at least 200 ms. The largest measured in-flight payload was 314,118 bytes, below the 4 MiB limit.

### B. Real sender pacing

A 233,328-byte encoded keyframe (198 datagrams, 236,892 bytes including headers) was sent through `VideoSender` over Windows loopback. The measured first-to-last send duration was 20,167 µs; maximum adjacent-send gap was 1,067 µs; maximum observed sleep call was 1,063 µs. The pacer target was 19,999 µs (60% of 33,333 µs). The measured duration exceeded that target by 168 µs. The loopback test consumes receiver events concurrently and is serialized with the CPU-heavy virtual soak, keeping the bounded event queue and host UDP receive queue from overflowing under test load; its zero-loss delivery assertion remains 300/300. This is one Windows loopback observation; no macOS timer result is claimed.

### C. Reassembly cost and memory

A release-mode single-thread micro-run processed 100,000 pre-encoded one-fragment video datagrams in 29.088 ms: **3,437,844 datagrams/second on one worker thread** on the Windows PC #1 machine. The largest in-flight payload observed across the simulation matrix was 314,118 bytes. This micro-run is a local CPU measurement, not an end-to-end stream-rate guarantee.

### D. Updated conclusion on recovery

The corrected unconstrained 8x matrix delivered, at 0.5% / 1% / 2% iid datagram loss: 480p 89.172% / 70.578% / 29.067%, 720p 61.939% / 22.117% / 2.383%, and 1080p 21.294% / 2.567% / 0.122%. Stale-picture percentages at those points were 10.974% / 29.539% / 70.979%, 38.182% / 77.926% / 97.617%, and 79.256% / 97.102% / 67.264%, respectively. The final 1080p/2% stale figure reflects a late delivered frame near the end of this fixed-seed run; it must be read with delivered percent and freeze percentiles.

For a concrete v0 acceptance threshold, define acceptable as at least 90% frames delivered and at most 10% stale time. All three tiers meet it at 0.1% iid loss. 480p also meets it at 0.25%; 720p and 1080p do not. No tier meets it at 0.5%. Thus the common all-tier v0 operating range is 0-0.1% iid loss on this synthetic unconstrained path, with 480p conditionally extending to 0.25%. This is simulation evidence only, not a Tailscale guarantee.

The simulator-only comparison favors the hybrid NACK-all+FEC-20 as the M7 candidate after the earlier topology and UI milestones. At 720p/2%/1 ms one-way, it delivered 100% with 1.389% stale time, 8.742% overhead and 1.538 ms median added latency, against baseline 2.467% delivered and 95.550% stale. At 720p/0.5%/20 ms it delivered 100% with 0.834% stale and 8.277% overhead, against baseline 59.411% and 40.566%. NACK-all alone uses less bandwidth (2.460% overhead at 720p/2%/1 ms) but its 48.622% stale time at 720p/2%/20 ms misses the 10% threshold. The 1080p/2%/20 ms hybrid row still has 17.875% stale time. These remain simulator-only comparisons; no NACK or FEC implementation is included here.

## Verification boundaries

- `[TESTED-FAKE]` Pure transport logic, property tests, the deterministic simulator and loopback sockets ran on Windows 10.0.19045.
- `[COMPILE-ONLY]` Windows and Intel macOS target checks establish type-checking only; they do not establish platform transport behavior.
- `[HUMAN-PENDING]` The two-machine Tailscale path, direct/DERP behavior, tailnet source filtering, Mac sleep timer behavior, and real stream quality remain for the hardware checklist.
