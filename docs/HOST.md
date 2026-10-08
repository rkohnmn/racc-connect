# Windows host agent

## Current status

The Windows service supervisor, console-session helper launcher, and foreground host runtime are present in source. The current evidence is **COMPILE-ONLY** for the Windows target and **TESTED-FAKE** for host-agent tests. The service, helper, real desktop capture, real network path, and real clipboard have not been run by the agent. No service was installed or registered.

## Process model and service lifecycle

- The service command enters the Windows SCM dispatcher, registers stop/shutdown/session-change handling, and supervises a helper for the active console session.
- The service uses WTS session notifications and polls the active console session every two seconds to reconcile missed changes. A bounded 64-entry notification queue fails closed on overflow.
- It obtains the active user's token with WTSQueryUserToken, creates an environment block, and starts the helper with CreateProcessAsUserW on WinSta0\Default.
- Each helper gets a random, ACL-restricted global stop event. Service stop, lock, logoff, and session switch signal that event; the service waits five seconds, then terminates a helper that did not exit.
- The helper runs the same foreground host lifecycle as console mode. The SCM supervisor policy and its failure/backoff behavior have fake tests; no SCM lifecycle has been observed on hardware.
- The install and uninstall binary commands only print instructions. The human-run scripts scripts/install-service.ps1 and scripts/uninstall-service.ps1 have not been executed and do not run automatically from the binary.

## Host data path and authorization

The Windows foreground host binds only to a validated Tailscale address. Before allocating a handler thread or performing whois, it applies a fixed-window limit of 20 accepted TCP connection attempts per peer IP per 60 seconds. The limiter retains at most 4,096 peer entries, expires old windows, and rejects new peers while full. It counts attempts before Hello, so stalled or malformed handshakes are included. For permitted attempts, the host performs Tailscale whois and checks the persisted node-ID allowlist before accepting Hello. Whois failures and unapproved peers are rejected. The host supports one active viewer.

For an accepted viewer, the host binds a Tailscale-policy UDP sender to the viewer's announced port, captures through the DXGI Desktop Duplication backend, configures Media Foundation H.264 hardware encoding with OpenH264 software fallback, and sends bounded H.264 frames through the paced VideoSender worker. The active foreground runtime applies HostRuntime actions, including the five-second control-disconnect grace period; after that timeout the session actions stop capture and video. These Windows behaviors are source-wired and compile-checked, not runtime-verified.

This is not the complete M6 acceptance pipeline: the racc-probe and host-loopback tools do not exist, the viewer UI is not yet a verified end-to-end client, and no two-PC stream has been run. Input injection is M7 and is not connected.

## Secure desktop and capture recovery

The service launches the helper only on WinSta0\Default. It does not attach to the Winlogon desktop and cannot claim to capture UAC or lock-screen content. When capture reports access loss, the host emits an explicit secure-desktop-unavailable diagnostic and retries after desktop access returns. The required Windows Graphics Capture fallback is not implemented. These paths remain HUMAN-PENDING and must not be described as secure-desktop capture support.

## Telemetry and clipboard host adapter

The foreground host queues a StatsReport to the authenticated viewer no more often than once per second. Sources are:

| Field | Source |
|---|---|
| Host CPU | Machine-wide GetSystemTimes, in tenths of a percent. The first/invalid sample is zero because the protocol has no unknown sentinel. |
| Capture backend | DXGI while capture is running; otherwise Unknown. |
| Encoder | Active Media Foundation hardware encoder, OpenH264 software fallback, or Unknown. |
| Resolution | Active encoder configuration; zero before an encoder is active. |
| Display refresh | Enumerated refresh for the selected display; zero when unavailable. |
| Target bitrate | Active encoder configuration in kilobits per second; zero before configuration. |
| Actual bitrate | Successful Racc UDP datagram bytes over the report interval; includes project headers but excludes UDP/IP overhead. |

This is source-wired and COMPILE-ONLY; no sample values or runtime measurement are claimed. RTT, loss, viewer decoder/render data, and UI presentation still require their respective viewer/runtime paths and hardware checks.

For an authenticated Windows viewer, clipboard sync starts disabled and becomes active only after `ClipboardSyncControl { enabled: true }`. Disabling or closing the session stops future host reads and drops pending updates. Local text changes pass through the bounded shared size, sequence, conflict, and echo policy and are queued through ControlSendHandle. The Windows viewer app uses a separate same-user clipboard worker and exposes the session toggle only when the host advertises text clipboard support. Clipboard text is never placed in log messages or telemetry. The listener/writer and host bridge have fake round-trip coverage; real Windows clipboard traffic remains HUMAN-PENDING. macOS hosting does not advertise clipboard support until its host-side bridge is implemented.

## Local IPC, configuration, and logging gaps

`racc-core::ipc` defines bounded 4-byte little-endian length-prefixed JSON messages (64 KiB maximum), the safe synchronous `IpcClient`, and the request/reply types for status, hosting, allowlist changes, and pending subscriptions. The Windows transport is available from `host-agent::windows::local_ipc`: it uses an overlapped byte-mode named pipe at `\\.\pipe\racc-connect-host`, polls/cancels outstanding operations during shutdown, and enables `PIPE_REJECT_REMOTE_CLIENTS`. Its protected DACL grants SYSTEM and Administrators full access and Interactive Users generic read/write (`D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)`). The transport dispatches a request and can keep a `SubscribePending` connection open for events. These source paths are COMPILE-ONLY; the pipe ACL has not been inspected on a running Windows host.

The Windows foreground helper now starts `run_local_ipc_server` on a helper-owned worker and stops it during helper shutdown. Its production handler reads `HostRuntime` status, lists approved peers, persists approve/reject/remove operations through `TailscaleAllowlistAuthorizer`, and sends an initial pending snapshot followed by add/remove events. Pending events are derived from bounded allowlist snapshots, with a bounded wake channel from the authenticated control callback; the pending set itself is capped by the identity store. Approval removes the local pending queue entry; that peer must reconnect before opening a session. Removing an approved identity affects future authorization; an already-connected session is not forcibly revoked by this operation. Handler behavior is TESTED-FAKE through in-memory request tests, and Windows platform code is COMPILE-ONLY. The helper pipe was not launched or inspected on Windows during this work.

`SetHostingEnabled` deliberately returns `Failure(Unavailable)` and leaves the reported state unchanged. The current foreground architecture starts the TCP listener, capture/input workers, and event loop inline as one helper run; there is no safe stop/rebind lifecycle controller or persisted hosting setting for IPC to call. The helper reports hosting enabled while that run is active. The UI has no named-pipe client yet, so these operations are not available through the app. The remaining bridge is an app-side transport/client and a host lifecycle/settings controller that can stop and restart hosting without terminating the helper; until then, do not treat a hosting toggle request as successful.

The foreground helper derives its allowlist root from the interactive user's APPDATA, falling back to LOCALAPPDATA; it is not a machine-wide ProgramData location and does not yet have the service-owned restrictive ACL required for deployment. Service diagnostics currently use OutputDebugStringW; rotating files and Windows Event Log registration are absent. The 20-attempts-per-peer-per-minute fixed-window limit is enforced before whois and handler creation; its threshold and table capacity are documented above and covered by deterministic tests.

## Owner checks

Do not run the install/uninstall scripts from agent work. The human must review the release binary and run the checklist in docs/HARDWARE.md on both Windows PCs. It covers service installation/removal, helper supervision, a real Tailscale stream once a probe/viewer exists, unknown-peer behavior, capture and resource measurements, and explicit secure-desktop status.