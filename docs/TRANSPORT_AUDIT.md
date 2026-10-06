# M2.5 Transport Audit

Status: audit and measurement work complete; all local acceptance checks pass. The normal push is BLOCKED-HUMAN because this goal forbids network access. The corrected real-UDP loopback test passed in the final full workspace and both check-all runs. See question 18 and the final acceptance report in docs/PROGRESS.md. No M3 work is included.

## Phase 1 — analytic baseline

This section is an analytic prediction, not a packet simulation or a hardware measurement.

### Assumptions and formulas

- Tiers are 30 fps at 1.5, 3.5, and 7 Mbps. Mean P-frame bytes are B / 8 / 30: 6,250, 14,583, and 29,166. The normal keyframe is 8x P-frame size; 4x is a sensitivity case.
- Payload per datagram is 1,182 bytes: n_p = ceil(P_bytes / 1182), n_k = ceil(K_bytes / 1182).
- For IID packet loss L, P-frame success is p_p = (1-L)^n_p and one keyframe attempt succeeds with s_k = (1-L)^n_k.
- Loss events while decoding predictive frames are lambda = 30 * (1-p_p) per second. This omits losses during recovery and is an optimistic renewal approximation.
- Loss declaration is 8 ms. The request path is 1 ms one-way, encode delay 10 ms, mean wait to the next 30 fps encode opportunity 16.667 ms, and keyframe pacing span 20 ms. One attempt therefore costs 47.667 ms after declaration. Keyframe datagram serialization at the corrected default link is shorter than its 20 ms pacing span.
- Let q = 1-s_k. Expected retry delay is 200q + 400q^2 + 800q^3 + 1000q^4/s_k ms. This includes the 200/400/800/1000 ms backoff sequence and repeated 1000 ms attempts after the cap. Expected recovery time after loss declaration is R = 8 + 47.667/s_k + retry_delay.
- The last displayed image is already up to one frame interval old when the missing P-frame is declared. Approximate total displayed-picture freeze as F = 33.333 + R ms. Delivered-frame fraction is 1 / (1 + lambda*F/1000). Stale-picture fraction counts only time beyond one frame interval: delivered_fraction * lambda*R/1000. This accounts for the age of the displayed image at loss declaration; it is still an alternating-renewal approximation, not an exact frame scheduler.

### Predictions for 8x keyframes

| Tier | IID loss | n_p | P success | n_k | Key success | Loss events/s | Total freeze/event (ms) | Stale picture | Delivered frames |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 480p30 | 0.10% | 6 | 99.40% | 43 | 95.79% | 0.180 | 100.3 | 1.18% | 98.23% |
| 480p30 | 0.25% | 6 | 98.51% | 43 | 89.80% | 0.447 | 120.0 | 3.68% | 94.91% |
| 480p30 | 0.50% | 6 | 97.04% | 43 | 80.61% | 0.889 | 161.9 | 9.99% | 87.42% |
| 480p30 | 1.00% | 6 | 94.15% | 43 | 64.91% | 1.756 | 292.1 | 30.03% | 66.10% |
| 480p30 | 2.00% | 6 | 88.58% | 43 | 41.95% | 3.425 | 833.1 | 71.08% | 25.95% |
| 480p30 | 5.00% | 6 | 73.51% | 43 | 11.02% | 7.947 | 7,221.9 | 97.83% | 1.71% |
| 720p30 | 0.10% | 13 | 98.71% | 99 | 90.57% | 0.388 | 117.1 | 3.11% | 95.66% |
| 720p30 | 0.25% | 13 | 96.80% | 99 | 78.05% | 0.961 | 177.0 | 11.79% | 85.47% |
| 720p30 | 0.50% | 13 | 93.69% | 99 | 60.88% | 1.893 | 345.4 | 35.72% | 60.47% |
| 720p30 | 1.00% | 13 | 87.75% | 99 | 36.97% | 3.674 | 1,082.3 | 77.45% | 20.09% |
| 720p30 | 2.00% | 13 | 76.90% | 99 | 13.53% | 6.929 | 5,513.5 | 96.86% | 2.55% |
| 720p30 | 5.00% | 13 | 51.33% | 99 | 0.62% | 14.600 | 165,564.7 | 99.94% | 0.04% |
| 1080p30 | 0.10% | 25 | 97.53% | 198 | 82.03% | 0.741 | 154.2 | 8.04% | 89.74% |
| 1080p30 | 0.25% | 25 | 93.93% | 198 | 60.92% | 1.820 | 344.9 | 34.83% | 61.44% |
| 1080p30 | 0.50% | 25 | 88.22% | 198 | 37.07% | 3.533 | 1,076.9 | 76.74% | 20.81% |
| 1080p30 | 1.00% | 25 | 77.78% | 198 | 13.67% | 6.665 | 5,438.8 | 96.72% | 2.68% |
| 1080p30 | 2.00% | 25 | 60.35% | 198 | 1.83% | 11.896 | 54,697.0 | 99.79% | 0.15% |
| 1080p30 | 5.00% | 25 | 27.74% | 198 | 0.00% | 21.678 | 26,971,693.9 | 100.00% | 0.00% |

