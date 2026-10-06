# Racc Connect wire protocol v0

Status: draft version 0. Integers are little-endian. The Rust reference implementation is **crates/proto** (package **racc-proto**).

## Channels and session setup

| Channel | Transport | Contents |
|---|---|---|
| Control | TCP, length-prefixed, TCP_NODELAY | Handshake, topology, stream control, input, clipboard, telemetry, cursor shapes, keepalive |
| Video | UDP | H.264 Annex B fragments and cursor position/visibility |

Session order: TCP connect to the host's Tailscale address; viewer sends **Hello** (1); host returns **HelloAck** (2); host sends **TopologyAnnounce** (3); host sends successful **StreamReset** (5); host starts UDP video. The host checks Tailscale LocalAPI whois and its allowlist before accepting. WireGuard provides peer authentication and encryption; there is no application-layer encryption.

Video datagrams contain no session ID. The host sends to the viewer Tailscale IP and UDP port announced in Hello.video_udp_port. The viewer accepts datagrams only from the host IP address of the associated TCP control connection.

## Constants and common encoding

| Constant | Value | Meaning |
|---|---:|---|
| PROTOCOL_VERSION | 0 | Draft wire version |
| MAX_DATAGRAM | 1200 bytes | Maximum UDP datagram including header |
| VIDEO_HEADER_LEN | 18 bytes | Video slice header |
| MAX_VIDEO_PAYLOAD | 1182 bytes | Maximum video fragment payload |
| MAX_FRAGMENTS_PER_FRAME | 1024 | Maximum fragments in a frame |
| MAX_CONTROL_FRAME_BYTES | 1,048,576 bytes | Max TCP body after length prefix, including type byte |
| MAX_CLIPBOARD_BYTES | 524,288 bytes | Maximum UTF-8 clipboard data |
| MAX_DISPLAYS | 16 | Maximum display entries |
| MAX_NAME_BYTES | 128 bytes | Maximum encoded UTF-8 name/version |
| MAX_CURSOR_DIM | 128 pixels | Maximum cursor width or height |
| MAX_CURSOR_BYTES | 65,536 bytes | Maximum BGRA bitmap |

Strings use a one-byte length followed by that many UTF-8 bytes. Limits count encoded bytes. Invalid UTF-8 is rejected.

## UDP datagrams

### Video slice, kind 1

Offsets are from the beginning of the datagram.

| Offset | Bytes | Type | Field |
|---:|---:|---|---|
| 0 | 1 | u8 | version, exactly PROTOCOL_VERSION |
| 1 | 1 | u8 | kind = 1 |
| 2 | 1 | u8 | flags |
| 3 | 1 | u8 | reserved = 0 |
| 4 | 2 | u16 | epoch |
| 6 | 4 | u32 | frame_id |
| 10 | 2 | u16 | frag_idx |
| 12 | 2 | u16 | frag_cnt |
| 14 | 4 | u32 | capture_ts_us, wrapping host capture time |
| 18 | 1–1182 | bytes | H.264 Annex B access-unit fragment |

Flags: bit 0 KEY, bit 1 LAST_FRAGMENT, bit 2 CONFIG (SPS/PPS present); bits 3–7 are reserved and must be zero. Fragment count is 1..=1024; index is less than count; LAST is set iff index equals count minus one. Payload is nonempty and total datagram length is at most 1200. Since there is no payload-length field, a shorter datagram with a valid header and nonempty payload is indistinguishable from a valid shorter fragment. Join fragments by increasing index. Parsing borrows payload bytes and allocates nothing.

### Cursor update, kind 2

Fixed size: 19 bytes.

