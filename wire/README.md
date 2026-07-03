# veyron-wire

Shared wire-protocol crate for [Veyron](https://github.com/veyron-core/veyron):
frame framing, frame authentication, socket-path defaults, and the generated
Protobuf types. This is the protocol surface the Veyron kernel and its SDKs
both build on, so a plugin author can depend on `veyron-wire` alone instead
of pulling in the whole kernel crate.

## What's inside

- `framing` — frame read/write over an async stream (`write_frame`,
  `write_frame_raw`, `read_frame`, `read_frame_with_timeout`), fragmentation
  (`FragmentHeader`, `parse_frag_header`), and the wire constants
  (`MAX_PAYLOAD_SIZE`, `COMPRESS_THRESHOLD`, `FLAG_MAC_PRESENT`,
  `FLAG_COMPRESSED`, `FLAG_RAW_BINARY`, `FLAG_FRAGMENTED`).
- `mac` — HMAC-SHA256 frame authentication (`derive_session_key`,
  `compute_tag`, `verify_tag`).
- `socket` — default Unix socket path resolution (`default_socket_path`,
  `default_private_dir`), matching the kernel's `$XDG_RUNTIME_DIR` →
  `/run/user/<uid>` → `~/.veyron/run` fallback order.
- `proto::veyron` — protobuf types generated from `proto/veyron_protocol.proto`
  at build time via `prost-build`.
- `WireError` — the protocol-level error type returned by `framing` and
  `mac` functions.

## Who uses this

- The `veyron` kernel (path dependency, re-exports/wraps this crate's API
  so kernel call sites see no behavior change).
- `veyron-sdk` (Rust), via the published crates.io version.

C++ and Python SDKs can't depend on a Cargo crate directly — they vendor a
copy of `proto/veyron_protocol.proto` instead; see
[`veyron-sdk-cpp`](https://github.com/veyron-core/veyron-sdk-cpp) and
[`veyron-sdk-python`](https://github.com/veyron-core/veyron-sdk-python).

## MSRV

Rust 1.75.

## License

MIT