### Four-times-keyframe sensitivity

| Tier | IID loss | n_k (4x) | Key success | Total freeze/event (ms) | Stale picture | Delivered frames |
|---|---:|---:|---:|---:|---:|---:|
| 480p30 | 0.10% | 22 | 97.82% | 94.6 | 1.08% | 98.33% |
| 480p30 | 0.25% | 22 | 94.64% | 103.7 | 3.01% | 95.57% |
| 480p30 | 0.50% | 22 | 89.56% | 120.8 | 7.02% | 90.30% |
| 480p30 | 1.00% | 22 | 80.16% | 164.4 | 17.85% | 77.60% |
| 480p30 | 2.00% | 22 | 64.12% | 301.8 | 45.21% | 49.18% |
| 480p30 | 5.00% | 22 | 32.35% | 1,401.9 | 89.58% | 8.24% |
| 720p30 | 0.10% | 50 | 95.12% | 102.3 | 2.57% | 96.19% |
| 720p30 | 0.25% | 50 | 88.24% | 125.9 | 7.94% | 89.21% |
| 720p30 | 0.50% | 50 | 77.83% | 178.4 | 20.52% | 74.76% |
| 720p30 | 1.00% | 50 | 60.50% | 351.1 | 50.98% | 43.67% |
| 720p30 | 2.00% | 50 | 36.42% | 1,115.6 | 85.90% | 11.45% |
| 720p30 | 5.00% | 50 | 7.69% | 11,250.2 | 99.10% | 0.61% |
| 1080p30 | 0.10% | 99 | 90.57% | 117.1 | 5.71% | 92.01% |
| 1080p30 | 0.25% | 99 | 78.05% | 177.0 | 19.78% | 75.64% |
| 1080p30 | 0.50% | 99 | 60.88% | 345.4 | 49.66% | 45.03% |
| 1080p30 | 1.00% | 99 | 36.97% | 1,082.3 | 85.12% | 12.17% |
| 1080p30 | 2.00% | 99 | 13.53% | 5,513.5 | 97.90% | 1.50% |
| 1080p30 | 5.00% | 99 | 0.62% | 165,564.7 | 99.95% | 0.03% |

### Comparison with the original M2 measurements

The original M2 480p/0.5% row reported 8.278% delivered and 80.118% stale; 720p/0.5% reported 1.761% and 93.378%. The corrected analytic envelope predicts 87.42% / 9.99% for 480p and 60.47% / 35.72% for 720p. Delivery therefore differed by an order of magnitude for 480p and by 34x for 720p in the original run. The old stale values were additionally affected by the stale-total truncation defect described under H6.

## Phase 2 — diagnosis

### Deterministic legacy trace

`docs/TRANSPORT_TRACE_M2_5.txt` records the first 20 lost datagrams for 480p, 0.5% iid, seed 5716, using the explicit legacy link rate 1.53 Mbps, 4 MiB queue, and 20 ms feedback-path override to reproduce the original bottleneck. Tracing is opt-in and ordinary simulation calls remain silent. The full trace records lost fragments, loss declarations, NeedKeyframe state changes, request backoff steps and sender arrival, keyframe flags/configuration and fragment receipts, completion, delivery, and keyframe-state clearing.

Trace summary: 607 profile-loss datagrams, 18,174 queue drops, 4,194,302-byte peak link queue, 7.406% delivered, and 92.569% stale after stale-accounting correction; 79/79 completed keyframes were delivered. Reassembly peak was 60,638 B. Keyframe-specific counters report 522 partial-key drops associated with reorder expiry, zero timeouts, zero cap evictions, zero superseded, and zero inconsistent frames. A representative excerpt is:

```text
seed=5716; loss=0.5%; first 20 loss events
first lost fragment: t=1970.646 ms, P frame 59, fragment 2/6
loss declared:      t=2485.515 ms, incomplete=1, evicted=0, inconsistent=0
request emitted:    t=2485.515 ms, backoff step 1
request at sender:  t=2505.515 ms
keyframe sent:      t=2533.308 ms, frame 76, 50,000 B, 43 fragments, KEY=1 CONFIG=1
retry request:      t=2686.640 ms, before response fragment 1 at t=2990.452 ms
keyframe complete:  t=3249.645 ms, frame 76, 43/43 fragments
need_keyframe=false and delivery: t=3249.645 ms, frame 76
```

