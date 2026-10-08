# ADR 0020: Display identity inputs and fallback

- Status: Provisional implementation
- Date: 2026-10-07

## Context

Monitor identifiers must survive display enumeration order changes. The topology crate derives stable ids from EDID manufacturer/product/serial and the connector path, with deterministic collision handling.

## Decision

The Windows capture backend best-effort reads the EDID base block from the matching monitor interface's read-only Device Parameters registry key. It validates the EDID header and checksum, bounds the read to 32 KiB, and combines valid manufacturer, product code, serial, and connector path with the topology identity helper. When EDID is absent or invalid, the connector path alone is used and the result is marked ConnectorPathOnly; when DisplayConfig cannot provide the path, the DXGI GDI device name is the last-resort fallback.

## Consequences and follow-up

- The metadata reports whether EDID contributed to the id without exposing EDID serial data or device paths.
- The 2026-10-07 PC #1 metadata-only probe reported connector-path identity for all three outputs, so the registry read did not provide a valid EDID base block during that run. Connector-only ids may change if Windows renames or reassigns the path.
- Stable identity across reboots and hot-plug events remains HUMAN-PENDING.
