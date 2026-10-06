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

### A. Ten-minute deterministic delivery simulation

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

### D. Decision on recovery

Even 0.5% iid packet loss delivered only 8.278% of 480p frames, 1.761% of 720p frames, and 0.372% of 1080p frames in this model; at 2%, the 1080p profile delivered no frame during the ten-minute run. The 233 KB keyframe requires about 198 fragments, so it is particularly unlikely to arrive intact as loss rises. At the tested 0.5% iid loss point, interactive delivery is already unacceptable in this v0 recovery model. Since the next tested point is 0%, these runs do not establish a more precise threshold between 0% and 0.5%. **Recommendation:** evaluate selective retransmission requests for missing keyframe fragments before M7. It directly targets the measured long keyframe recovery intervals; do not add it in M2. XOR parity FEC remains a comparison candidate if M7's measured retransmission delay is too high. These are simulation-backed recommendations, not real-network measurements.

## Verification boundaries

- `[TESTED-FAKE]` Pure transport logic, property tests, the deterministic simulator and loopback sockets ran on Windows 10.0.19045.
- `[COMPILE-ONLY]` Windows and Intel macOS target checks establish type-checking only; they do not establish platform transport behavior.
- `[HUMAN-PENDING]` The two-machine Tailscale path, direct/DERP behavior, tailnet source filtering, Mac sleep timer behavior, and real stream quality remain for the hardware checklist.