The response keyframe took 716.337 ms from sender schedule to complete delivery and its first fragment arrived 457.144 ms after send. The first loss event took 514.869 ms to declare because the configured link queue delayed the evidence used by the reorder/loss window. These durations are from the intentionally constrained legacy profile; they are not expected unconstrained-path latency.

### H1-H7 hypothesis verdicts

| Hypothesis | Verdict | Evidence |
|---|---|---|
| H1: 102% average-bitrate bottleneck delays keyframes and following frames | **CONFIRMED** | The old default assigned 1.02 x media bitrate. The seeded trace filled the 4 MiB queue to 4,194,302 B and dropped 18,174 datagrams after 607 profile losses. A 50 KB 480p keyframe took 716 ms to complete after being queued behind earlier traffic. |
| H2: completed keyframes are withheld by ordering, watermark, late-drop, or epoch rules | **REFUTED** | The trace counts 79 completed and 79 delivered keyframes. Added counters distinguish completion from delivery; the reassembler regression tests for late partial frames and watermark behavior pass. No completed-but-undelivered keyframe appeared. |
| H3: keyframes are lost to frame/byte cap, idle timeout, or consistency checks | **REFUTED** | Legacy trace counters: 0 cap evictions, 0 timeouts, 0 inconsistent, 0 superseded. The 522 partial keyframe drops were reorder expirations, not any named cap/timeout/consistency path. Peak reassembly was 60,638 B, far below 4 MiB. |
| H4: keyframe state/backoff is not cleared or a stale partial immediately re-arms it | **REFUTED** | Trace repeatedly records `need_keyframe_cleared_after_keyframe_delivery`; a targeted net regression test verifies an older partial cannot re-arm a request after keyframe recovery. Existing backoff tests and the new per-unanswered-episode retry metric confirm the 200 ms minimum applies to unanswered retries; a new post-recovery loss may start a fresh immediate request. |
| H5: simulated sender response is malformed, discontinuous, suppressed, or delayed | **CONFIRMED (delay only)** | Sent IDR uses KEY=1 and CONFIG=1, continuous frame ID 76, and all 43 fragments complete and deliver. The 457 ms first-fragment delay and 716 ms completion are queue delay, not suppression or missing flags. |
| H6: delivery, stale, recovery, or request metrics are miscomputed | **CONFIRMED (stale total)** | `frozen_us` was accumulated across delivery gaps, then incorrectly capped to only the final tail duration. The regression input with 200,000 us of prior freezes and a 466,667 us tail produced 500,000 us under the old cap instead of the correct 666,667 us. Delivered-frame count, requests/minute, and recovery timestamps remain independently counted. |
| H7: original poor measurements are the correct result of the designed policy | **REFUTED as an explanation of the original table** | Removing the artificial bottleneck changes 480p/0.5% to 89.172% delivered and 10.974% stale, and 720p/0.5% to 61.939% / 38.182%. Corrected data still shows a real policy limitation at 720p/1080p above 0.1%; that is not the cause of the original 8.278% / 1.761% values. |

### Root cause

The dominant original failure was the testkit's silent 102%-of-video-bitrate bottleneck with only a 4 MiB queue. Protocol headers and duplicated datagrams already consumed some of that margin, and each 8x keyframe burst serialized for hundreds of milliseconds, causing queue buildup and requests to be retried before a response arrived. The old stale-picture accumulator also truncated earlier freezes to the last tail interval, so those stale percentages were inaccurate. Neither fault required changing `racc-net` policy constants or wire types.

The real-UDP smoke test also showed that a 0% injected-loss profile cannot guarantee lossless delivery from the Windows UDP stack. The old proxy counter counted attempted sends as forwarded; it now reports successful writes and send errors separately. The revised test requires exact delivery when no loss is observed and checks keyframe recovery when real UDP loss occurs. Its final full-workspace and both check-all runs passed. The exact stage of the historical localhost packet drop remains inconclusive.

## Phase 3 — fixes and corrected model