| Offset | Bytes | Type | Field |
|---:|---:|---|---|
| 0 | 1 | u8 | version, exactly PROTOCOL_VERSION |
| 1 | 1 | u8 | kind = 2 |
| 2 | 1 | u8 | flags = 0 |
| 3 | 1 | u8 | reserved = 0 |
| 4 | 2 | u16 | epoch |
| 6 | 4 | u32 | shape_id, references CursorShape |
| 10 | 4 | i32 | x, host physical pixel relative to streamed display top-left |
| 14 | 4 | i32 | y, host physical pixel relative to streamed display top-left |
| 18 | 1 | u8 | visible, exactly 0 or 1 |

Viewer applies the bitmap hotspot and scales the position. Cursor bitmap travels on TCP, never in UDP.

## TCP framing

The frame is a four-byte unsigned little-endian length, then a one-byte type, then payload. Length counts the type byte plus payload, so it is 1..=MAX_CONTROL_FRAME_BYTES.

| Offset | Bytes | Type | Field |
|---:|---:|---|---|
| 0 | 4 | u32 | body length, excluding this prefix |
| 4 | 1 | u8 | message type |
| 5 | variable | bytes | payload |

The incremental decoder accepts arbitrary chunks, including one byte. It rejects zero and oversized lengths as soon as the full prefix arrives; its partial body buffer is bounded by MAX_CONTROL_FRAME_BYTES. A complete control body must be consumed exactly.

## Control messages

Payload offsets below start after the type byte. Strings are a one-byte length followed by UTF-8. Variable offsets follow preceding strings or lists.

| Type | Name | Direction |
|---:|---|---|
| 1 | Hello | Viewer → host |
| 2 | HelloAck | Host → viewer |
| 3 | TopologyAnnounce | Host → viewer |
| 4 | SwitchMonitor | Viewer → host |
| 5 | StreamReset | Host → viewer |
| 6 | SetQuality | Viewer → host |
| 7 | RequestKeyframe | Viewer → host |
| 8 | PauseVideo | Viewer → host |
| 9 | ResumeVideo | Viewer → host |
| 10 | InputEvent | Viewer → host |
| 11 | ClipboardUpdate | Either |
| 12 | StatsReport | Host → viewer |
| 13 | Ping | Either |
| 14 | Pong | Either |
| 15 | CursorShape | Host → viewer |
| 16 | Goodbye | Either |

### 1. Hello

| Payload offset | Size/type | Field |
|---:|---|---|
| 0 | 1 / u8 | protocol_version |
| 1 | variable / string | device_name |
| after name | 1 / u8 | os |
| after OS | variable / string | app_version |
| after version | 2 / u16 | video_udp_port |
| next | 4 / u32 | codecs |
| next | 2 / u16 | max_height |
| next | 4 / u32 | features |

Codec mask bit 0 is H.264; feature bit 0 is text clipboard. Other bits are invalid.

### 2. HelloAck

| Payload offset | Size/type | Field |
|---:|---|---|
| 0 | 1 / u8 | protocol_version |
| 1 | 1 / u8 | status |
| 2 | variable / string | device_name |
| after name | 1 / u8 | os |
| after OS | variable / string | app_version |
| next | 4 / u32 | codecs |
| next | 2 / u16 | max_height |
| next | 4 / u32 | features |
| final | 1 / u8 | host_cpu_cores |

OS: 0 Unknown, 1 Windows, 2 macOS, 3 Linux. HelloAck status: 0 Ok, 1 UnsupportedVersion, 2 NotAuthorized, 3 Busy.

### 3. TopologyAnnounce

Payload header: offset 0 u32 topology_rev; offset 4 u32 active_display_id (zero means none); offset 8 u8 display_count (0..=16); offset 9 begins records.

Each record is sequential: u32 display_id; string name; i32 x; i32 y; u32 width_px; u32 height_px; u16 scale_milli; u32 refresh_mhz; u8 flags. IDs are unique within the message; width and height are nonzero. Flag bits: 0 primary, 1 active, 2 available, 3 read-only HDR label; other bits invalid.

### 4. SwitchMonitor

Offset 0 u32 req_id; offset 4 u32 display_id.

