# Rust SDK Audit — `sdk/rust/` (veyron-sdk)

Date: 2026-07-02
Scope: full audit of the Rust SDK against the wire protocol specified in
`docs/FRAMING.md` and `proto/veyron_protocol.proto`, followed by completion of
the SDK (protocol coverage, `Plugin` trait, `VeyronClient`, error handling,
tests, docs, crates.io metadata).

## 1. State before the audit

The SDK was a thin wrapper: ~230 lines of client, a 3-line framing re-export,
and a minimal `Plugin` trait. It worked for the happy path exercised by the
integration tests, but did not implement the full protocol.

### Findings

| ID | Severity | Finding |
|----|----------|---------|
| RS-01 | High | **No fragmentation support at all.** `FLAG_FRAGMENTED` was not re-exported; the client neither sent fragments nor reassembled inbound fragmented frames. A fragmented frame arriving at a plugin failed Protobuf decode with a confusing error. |
| RS-02 | Medium | **Unsecured sends bypassed compression.** `send()` without a session key used `write_frame`, which never compresses. Only MAC'd frames went through `write_frame_raw` (the compressing path). Plugins on `allow_no_auth` kernels sent large payloads uncompressed. |
| RS-03 | Medium | **`Plugin::run` ignored auth entirely.** It always connected without a secret and registered with an empty JWT, so the trait was unusable against a secured kernel (the default configuration — `allow_no_auth` is an explicit opt-out). |
| RS-04 | Medium | **No lifecycle handling in the receive loop.** `PluginShutdown` was passed to `on_message` instead of ending the loop; kernel `Ping`s were not answered (watchdog liveness); delivered `Event`s were never acknowledged, so the kernel kept retrying them. |
| RS-05 | Low | **`ping()` sent a bogus timestamp.** It sent `start.elapsed()` (~0 ms since the Instant was just created) instead of wall-clock time, making the kernel-side `original_timestamp` useless. |
| RS-06 | Low | **`register_full`-style version control missing.** The plugin version was hard-coded to `"1.0.0"`. |
| RS-07 | Low | **Registration rejection surfaced as a generic decode error.** An `ErrorMessage` reply to `PluginRegister` produced "expected PluginRegisterAck" with no detail. |
| RS-08 | Low | **No request timeouts.** `recv()`, `send_command()`, and `register()` could hang forever on a stalled kernel. |
| RS-09 | Low | **No ActionRequest / Unsubscribe / EventAck / audio APIs.** Plugins had to hand-build envelopes for core protocol operations. |
| RS-10 | Info | **No tests, no docs, no crates.io metadata.** The SDK crate had zero tests of its own, no README, no rustdoc, and a manifest missing `description`/`license`/`readme`. |
| RS-11 | Info | **Duplicate manual frame-building code.** `send_raw` and `send_raw_with_flags` each hand-rolled the target-padding + CRC logic. |

### Things the old SDK already did right

- Re-exported the kernel framing layer instead of re-implementing it, so the
  wire format (including zstd normalization on read) could not drift — this
  matches the FRAMING.md note and was kept.
- MAC handling was correct: HKDF session-key derivation from the register
  ack's `session_nonce`, tags over the plaintext header+payload, constant-time
  verification, and rejection of untagged inbound frames when secured.
- Socket-path resolution honored BUG-006 (per-user runtime dir, never shared
  `/tmp`).

## 2. What was done

### `src/client.rs` — full-protocol `VeyronClient`

- **Fragmentation (RS-01):** `send_fragmented(target, payload, chunk_size)`
  emits `FLAG_FRAGMENTED` frames with the 10-byte big-endian fragment header
  (`fragment_id`, `sequence`, `total`, `stream_id`) per FRAMING.md; each
  fragment is individually MAC'd on secured connections. Inbound fragments are
  reassembled transparently in `recv_frame`/`recv` with the same bounds the
  kernel enforces: ≤ 64 concurrent streams, reassembled size ≤ 1 MiB,
  incomplete sets pruned after 30 s, hard errors on header/total violations.
- **Compression (RS-02):** all sends now go through `write_frame_raw`, so
  payloads ≥ 64 KiB are zstd-compressed whether or not the connection is
  secured. Raw-binary (audio) payloads are never compressed, per FRAMING.md.
- **New APIs (RS-09):** `send_action` (ActionRequest with id matching +
  deadline), `unsubscribe`, `ack_event`, `send_audio_chunk`,
  `send_raw_audio` (FLAG_RAW_BINARY), `recv_frame` (raw frames + reassembly),
  `recv_timeout`, `connect_from_env`, `from_stream` (testability),
  `register_full` (explicit version, RS-06), `is_secured`.
