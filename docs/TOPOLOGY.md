# Topology domain and coordinate rules

`racc-topology` is a pure-Rust domain crate. It depends at runtime only on `racc-proto`; it does not query displays or inject pointer events. Host and viewer platform adapters must translate their OS measurements into these values.

## Domain and wire boundary

- `DisplayId` wraps a nonzero `u32`; wire value zero is reserved.
- `Display` stores the stable identifier, bounded UTF-8 name, physical-pixel origin, physical-pixel size, thousandths scale factor, thousandths-of-hertz refresh, and typed primary/active/available/read-only-HDR flags.
- `Topology` holds at most 16 displays sorted by ascending id, one revision, and an optional streamed display id.
- Validation rejects duplicate and zero ids, zero dimensions, names longer than 128 UTF-8 bytes, more than one primary display, reserved flag bits, and a streamed id absent from the list. Wire conversion uses `TopologyAnnounce` and reports typed errors.
- `apply_update` uses u32 serial arithmetic. A revision is newer when `candidate.wrapping_sub(current)` is nonzero and less than `2^31`; equality and the exactly-half-range case are ignored as stale or ambiguous.
- Virtual-desktop bounds include every available display and any gaps between them. Origins may be negative. Bounds use exclusive right/bottom edges for extent calculation and return an overflow error when the union cannot fit the supported u32 dimensions.

### Stable display identity

The v1 canonical identity bytes are concatenated as follows: ASCII `racc-display-id-v1` plus NUL; the three EDID manufacturer letters uppercased; product code as little-endian u16; serial-presence tag (zero if absent, otherwise one followed by the little-endian u32 serial); connector-name UTF-8 byte length as little-endian u32; connector bytes; ordinal-presence tag (zero if absent, otherwise one followed by the little-endian u32 ordinal). FNV-1a 32-bit hashes these bytes. A zero hash maps to id 1.

Golden vectors:

| Inputs | Canonical bytes (hex, spaces separate bytes) | DisplayId |
|---|---|---:|
| `DEL`, product `0x1234`, no serial, connector `DISPLAY1`, ordinal 0 | `72 61 63 63 2d 64 69 73 70 6c 61 79 2d 69 64 2d 76 31 00 44 45 4c 34 12 00 08 00 00 00 44 49 53 50 4c 41 59 31 01 00 00 00 00` | `0x69cb2786` |
| `ACM`, product `0x00ab`, serial `0x01020304`, connector `DP-2`, no ordinal | `72 61 63 63 2d 64 69 73 70 6c 61 79 2d 69 64 2d 76 31 00 41 43 4d ab 00 01 04 03 02 01 04 00 00 00 44 50 2d 32 00` | `0xa4cc8445` |

`assign_display_ids` sorts canonical identities before resolving a hash collision by bounded deterministic rehash probing, then returns ids in the input order. The identity depends on host-reported EDID and connector metadata. A missing/zero serial or a connector rename can change an id; the host may supply an ordinal as a disambiguator. This pure crate does not verify that a platform adapter's metadata is stable.

### TopologyAnnounce wire golden

The test encodes one primary, available display named `D1` at `(-1920, 0)`, size `1920x1080`, scale 1000, refresh 60000, streamed id 1, revision 1. The full length-prefixed frame is:

```text
28 00 00 00 03 01 00 00 00 01 00 00 00 01 01 00 00 00 02 44 31
80 f8 ff ff 00 00 00 00 80 07 00 00 38 04 00 00 e8 03 60 ea 00 00 07
```

`28 00 00 00` is the 40-byte body length. `03` is the TopologyAnnounce message type. The same golden bytes are asserted by `topology_proto_conversion_has_negative_origin_golden_frame`.

## Diff semantics

`diff_topologies` returns sorted added and removed displays and exact per-field changes for matching ids. It separately reports a changed streamed-display id and a changed primary id. `requires_stream_reset` is true when the selected stream display was removed, resized, changed refresh or scale, or became unavailable. A name, origin, primary, active, or HDR-only change does not require a video encoder reset; input mapping consumers still use the newest topology.

## Coordinate conventions

All arithmetic for physical and Windows pointer mapping is integer based with round-half-up. No function reads platform APIs. Relative mouse motion stays raw and must bypass these absolute-coordinate helpers.

1. `letterbox_rect` uses the stream aspect ratio, chooses the limiting container dimension, rounds the other dimension half-up, centers with floor offsets, and never returns a zero extent for nonzero input. A zero container returns `None`; zero stream dimensions are an error.
2. Pointer normalization uses the rendered video rectangle in device pixels. `Reject` treats the right and bottom edges as exclusive; `Clamp` maps outside positions to an edge. The normalized result is u16 in `0..=65535`.
3. Host physical pixels use `D.origin + round_half_up(u * (D.extent - 1) / 65535)`. A one-pixel dimension always maps to its origin.
4. Windows virtual-desktop absolute coordinates use `round_half_up((X - V.left) * 65535 / (V.width - 1))` and the corresponding y expression; a one-pixel virtual extent maps to zero. The result is intended for `MOUSEEVENTF_VIRTUALDESK`. Windows' exact internal rounding and DPI behavior are **not verified**. If hardware corner tests fail, compare with `SetCursorPos` in a per-monitor-DPI-aware process before choosing the M6/M7 path.
5. macOS mapping uses a host-local `HostDisplayGeometry` in points supplied by OS queries. It computes `origin_pt + (u / 65535) * extent_pt`; it never derives point size from the rounded scale factor.
6. Cursor position, bitmap size, and hotspot are scaled from host physical pixels to the rendered rectangle. Signed positions round half away from zero; extents and hotspot values round half-up; bitmap output stays at least one pixel and may cross the video rectangle.

### Worked Windows examples

**Three-wide layout:** displays are each `1920x1080` at origins `(-1920,0)`, `(0,0)`, `(1920,0)`. The virtual bounds are `(-1920,0,5760,1080)`. Absolute coordinates at the virtual corners are `(0,0)`, `(65535,0)`, `(0,65535)`, `(65535,65535)`. Host-pixel centers obtained from normalized midpoint `(32768,32768)` are `(-960,540)`, `(960,540)`, `(2880,540)`; Windows absolute results are `(10924,32798)`, `(32773,32798)`, `(54622,32798)`.

**Mixed DPI:** a `3840x2160` display at `(0,0)`, scale 1500, sits beside a `1920x1080` display at `(3840,0)`, scale 1000. Virtual bounds are `(0,0,5760,2160)`. The two normalized midpoint pixels are `(1920,1080)` and `(4800,540)`; their Windows absolute results are `(21849,32783)` and `(54622,16391)`. Scale is descriptive; physical pixel positions drive the mapping.

**Stream smaller than display:** for both 480p (`854x480`) and 720p (`1280x720`) on a 1080p or 4K display, pointer normalization is against the rendered rectangle, then maps against the display dimensions. The explicit test table confirms that the same normalized midpoint maps to the same host pixel independent of encoded stream dimensions.

The coordinate tables, property tests, and these numeric vectors establish only pure arithmetic behavior. They do not verify OS input APIs or real monitor arrangements; see `docs/HARDWARE.md`.
