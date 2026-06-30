# Veyron Frame Flag Bit Space

This document is the **single source of truth** for all flag bits in the Veyron wire protocol.
No other file may define flag constants; all SDKs import the values defined here.

## Flag Bit Table

| Bit | Hex    | Constant         | Meaning |
|-----|--------|------------------|---------|
| 0   | 0x0001 | FLAG_MAC_PRESENT | 32-byte HMAC-SHA256 tag appended after payload |
| 1   | 0x0002 | FLAG_COMPRESSED  | Payload compressed with zstd (reserved, not yet implemented) |
| 2   | 0x0004 | FLAG_FRAGMENTED  | Frame is one fragment of a larger message (reserved) |
| 3   | 0x0008 | FLAG_PRIORITY    | High-priority system frame (reserved) |
| 4   | 0x0010 | FLAG_RAW_BINARY  | Payload is raw bytes (PCM or Opus); router skips Protobuf decode |
| 5–15 | —     | —                | Reserved |

### FLAG_RAW_BINARY (Bit 4)

When set, the payload is raw binary audio data (PCM_S16LE or Opus). The kernel routes
the frame without Protobuf decode. Stream metadata must be negotiated out-of-band via an
`AudioStreamChunk` message on a prior frame.

Plugins that send frames with `FLAG_RAW_BINARY` set must hold `PERMISSION_AUDIO_STREAM`.
See [Audio Permissions](#audio-permissions) below.

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