- **Error handling:** registration rejections carried in `ErrorMessage` are
  surfaced with message + details (RS-07); `recv_timeout`/`send_action`
  return `VeyronError::Timeout` (RS-08); `recv()` on a raw-binary frame
  returns a directed error pointing at `recv_frame` instead of a Protobuf
  decode failure.
- `ping()` now sends wall-clock Unix millis (RS-05); frame construction is
  factored into one `build_frame` helper (RS-11).

### `src/plugin.rs` — complete `Plugin` trait

- Only `id`, `manifest`, `on_message` are required; `version`, `on_init`,
  `on_event`, `on_shutdown` have defaults.
- `run`/`run_with` read `VEYRON_JWT_TOKEN` / `VEYRON_JWT_SECRET` and register
  with them, so the trait works against secured kernels (RS-03). Registration
  rejection (`accepted == false`) is now an error instead of being ignored.
- Receive loop (RS-04): answers `Ping` with `Pong`, exits on
  `PluginShutdown`, dispatches `Event` to `on_event` and auto-acks on success
  (no ack on error → kernel retries), and passes everything else to
  `on_message`. New `serve(client, jwt)` building block for tests/custom
  transports.

### `src/framing.rs`, `src/lib.rs`

- Framing re-export now covers the full surface: `FLAG_FRAGMENTED`,
  `FRAG_HEADER_SIZE`, `FragmentHeader`, `parse_frag_header`,
  `read_frame_with_timeout`, `serialize_header`, `target_as_str`.
- New public modules: `veyron_sdk::proto` (generated Protobuf types) and
  `veyron_sdk::frame_mac` (shared MAC primitives), so plugin authors no longer
  need to depend on the `veyron` kernel crate directly.
- Crate-level rustdoc with a compile-tested quick-start example.

### Tests (RS-10)

`cargo test` in `sdk/rust/`: **15 tests, all passing** (12 protocol tests +
2 unit + 1 doctest), no kernel required (they run over `UnixStream::pair`):

- envelope round-trip; compression-on-wire + normalization-on-read
- secured registration (nonce → HKDF key), kernel-side MAC verification,
  rejection of untagged inbound frames when secured
- fragmentation: client-level reassembly round-trip, exact wire format of the
  fragment header per FRAMING.md, oversized-payload rejection
- raw-binary frames bypass Protobuf; `recv()` rejects them with a clear error
- `recv_timeout`; MAC tag round-trip over the serialized header
- full `Plugin::serve` loop: register → Ping/Pong → Event/EventAck →
  PluginShutdown exit, with `on_init`/`on_shutdown` invocation asserted

Kernel-in-the-loop coverage unchanged and passing:
`tests/integration/test_sdk_rust.rs`, `tests/unit/test_sdk.rs` (main repo),
plus `examples/echo_plugin_rs` builds against the new SDK unmodified — the
existing public API is fully backward compatible.

`cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`: clean.

### `Cargo.toml` (crates.io readiness)

Added `description`, `license = "MIT"` (matches repo LICENSE), `readme`,
`repository`, `documentation`, `keywords`, `categories`, `rust-version`, and
`version = "0.1.0"` on the `veyron` path dependency (required for publishing).
Tokio features narrowed from `full` to what the SDK uses
(`net`, `io-util`, `time`, `rt`, `macros`).

## 3. Remaining gaps / follow-ups

| ID | Item |
|----|------|
| F-01 | **Publishing blocker:** `veyron-sdk` depends on the `veyron` kernel crate by path. crates.io requires the dependency to be published first. Longer-term the protocol pieces (`ipc::framing`, `auth::frame_mac`, generated proto) should be split into a small `veyron-protocol` crate so plugin authors don't compile the whole kernel (axum, reqwest, etc.). |
| F-02 | Client-side reassembly is defensive: today the kernel reassembles inbound fragments itself and always forwards whole frames to plugins, so plugins should not normally see `FLAG_FRAGMENTED`. Supported anyway for protocol completeness and peer implementations. |
| F-03 | `send_action`/`send_command` discard unrelated envelopes that arrive while waiting. Fine for the single-task `Plugin` model; a multiplexing client (background read task + oneshot response routing) would be needed for concurrent requests. |
| F-04 | `FLAG_PRIORITY` (bit 3) remains reserved/unimplemented protocol-wide — nothing to do in the SDK until the kernel defines semantics. |
| F-05 | Python and C++ SDKs still lack compression normalization (R5-01) and fragmentation; frames ≥ 64 KiB still break non-Rust plugins. Out of scope here, tracked in FRAMING.md. |