- `racc-testkit` now assigns an unset link to `max(10 x tier bitrate, 50 Mbps)` and at least 64 MiB queue. Explicit rate/queue profiles are left intact for the two constrained cases. A 600-second 480p/0.5% regression verifies zero queue drops and peak scheduled queue below 1 MB; a direct test checks the capacity margin.
- Virtual-link arrivals occur after packet serialization completes plus path delay; duplicates serialize as separate copies and consume bitrate. The simulator reports mean and maximum serializer queue wait. A 10-byte/8 kbps unit case verifies 11,000 us and 21,001 us duplicate arrival times and the second copy's 10,000 us queue wait.
- Request response timing uses the profile one-way path delay, 10 ms encode time, and next frame opportunity. A separate trace option retains the historical 20 ms request-path override for legacy diagnosis. Keyframe-size options support the 4x sensitivity row.
- The stale-picture accumulator now adds the final tail to earlier stale intervals without replacing/capping them. Its regression test fails with the former cap. The full low-loss envelope test covers 480p and 720p at 0.1%, 0.25%, and 0.5%; all six delivered and stale fractions remain within a factor of two of the revised analytic prediction.
- Reassembly counters distinguish keyframe completion/delivery, incomplete/reorder/timeout/eviction/supersession/inconsistency. Targeted tests cover keyframe delivery accounting and stale partial recovery. No production behavior constants changed.
- Simulator-only NACK/FEC models live under `racc-testkit::sim::recovery_models`; `racc-net` and `racc-proto` contain no NACK/FEC implementation or wire changes.

Regression tests were verified against the old behavior by temporary local reversions: the default-link test failed on `Some(1,530,000)` versus `Some(50,000,000)` under the old 102% default; the stale test failed on 500,000 versus 666,667 us under the old cap. Both passed after restoration of the fixes.

## Phase 4 — corrected measurements

The complete corrected matrix, including all 600-second seeds, keyframe-size sensitivities, burst profile, queue delays, and both constrained links, is in `docs/TRANSPORT.md`, section A. The 8x headline at 0.5% / 1% / 2% is:

| Tier | Delivered percent | Stale percent | Requests/min |
|---|---:|---:|---:|
| 480p30 | 89.172 / 70.578 / 29.067 | 10.974 / 29.539 / 70.979 | 57.80 / 113.00 / 136.00 |
| 720p30 | 61.939 / 22.117 / 2.383 | 38.182 / 77.926 / 97.617 | 122.50 / 129.60 / 81.60 |
| 1080p30 | 21.294 / 2.567 / 0.122 | 79.256 / 97.102 / 67.264 | 131.60 / 80.50 / 62.80 |

Envelope test seeds are `0x2505 ^ tier_bitrate_kbps ^ loss_ppm`; the printed matrix contains the decimal seed for every row. The ordinary matrix seed is `0x2505 ^ tier_bitrate_kbps ^ loss_ppm`; the 4x sensitivity XORs 4. Burst seed is `0xfeed`. Constrained seeds are `0x2505 ^ 3500 ^ loss_ppm ^ link_rate_bps`.

## Phase 5 — recovery mechanism comparison

All six models are in `racc-testkit::sim::recovery_models`; the table below reports 84 independent 600-second rows, each with its reproducible seed, both one-way delays (1 and 20 ms), all required IID/burst cases, delivered percent, stale percent, median/p95 freeze, keyframe requests/minute, measured packet loss, bandwidth overhead, and median extra latency. FEC sends one padded XOR parity datagram per 10 or 20 data fragments; the 18-byte parity header, short final groups, and pacing are charged, and parity payload plus header stays within 1200 bytes. NACK rounds cost a 64-byte control message and full 1200-byte retransmit datagrams; the simulator limits the retransmission ring to 250 ms and at most two rounds. From the final original-fragment arrival, retry cost includes the one-RTT plus 8 ms missing-fragment detection window, then one-way request and reply trips. Overhead is extra transmitted bytes, including headers/control, relative to nominal media payload. Added median latency is beyond the same paced one-way base; freeze metrics capture waiting for recovery.