### 5. StreamReset

| Offset | Size/type | Field |
|---:|---|---|
| 0 | 4 / u32 | req_id (zero if host-initiated) |
| 4 | 2 / u16 | epoch |
| 6 | 1 / u8 | codec |
| 7 | 2 / u16 | width |
| 9 | 2 / u16 | height |
| 11 | 1 / u8 | fps |
| 12 | 4 / u32 | topology_rev |
| 16 | 4 / u32 | display_id |
| 20 | 1 / u8 | status |

Codec: 1 H.264. Status: 0 Ok, 1 DisplayNotFound, 2 CaptureFailed, 3 EncoderFailed, 4 Paused, 5 Busy. Successful status requires nonzero width, height and fps.

### 6. SetQuality

Offset 0 u16 max_height (0 Auto; otherwise only 480, 720 or 1080); offset 2 u32 bitrate_hint_kbps (0 Auto).

### 7. RequestKeyframe

Offset 0 u16 epoch.

### 8. PauseVideo and 9. ResumeVideo

Empty payloads.

### 10. InputEvent

Payload offsets: 0 u16 epoch; 2 u32 display_id; 6 u8 tag; 7 event data.

| Tag | Event | Data after tag |
|---:|---|---|
| 1 | MouseMoveAbs | u16 u, u16 v (normalized 0..=65535) |
| 2 | MouseMoveRel | i16 dx, i16 dy (raw units) |
| 3 | MouseButton | u8 button, u8 pressed; buttons 1 left, 2 right, 3 middle, 4 back, 5 forward |
| 4 | Wheel | i16 dx, i16 dy (units of 1/120 notch) |
| 5 | Key | u16 hid_usage, u8 pressed, u8 modifiers |

Boolean fields are exactly 0 or 1. Key identity is USB HID usage page 0x07. Modifier bits 0–3 are shift, control, alt and meta. Unknown tags, other modifier bits, or button numbers outside 1..=5 are invalid.

### 11. ClipboardUpdate

Offset 0 u32 seq; offset 4 u8 origin (0 viewer, 1 host); offset 5 u8 kind (1 UTF-8 text); offset 6 u32 byte_length (at most 524288); offset 10 text bytes. Other kinds and invalid UTF-8 are rejected.

### 12. StatsReport

| Offset | Size/type | Field |
|---:|---|---|
| 0 | 2 / u16 | host_cpu_pct_x10 (0..=1000) |
| 2 | 1 / u8 | capture_backend |
| 3 | 1 / u8 | encoder |
| 4 | 2 / u16 | width |
| 6 | 2 / u16 | height |
| 8 | 4 / u32 | display_refresh_mhz |
| 12 | 4 / u32 | target_bitrate_kbps |
| 16 | 4 / u32 | actual_bitrate_kbps |

Capture backend: 0 Unknown, 1 DXGI, 2 WGC, 3 ScreenCaptureKit, 4 CGDisplayStream. Encoder: 0 Unknown, 1 MediaFoundationHw, 2 OpenH264, 3 VideoToolbox, 4 Nvenc, 5 Amf, 6 Qsv.

### 13. Ping and 14. Pong

Both contain u64 nonce at offset 0 and a u64 timestamp at offset 8. Ping uses sender_ts_us; Pong echoes it as echo_ts_us. Nonce and timestamp support RTT telemetry.

### 15. CursorShape

Offset 0 u32 shape_id; 4 u16 width; 6 u16 height; 8 u16 hotspot_x; 10 u16 hotspot_y; 12 BGRA pixel bytes. Width and height are 1..=128; hotspots are less than their dimensions. No pixel length prefix is present: exact checked byte length is width × height × 4, at most 65536.

### 16. Goodbye

Offset 0 u8 reason: 0 Normal, 1 Error, 2 Superseded, 3 NotAuthorized, 4 Shutdown.

