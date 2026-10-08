# ADR 0041: macOS text clipboard adapter and capability boundary

Date: 2026-10-07

Status: Accepted for source integration; Apple host compilation and Mac runtime verification remain pending.

## Context

M8/M9 require bounded, text-only clipboard sync in both host and viewer roles. The repository already has a portable session policy and change-count poller. Native calls must remain target-gated, confined to a thin platform module, and dormant until the user enables sync for the authenticated active session. The host must advertise clipboard capability only after its platform bridge is wired.

## Decision

Use `NSPasteboard.generalPasteboard()` and `NSPasteboardTypeString` through the maintained `objc2` bindings. `MacPasteboard` implements the portable `PasteboardText` interface and reads/writes only UTF-8 string content. It obtains the NSString UTF-8 byte count before converting to a Rust `String` and rejects values above 512 KiB. AppKit retained objects use objc2 ownership wrappers. The only explicit unsafe access is the immutable AppKit string-type constant, isolated in `crates/clipboard/src/macos.rs` with a SAFETY comment.

The viewer creates a dormant worker in the app's user session. It starts a poller only after the active connected session is explicitly enabled, checks an enable generation before reading/sending/applying, coalesces to one local value and one remote apply, and clears queued values on disable or connection end. The foreground macOS host creates a bridge only for the active authenticated viewer connection. `ClipboardSyncControl` enables the policy and starts the change-count poller; disable, close, reconnect and shutdown end the policy and discard pending data. The host accepts/applies remote text only for that connection and uses the shared echo/sequence/size policy for host-originated changes. Neither path logs clipboard content.

The foreground Mac host sets `FEATURE_TEXT_CLIPBOARD` because the host-side bridge and handlers are source-wired. This feature indicates implementation capability, not current opt-in: clipboard reads/writes remain off until `ClipboardSyncControl { enabled: true }` arrives. The viewer also requires its local worker and the remote host feature before offering the toggle.

## Binding metadata

Package metadata was checked in the local Cargo registry; release dates were checked on docs.rs version listings.

| Crate | Resolved version | SPDX license expression | Release date | Direct features used |
|---|---:|---|---|---|
| `objc2-app-kit` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | 2025-10-04 | `std`, `NSPasteboard`; default features disabled |
| `objc2-foundation` | 0.3.2 | `MIT` | 2025-10-04 | `std`, `NSString`; default features disabled |
| `objc2` (transitive) | 0.6.5 | `MIT` | 2026-10-05 | `std` |
| `objc2-encode` (transitive) | 4.1.0 | `MIT` | 2025-01-22 | `alloc`, `std` through objc2 |
| `bitflags` (transitive) | 2.13.2 | `MIT OR Apache-2.0` | — | `std` |

References: [objc2-app-kit 0.3.2](https://docs.rs/crate/objc2-app-kit/0.3.2), [objc2-foundation 0.3.2](https://docs.rs/crate/objc2-foundation/0.3.2), [objc2 0.6.5](https://docs.rs/crate/objc2/0.6.5), [objc2-encode 4.1.0](https://docs.rs/crate/objc2-encode/4.1.0).

## Target dependency audit

Observed with `cargo tree --offline -p racc-clipboard --target ... -e normal`:

- macOS: `racc-clipboard -> objc2-app-kit 0.3.2 -> objc2 0.6.5 -> objc2-encode 4.1.0`; AppKit also depends on `objc2-foundation 0.3.2`. `racc-clipboard` directly enables `objc2-foundation 0.3.2` for NSString conversion. `bitflags 2.13.2` is also in this binding tree.
- Windows: `racc-clipboard -> windows 0.58.0 -> windows-core/windows-targets`. The Windows tree contains no `objc2`, AppKit, or Foundation crates.

Cargo manifests pin the AppKit and Foundation versions and declare them only under `cfg(target_os = "macos")`. No Apple dependency is added to `proto`, transport, or portable policy code.

## Consequences and limits

- The 250 ms change-count poll avoids continuous reads and snapshots the existing change count on enable, so enabling sync does not immediately copy pre-existing clipboard contents.
- Remote writes update the remembered change count and suppress the resulting local echo. A 512 KiB size limit applies in both directions; rich text, images, and files remain out of scope.
- The AppKit adapter and app worker pass the installed `x86_64-apple-darwin` compile check on the Windows coding host. The full `racc-host-agent` Apple check is blocked before host Rust compilation because OpenH264's build script cannot find a target C++ compiler. No Mac API or clipboard behavior has been exercised.
- Human checks remain required on Monterey: both Mac viewer/host directions against each Windows PC, Unicode and size boundaries, disabled/default and disconnect gating, echo prevention, and content-free logs. Privacy prompts on newer macOS versions are also unverified.