| Tier | Scenario | One-way ms | Model | Seed | Observed loss | Delivered | Stale | Freeze median / p95 ms | Keyframe req/min | Bandwidth overhead | Added median latency ms |
|---|---|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 720p30 | 0.50% | 1 | baseline | 15312 | 0.4990% | 62.822% | 37.163% | 66.666 / 1566.651 | 118.90 | 46.211% | 0.000 |
| 720p30 | 0.50% | 1 | nack-key | 15312 | 0.4991% | 88.633% | 12.124% | 66.666 / 79.070 | 102.30 | 40.030% | 0.000 |
| 720p30 | 0.50% | 1 | nack-all | 15312 | 0.5083% | 100.000% | 2.467% | 13.538 / 13.538 | 0.00 | 0.575% | 0.000 |
| 720p30 | 0.50% | 1 | fec-10 | 15312 | 0.5067% | 99.733% | 0.267% | 1.056 / 65.610 | 2.40 | 17.476% | 3.076 |
| 720p30 | 0.50% | 1 | fec-20 | 15312 | 0.5050% | 99.489% | 0.511% | 0.528 / 66.138 | 4.60 | 10.101% | 1.538 |
| 720p30 | 0.50% | 1 | nack-all+fec-20 | 15312 | 0.5072% | 100.000% | 0.109% | 15.076 / 15.076 | 0.00 | 8.266% | 1.538 |
| 720p30 | 0.50% | 20 | baseline | 15301 | 0.5078% | 59.411% | 40.566% | 99.999 / 1599.984 | 113.50 | 44.111% | 0.000 |
| 720p30 | 0.50% | 20 | nack-key | 15301 | 0.5107% | 84.100% | 21.342% | 99.999 / 188.403 | 95.40 | 37.348% | 0.000 |
| 720p30 | 0.50% | 20 | nack-all | 15301 | 0.4969% | 100.000% | 15.769% | 89.538 / 89.538 | 0.00 | 0.562% | 0.000 |
| 720p30 | 0.50% | 20 | fec-10 | 15301 | 0.5079% | 99.333% | 0.667% | 1.056 / 98.943 | 4.00 | 18.157% | 3.076 |
| 720p30 | 0.50% | 20 | fec-20 | 15301 | 0.4964% | 99.050% | 0.950% | 0.528 / 99.471 | 5.30 | 10.386% | 1.538 |
| 720p30 | 0.50% | 20 | nack-all+fec-20 | 15301 | 0.4968% | 100.000% | 0.834% | 91.076 / 92.615 | 0.00 | 8.277% | 1.538 |
| 720p30 | 1.00% | 1 | baseline | 3912 | 0.9808% | 22.578% | 76.641% | 299.997 / 3633.297 | 131.60 | 51.151% | 0.000 |
| 720p30 | 1.00% | 1 | nack-key | 3912 | 0.9858% | 80.256% | 21.709% | 78.868 / 79.272 | 177.70 | 69.926% | 0.000 |
| 720p30 | 1.00% | 1 | nack-all | 3912 | 0.9911% | 99.978% | 4.461% | 13.538 / 15.076 | 0.20 | 1.202% | 0.000 |
| 720p30 | 1.00% | 1 | fec-10 | 3912 | 0.9882% | 98.756% | 1.234% | 33.333 / 65.610 | 9.80 | 20.625% | 3.076 |
| 720p30 | 1.00% | 1 | fec-20 | 3912 | 0.9913% | 97.917% | 2.072% | 0.528 / 299.469 | 14.70 | 14.214% | 1.538 |
| 720p30 | 1.00% | 1 | nack-all+fec-20 | 3912 | 0.9923% | 100.000% | 0.344% | 15.076 / 15.076 | 0.00 | 8.350% | 1.538 |
| 720p30 | 1.00% | 20 | baseline | 3933 | 1.0092% | 18.761% | 81.232% | 333.330 / 5733.276 | 117.00 | 45.472% | 0.000 |
| 720p30 | 1.00% | 20 | nack-key | 3933 | 1.0217% | 72.800% | 41.107% | 188.201 / 188.605 | 163.20 | 64.279% | 0.000 |
| 720p30 | 1.00% | 20 | nack-all | 3933 | 1.0236% | 100.000% | 29.879% | 89.538 / 91.076 | 0.00 | 1.162% | 0.000 |
| 720p30 | 1.00% | 20 | fec-10 | 3933 | 1.0117% | 97.989% | 2.011% | 98.943 / 98.943 | 11.20 | 21.221% | 3.076 |
| 720p30 | 1.00% | 20 | fec-20 | 3933 | 1.0150% | 96.989% | 3.011% | 0.528 / 99.471 | 16.00 | 14.744% | 1.538 |
| 720p30 | 1.00% | 20 | nack-all+fec-20 | 3933 | 1.0191% | 100.000% | 2.379% | 91.076 / 91.076 | 0.00 | 8.367% | 1.538 |
| 720p30 | 2.00% | 1 | baseline | 26232 | 2.0151% | 2.467% | 95.550% | 2599.974 / 20166.465 | 77.60 | 30.185% | 0.000 |
| 720p30 | 2.00% | 1 | nack-key | 26232 | 1.9979% | 68.783% | 35.222% | 78.868 / 79.676 | 280.20 | 111.658% | 0.000 |
| 720p30 | 2.00% | 1 | nack-all | 26232 | 2.0147% | 99.956% | 7.546% | 13.538 / 15.076 | 0.40 | 2.460% | 0.000 |
| 720p30 | 2.00% | 1 | fec-10 | 26232 | 2.0178% | 91.422% | 8.578% | 65.610 / 298.941 | 45.40 | 35.774% | 3.076 |
| 720p30 | 2.00% | 1 | fec-20 | 26232 | 2.0144% | 84.844% | 15.156% | 66.138 / 299.469 | 65.20 | 34.781% | 1.538 |
| 720p30 | 2.00% | 1 | nack-all+fec-20 | 26232 | 2.0151% | 100.000% | 1.389% | 15.076 / 16.615 | 0.00 | 8.742% | 1.538 |
| 720p30 | 2.00% | 20 | baseline | 26221 | 1.9904% | 2.811% | 92.438% | 1599.984 / 17099.829 | 79.60 | 30.924% | 0.000 |
| 720p30 | 2.00% | 20 | nack-key | 26221 | 1.9782% | 59.483% | 65.002% | 188.201 / 189.009 | 243.00 | 96.753% | 0.000 |
| 720p30 | 2.00% | 20 | nack-all | 26221 | 2.0008% | 99.983% | 48.622% | 89.538 / 91.076 | 0.10 | 2.326% | 0.000 |
| 720p30 | 2.00% | 20 | fec-10 | 26221 | 1.9877% | 90.689% | 9.295% | 33.333 / 332.274 | 42.40 | 34.498% | 3.076 |
| 720p30 | 2.00% | 20 | fec-20 | 26221 | 1.9868% | 82.950% | 16.995% | 99.471 / 332.802 | 66.30 | 35.229% | 1.538 |
| 720p30 | 2.00% | 20 | nack-all+fec-20 | 26221 | 1.9955% | 100.000% | 8.671% | 91.076 / 92.615 | 0.00 | 8.759% | 1.538 |
| 720p30 | 5.00% | 1 | baseline | 60168 | 4.9801% | 0.083% | 94.830% | 72865.938 / 160698.393 | 60.30 | 23.417% | 0.000 |
| 720p30 | 5.00% | 1 | nack-key | 60168 | 5.0059% | 49.628% | 56.094% | 79.272 / 92.282 | 435.70 | 179.955% | 0.000 |
| 720p30 | 5.00% | 1 | nack-all | 60168 | 4.9796% | 99.611% | 12.258% | 13.538 / 27.076 | 3.00 | 7.046% | 0.000 |
| 720p30 | 5.00% | 1 | fec-10 | 60168 | 5.0077% | 21.533% | 77.857% | 65.610 / 3632.241 | 116.40 | 65.945% | 3.076 |
| 720p30 | 5.00% | 1 | fec-20 | 60168 | 4.9869% | 6.267% | 93.469% | 66.138 / 9832.707 | 88.70 | 44.311% | 1.538 |
| 720p30 | 5.00% | 1 | nack-all+fec-20 | 60168 | 4.9932% | 99.867% | 6.831% | 15.076 / 28.614 | 1.20 | 11.618% | 1.538 |
| 720p30 | 5.00% | 20 | baseline | 60189 | 4.9194% | 0.000% | 100.000% | 599994.000 / 599994.000 | 58.40 | 22.678% | 0.000 |
| 720p30 | 5.00% | 20 | nack-key | 60189 | 4.9629% | 39.850% | 92.085% | 188.605 / 277.413 | 354.50 | 146.350% | 0.000 |
| 720p30 | 5.00% | 20 | nack-all | 60189 | 4.9240% | 99.517% | 75.185% | 89.538 / 179.076 | 2.90 | 6.939% | 0.000 |
| 720p30 | 5.00% | 20 | fec-10 | 60189 | 4.9748% | 20.622% | 79.371% | 98.943 / 3665.574 | 116.20 | 65.860% | 3.076 |
| 720p30 | 5.00% | 20 | fec-20 | 60189 | 4.9554% | 5.828% | 94.165% | 99.471 / 10899.363 | 85.30 | 42.926% | 1.538 |
| 720p30 | 5.00% | 20 | nack-all+fec-20 | 60189 | 4.9220% | 99.783% | 38.843% | 91.076 / 180.614 | 1.30 | 11.508% | 1.538 |
| 1080p30 | 1.00% | 1 | baseline | 6588 | 0.9903% | 3.472% | 95.574% | 2599.974 / 17066.496 | 82.50 | 32.042% | 0.000 |
| 1080p30 | 1.00% | 1 | nack-key | 6588 | 0.9888% | 69.311% | 34.472% | 78.767 / 79.171 | 276.20 | 108.639% | 0.000 |
| 1080p30 | 1.00% | 1 | nack-all | 6588 | 0.9886% | 100.000% | 6.691% | 12.800 / 13.600 | 0.00 | 1.076% | 0.000 |
| 1080p30 | 1.00% | 1 | fec-10 | 6588 | 0.9873% | 96.822% | 3.178% | 33.333 / 66.286 | 21.80 | 21.666% | 2.400 |
| 1080p30 | 1.00% | 1 | fec-20 | 6588 | 0.9883% | 93.378% | 6.622% | 66.076 / 299.407 | 40.80 | 24.839% | 1.600 |
| 1080p30 | 1.00% | 1 | nack-all+fec-20 | 6588 | 0.9860% | 100.000% | 0.839% | 13.600 / 14.400 | 0.00 | 8.399% | 1.600 |
| 1080p30 | 1.00% | 20 | baseline | 6569 | 0.9964% | 2.667% | 97.327% | 2633.307 / 15033.183 | 80.20 | 31.148% | 0.000 |
| 1080p30 | 1.00% | 20 | nack-key | 6569 | 1.0007% | 60.067% | 64.650% | 188.100 / 188.504 | 239.60 | 94.310% | 0.000 |
| 1080p30 | 1.00% | 20 | nack-all | 6569 | 0.9725% | 100.000% | 46.370% | 88.800 / 89.600 | 0.00 | 1.058% | 0.000 |
| 1080p30 | 1.00% | 20 | fec-10 | 6569 | 0.9854% | 95.400% | 4.600% | 33.333 / 332.950 | 22.30 | 21.880% | 2.400 |
| 1080p30 | 1.00% | 20 | fec-20 | 6569 | 0.9921% | 91.133% | 8.867% | 99.409 / 332.740 | 39.90 | 24.472% | 1.600 |
| 1080p30 | 1.00% | 20 | nack-all+fec-20 | 6569 | 0.9752% | 100.000% | 5.149% | 89.600 / 90.400 | 0.00 | 8.390% | 1.600 |
| 1080p30 | 2.00% | 1 | baseline | 28812 | 1.9972% | 0.189% | 79.224% | 33599.664 / 198931.344 | 60.50 | 23.487% | 0.000 |
| 1080p30 | 2.00% | 1 | nack-key | 28812 | 1.9893% | 55.706% | 49.525% | 78.868 / 90.868 | 396.90 | 158.029% | 0.000 |
| 1080p30 | 2.00% | 1 | nack-all | 28812 | 2.0127% | 99.978% | 9.656% | 12.800 / 14.400 | 0.20 | 2.280% | 0.000 |
| 1080p30 | 2.00% | 1 | fec-10 | 28812 | 1.9997% | 77.306% | 22.683% | 33.333 / 732.946 | 85.00 | 48.697% | 2.400 |
| 1080p30 | 2.00% | 1 | fec-20 | 28812 | 1.9995% | 48.233% | 51.752% | 66.076 / 1566.061 | 121.20 | 57.532% | 1.600 |
| 1080p30 | 2.00% | 1 | nack-all+fec-20 | 28812 | 2.0054% | 99.978% | 2.881% | 13.600 / 25.600 | 0.20 | 8.941% | 1.600 |
| 1080p30 | 2.00% | 20 | baseline | 28825 | 1.9955% | 0.128% | 88.921% | 40866.258 / 146265.204 | 60.70 | 23.565% | 0.000 |
| 1080p30 | 2.00% | 20 | nack-key | 28825 | 1.9945% | 45.850% | 85.078% | 188.201 / 276.201 | 324.50 | 129.233% | 0.000 |
| 1080p30 | 2.00% | 20 | nack-all | 28825 | 1.9882% | 99.878% | 65.946% | 88.800 / 90.400 | 0.60 | 2.412% | 0.000 |
| 1080p30 | 2.00% | 20 | fec-10 | 28825 | 1.9961% | 75.944% | 24.056% | 33.333 / 333.330 | 83.00 | 47.842% | 2.400 |
| 1080p30 | 2.00% | 20 | fec-20 | 28825 | 1.9956% | 49.517% | 50.428% | 99.409 / 1599.394 | 113.00 | 54.234% | 1.600 |
| 1080p30 | 2.00% | 20 | nack-all+fec-20 | 28825 | 1.9919% | 100.000% | 17.875% | 89.600 / 91.200 | 0.00 | 8.816% | 1.600 |
| 720p30 | burst_GE(0.224% actual) | 1 | baseline | 51001 | 0.2241% | 96.644% | 3.356% | 66.666 / 299.997 | 20.50 | 7.974% | 0.000 |
| 720p30 | burst_GE(0.226% actual) | 1 | nack-key | 51001 | 0.2262% | 98.044% | 2.027% | 66.666 / 79.474 | 17.60 | 6.897% | 0.000 |
| 720p30 | burst_GE(0.219% actual) | 1 | nack-all | 51001 | 0.2195% | 99.933% | 0.570% | 13.538 / 31.691 | 0.60 | 0.473% | 0.000 |
| 720p30 | burst_GE(0.226% actual) | 1 | fec-10 | 51001 | 0.2256% | 98.300% | 1.700% | 33.333 / 298.941 | 12.10 | 21.604% | 3.076 |
| 720p30 | burst_GE(0.223% actual) | 1 | fec-20 | 51001 | 0.2228% | 98.089% | 1.911% | 0.528 / 299.469 | 13.20 | 13.603% | 1.538 |
| 720p30 | burst_GE(0.223% actual) | 1 | nack-all+fec-20 | 51001 | 0.2229% | 99.933% | 0.431% | 15.076 / 36.306 | 0.60 | 8.668% | 1.538 |
| 720p30 | burst_GE(0.275% actual) | 20 | baseline | 50988 | 0.2753% | 95.950% | 4.050% | 99.999 / 333.330 | 20.50 | 7.974% | 0.000 |
| 720p30 | burst_GE(0.275% actual) | 20 | nack-key | 50988 | 0.2750% | 96.833% | 3.448% | 99.999 / 188.605 | 19.00 | 7.433% | 0.000 |
| 720p30 | burst_GE(0.273% actual) | 20 | nack-all | 50988 | 0.2728% | 99.733% | 3.339% | 91.076 / 182.153 | 1.60 | 0.916% | 0.000 |
| 720p30 | burst_GE(0.277% actual) | 20 | fec-10 | 50988 | 0.2774% | 97.556% | 2.445% | 33.333 / 98.943 | 13.00 | 21.987% | 3.076 |
| 720p30 | burst_GE(0.279% actual) | 20 | fec-20 | 50988 | 0.2795% | 97.372% | 2.628% | 0.528 / 332.802 | 13.70 | 13.807% | 1.538 |
| 720p30 | burst_GE(0.276% actual) | 20 | nack-all+fec-20 | 50988 | 0.2765% | 99.833% | 2.464% | 91.076 / 182.153 | 1.00 | 8.886% | 1.538 |