## Validation and errors

Decoders reject truncated fields, oversized declared lengths, invalid closed enums, reserved mask bits, unknown types/kinds, unsupported datagram versions, invalid UTF-8, malformed booleans, inconsistent fragments, duplicate display IDs, zero display dimensions and trailing bytes. Length arithmetic is checked; length fields are validated before allocation.

The API uses ProtoError: Truncated, TooLarge, InvalidValue, UnknownKind, UnknownMessageType, UnsupportedVersion, InvalidUtf8, or TrailingBytes. ControlMessage::decode_body requires exact consumption. Each ControlPayload::decode_payload returns a value and bytes consumed.

## Golden vectors

Hex bytes below are asserted in crates/proto/tests/golden.rs.

Video KEY|LAST_FRAGMENT|CONFIG, epoch 0x1234, frame 0x01020304, fragment 2 of 3:
~~~text
00 01 07 00 34 12 04 03 02 01 02 00 03 00 0d 0c 0b 0a 00 00 00 01 65
~~~

Video middle fragment with KEY|CONFIG, fragment 1 of 3:
~~~text
00 01 05 00 34 12 04 03 02 01 01 00 03 00 0d 0c 0b 0a 00 00 01 41
~~~

Cursor update, shape 0x11223344, x=-2, y=0x01020304, visible:
~~~text
00 02 00 00 34 12 44 33 22 11 fe ff ff ff 04 03 02 01 01
~~~

Hello frame and four-byte body-length prefix:
~~~text
13 00 00 00 01 00 01 41 02 01 31 34 12 01 00 00 00 38 04 01 00 00 00
~~~

TopologyAnnounce with two displays, first x=-1920:
~~~text
46 00 00 00 03 01 00 00 00 01 00 00 00 02
01 00 00 00 02 44 31 80 f8 ff ff 00 00 00 00 80 07 00 00 38 04 00 00 dc 05 60 ea 00 00 07
02 00 00 00 02 44 32 00 00 00 00 00 00 00 00 00 05 00 00 00 04 00 00 e8 03 60 ea 00 00 04
~~~

StreamReset successful H.264 1280×720 at 30 fps:
~~~text
16 00 00 00 05 00 00 00 00 02 00 01 00 05 d0 02 1e 03 00 00 00 01 00 00 00 00
~~~

InputEvent frames: MouseMoveAbs, MouseMoveRel, MouseButton, Wheel and Key:
~~~text
0c 00 00 00 0a 34 12 01 00 00 00 01 00 80 ff ff
0c 00 00 00 0a 34 12 01 00 00 00 02 fe ff 2c 01
0a 00 00 00 0a 34 12 01 00 00 00 03 04 01
0c 00 00 00 0a 34 12 01 00 00 00 04 88 ff f0 00
0c 00 00 00 0a 34 12 01 00 00 00 05 04 00 01 03
~~~

ClipboardUpdate text “hi”:
~~~text
0d 00 00 00 0b 07 00 00 00 00 01 02 00 00 00 68 69
~~~

CursorShape with two BGRA pixels:
~~~text
15 00 00 00 0f 05 00 00 00 02 00 01 00 01 00 00 00 11 22 33 44 55 66 77 88
~~~

## Versioning and compatibility

Any change to wire headers, type numbers, fields, enum values, limits or validation requires a PROTOCOL_VERSION bump and this file updated in the same commit. Unknown closed-enum values and reserved bits are not ignored. Version 0 is draft; no other compatibility is implied.

## Not in v0

Forward error correction, NACK retransmission, audio, HEVC, AV1, clipboard images/files, Unicode text injection for international keyboard layouts, and application-layer encryption beyond Tailscale WireGuard.

## Verification

Property tests cover valid round trips, arbitrary malformed bytes and mutations of valid frames. Default proptest cases stay quick; stress with PROPTEST_CASES=10000 cargo test -p racc-proto.
