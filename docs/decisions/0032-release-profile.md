# ADR 0032: Measured Windows release profile

- Status: Accepted for the Windows release build; Mac runtime and size remain unmeasured
- Date: 2026-10-07

## Context

M10 asks for release profile tuning measured against the Cargo default. The first real Windows portable build on Windows PC #1 produced a direct baseline. An alternate build used thin LTO, one codegen unit, symbol stripping, and the default unwind panic behavior. Both builds used the same working tree and pinned Rust/Cargo toolchain. The candidate was built after a cold release-profile dependency compile, so its elapsed time includes recompilation under the distinct profile.

## Measurements

| Windows x64 profile | App executable | Host-agent executable | Combined exe size | Clean-profile build time |
|---|---:|---:|---:|---:|
| Cargo default | 13,117,952 B | 1,885,696 B | 15,003,648 B | 2m 20s |
| thin LTO, 1 codegen unit, stripped symbols | 12,058,624 B | 1,701,376 B | 13,760,000 B | 3m 59s |

The candidate reduced the app by 8.08%, the host-agent by 9.77%, and combined executable bytes by 8.29%. The initial candidate build took 1m 39s longer (about 70.7%) because its release-profile dependencies were rebuilt. The measured default-profile portable ZIP was 5,700,501 bytes. The optimized final ZIP was 5,464,008 bytes with SHA-256 `5b2131213003545b13de172b86078558d1678ef4b2a9d5f8d12a1062d71192cf`. These are local build observations, not deterministic size guarantees across toolchains.

## Decision

Use `lto = "thin"`, `codegen-units = 1`, and `strip = "symbols"` for `[profile.release]`. Keep `panic = "unwind"`: the Windows service catches unwinds at its FFI boundary, and this comparison did not measure a reason to change that failure behavior. Do not infer runtime throughput or idle-resource improvements from file size.

## Consequences

The tested candidate build completed successfully, but a fresh release build takes longer and this pass measured no startup, frame pacing, throughput, CPU, or idle private memory. The full release artifact must be rebuilt and packaged with this profile. Mac sizes and runtime remain HUMAN-PENDING.