### Recommendation

Implement the simulator's hybrid NACK-all+FEC-20 in M7, after M3 topology/session, M4 UI, M5 encode, and M6 host-agent work; do not add it to the current M2 transport implementation. At 720p/2%/1 ms one-way, the model raises delivery from 2.467% to 100%, lowers stale time from 95.550% to 1.389%, and uses 8.742% extra bandwidth with 1.538 ms median added latency. At 720p/0.5%/20 ms it raises delivery from 59.411% to 100% and lowers stale time from 40.566% to 0.834%, using 8.277% overhead and 1.538 ms median added latency. NACK-all alone is a lower-cost fallback (at 720p/2%/1 ms, 99.956% delivered / 7.546% stale / 2.460% overhead), but at 20 ms its 48.622% stale result misses the selected 10% criterion.

The hybrid implementation cost is high: a bounded retransmission ring, missing-fragment timers, two-round retry state, late/duplicate reply handling, parity generation/recovery, and parity pacing. A future wire proposal needs a bounded `NackFragments` control message containing epoch, frame ID, missing fragment indexes/ranges, and round number. Parity will also need a defined datagram kind and bounded group identification; no header or control change is implemented here. The wire proposal is descriptive only. At 1080p/2%/20 ms, the hybrid reaches 100% delivery but still has 17.875% stale time, so the model does not support an all-tier 2% WAN quality claim. Revisit the mechanism after real M7 path results and keep the 1080p WAN case visible in acceptance.

