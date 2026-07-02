# Veyron Frame Flag Bit Space

This document is the **single source of truth** for all flag bits in the Veyron wire protocol.
No other file may define flag constants; all SDKs import the values defined here.

## Flag Bit Table

| Bit | Hex    | Constant         | Meaning |
|-----|--------|------------------|---------|
| 0   | 0x0001 | FLAG_MAC_PRESENT | 32-byte HMAC-SHA256 tag appended after payload |
| 1   | 0x0002 | FLAG_COMPRESSED  | Payload zstd-compressed on the wire (implemented — see below) |
| 2   | 0x0004 | FLAG_FRAGMENTED  | Frame is one fragment of a larger message (implemented — see below) |
| 3   | 0x0008 | FLAG_PRIORITY    | High-priority system frame (reserved, not yet implemented) |
| 4   | 0x0010 | FLAG_RAW_BINARY  | Payload is raw bytes (PCM or Opus); router skips Protobuf decode |
| 5–15 | —     | —                | Reserved |

### FLAG_COMPRESSED (Bit 1)

Implemented on the UDS path. The kernel's write path (`write_frame_raw`) transparently
zstd-compresses any non-raw-binary payload ≥ 64 KiB (`COMPRESS_THRESHOLD`) when the
compressed form is smaller, sets this flag, and rewrites `length`/`crc32` to describe
the wire (compressed) bytes. The read path decompresses and **normalizes**: after
`read_frame` returns, `payload` is always plaintext and `flags`/`length`/`crc32`
describe the plaintext.

**MAC interaction:** on secured connections the HMAC tag is computed over the
*plaintext* header and payload (before compression). Receivers must therefore verify
the tag against the normalized (decompressed) header/payload, not the raw wire bytes.

> **SDK status:** all three SDKs (Rust, Python, C++) decompress and normalize
> correctly (R5-01 ✓): payload is always plaintext after the read call, and MAC
> verification (when a session key is supplied) runs against the rebuilt plaintext
> header. Python depends on `zstandard`; C++ links `libzstd` via pkg-config. The
> WebSocket gateway rejects inbound frames carrying `FLAG_COMPRESSED` with a parse
> error (R5-03 ✓) rather than mishandle them — see below.

### FLAG_FRAGMENTED (Bit 2)

Implemented on the UDS path (kernel side). The first 10 bytes of the payload are the
fragment header: `[fragment_id: u16][sequence: u16][total: u16][stream_id: u32]`,
all big-endian. The kernel reassembles per `stream_id` with these bounds: max 64
concurrent streams per connection, reassembled size ≤ 1 MiB (`MAX_PAYLOAD_SIZE`),
incomplete sets discarded after 30 s. Violations drop the connection. Rejected on
the WebSocket gateway (R5-03 ✓) — see below.

> **SDK status:** the Rust SDK implements both sides — `VeyronClient::send_fragmented`
> emits spec-conformant fragments (each individually MAC'd when secured), and
> `recv`/`recv_frame` reassemble inbound fragments with the same bounds as the kernel.
> The Python and C++ SDKs do not implement fragmentation.

### FLAG_RAW_BINARY (Bit 4)

When set, the payload is raw binary audio data (PCM_S16LE or Opus). The kernel routes
the frame without Protobuf decode. Stream metadata must be negotiated out-of-band via an
`AudioStreamChunk` message on a prior frame.

Plugins that send frames with `FLAG_RAW_BINARY` set must hold `PERMISSION_AUDIO_STREAM`.
See [Audio Permissions](#audio-permissions) below.

## WebSocket Gateway Inbound Frame Support (R5-03)

`parse_frame` (`src/api/websocket.rs`) does not decompress or reassemble frames —
WS has its own native message framing, so `FLAG_FRAGMENTED` support isn't needed,
and normalizing `FLAG_COMPRESSED` before MAC verification/routing was out of scope
for the gateway. Rather than silently mis-verify a MAC or route a still-compressed
payload downstream, the gateway **rejects** any inbound binary frame carrying
`FLAG_COMPRESSED` or `FLAG_FRAGMENTED` with a parse error (counted the same as any
other malformed frame, subject to the existing `MAX_WS_PARSE_ERRORS` budget). This
does not affect kernel→WS outbound frames, which are never compressed (the gateway
does not call `write_frame_raw`).

## WebSocket JWT Delivery

**WebSocket JWT delivery:** The Veyron manifesto originally specified `?token=<jwt>` as
the URL query parameter for WebSocket auth. The implementation uses
`Sec-WebSocket-Protocol: veyron, <jwt>` instead. This is intentional: tokens in URL
query strings appear in server access logs, browser history, and proxy logs.
The header approach is superior. The manifesto text is superseded by this document.
Third-party clients must use the `Sec-WebSocket-Protocol` header.

## Audio Permissions

`PERMISSION_AUDIO_STREAM` — required for any plugin that sends frames with `FLAG_RAW_BINARY`
set, or sends `AudioStreamChunk` messages to another plugin.

Note: `PERMISSION_AUDIO` (value 5) is separate — it covers `play_audio` / `record_audio`
actions via `ActionRequest`. `PERMISSION_AUDIO_STREAM` gates peer-to-peer raw audio frame
routing.

## Audio Transport Convention

- **Local plugin-to-plugin over UDS:** prefer `PCM_S16LE` + `FLAG_RAW_BINARY`. Zero transcoding.
- **Plugin-to-external-client over WebSocket gateway:** prefer `OPUS`. Gateway transparently
  forwards; transcoding is the sending plugin's responsibility.
- Kernel never chooses codec. Kernel is dumb.
