# Viewer-less stream probe

The opt-in racc-probe binary performs the viewer control handshake and receives ordered H.264 access units without creating a UI or decoder. It uses ViewerSession, the normal TCP control transport, and the normal UDP receiver. Captured payload is the reassembled Annex B byte stream.

Build and run it from the workspace root:

    cargo run --offline -p racc-testkit --no-default-features --features probe --bin racc-probe -- 100.x.y.z:47473 100.x.y.w

The first address is the host Tailscale IP and control port. The second is a local Tailscale IP on the viewer machine. Both are validated with the production Tailscale-only bind policy. An explicitly test-only loopback build is available as `probe-loopback`; it accepts only `127.0.0.1`/`::1` or Tailscale addresses and must not be used for shipped builds. An optional third argument selects the output path. By default, captures go under the ignored target/probe-captures directory.

The capture stops growing at 512 MiB. It writes only complete frames after the current epoch's keyframe, and flushes at one-second stats intervals. The output is a raw H.264 Annex B stream without container timestamps. Inspect it with ffprobe or play it using an H.264 elementary-stream player.

Stdin commands are help, list displays, stats, switch <display-id>, pause, resume, quality 480|720|1080|auto, keyframe, and quit. Stats print once per second and include frames, bytes, FPS, packet/frame loss, keyframe requests, RTT, and bounded host-reported values. The probe reports transport and file-writing behavior; it does not prove that the access units decode or that a remote display is visible. The end-to-end run still needs two authorized Tailscale peers and the human checks in docs/HARDWARE.md.

A local transport harness is available only through the explicit `host-loopback` testkit feature:

    cargo run --offline -p racc-testkit --no-default-features --features host-loopback --example host-loopback

It binds only to loopback, uses `HostRuntime::new_loopback_test`, and grants a synthetic test identity after checking the peer address is loopback. In another terminal, run `racc-probe` with `--features probe-loopback` against the printed address. The host emits tiny synthetic Annex B payloads to exercise handshake, session control, UDP reassembly, pause/resume, switching and disconnect timeout. Those payloads are not encoded video and do not prove decode, capture or hardware encoding. The loopback feature is not enabled by `racc-app` or `racc-host-agent`; `scripts/check-features` checks this.