The additional parity bytes in the hybrid are about 8-9% on the 720p scenarios; this buys much lower staleness than NACK-all under 20 ms one-way delay. If M7 measures only low-latency direct paths and confirms NACK-all stays below the product stale-time target, the simpler NACK-all design remains preferable; the current audit data favors the hybrid across the required delay cases.
For the current implementation, define v0 acceptable as >=90% delivered and <=10% stale. Corrected measurements meet that threshold across 480p/720p/1080p only through 0.1% IID loss; 480p also meets it at 0.25%. All tiers fail at 0.5% and above. This threshold is a stated interpretation of the measured behavior, not a measured Tailscale limit.

## Verification labels and limits

- `[VERIFIED-RUN]` means reproducible deterministic simulator or local tests on this Windows worktree; seeds and commands are in the report.
- `[COMPILE-ONLY]` covers target compile checks only; no Mac/Windows streaming claim follows from it.
- `[HUMAN-PENDING]` covers real Tailscale paths, Windows GPU/capture behavior, and the owner's Mac identity/performance. No hardware transport result is inferred from these simulations.

## Final acceptance checklist

The 14 M2.5 acceptance checks and evidence are appended to `docs/PROGRESS.md`. Local checks 1–13 pass; check 14 remains BLOCKED-HUMAN solely because the no-network constraint prevents a normal push.
