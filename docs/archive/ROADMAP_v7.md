# Veyron ROADMAP — Phase 7

**Baseline:** 2026-07-13 · Kernel `0.1.0`
**Branch:** `develop`
**Previous phases:** `docs/archive/` (Phase 1–2: `ROADMAP_phase1.md`/`ROADMAP_v2.md`/`ROADMAP_v3.md` · Phase 3–4: `ROADMAP_v4.md` · Phase 5: `ROADMAP_v5.md` · Phase 6 (network protocol support + full audit remediation): `ROADMAP_v6.md`, all items complete)

---

## Manifesto (non-negotiable)

- Kernel = dumb byte router + process supervisor. Zero business logic. Zero AI. Zero application databases.
- Intra-host IPC = UDS only. No TCP, no Redis, no queues.
- Protocol = single `.proto` file. Changes propagate to all SDKs.
- Plugin = isolated OS process. Cannot bypass kernel. Speaks only UDS.
- External access = WebSocket/HTTP gateway only (Axum).

---

## Current baseline — 2026-07-13

Phase 6 landed the wire protocol for event-publish (R6-01), streaming actions
(R6-02), per-caller quotas (R6-03), and long-lived sessions (R6-04) — but
**kernel + Rust SDK only** in every case. C++ and Python SDKs were explicitly
scoped out as "deferred as follow-up" (`docs/archive/ROADMAP_v6.md`). That
follow-up is Phase 7.

## Phase 7 — C++ / Python SDK parity with Rust

The Rust SDK (`sdk/rust/src/client.rs`) is the reference implementation.
Wire format is already final (proto mirrored to all three SDKs, no proto
work needed here) — this phase is pure client-library work, one pair of
SDKs at a time.

### P7-01 — `publish_event` in C++ and Python SDKs ✅ done

Rust reference: `VeyronClient::publish_event()` (`sdk/rust/src/client.rs:465`).
Sends `EventPublish`, awaits `EventPublishAck`, surfaces
`EventPublishStatus` (`EVENT_PUBLISH_OK`/`ERROR`/`PERMISSION_DENY`) as a
typed result/exception.

Shipped: `VeyronClient::publish_event()` in `sdk/cpp/include/veyron/client.hpp`
+ `.cpp` (new `read_frame_full_with_deadline` primitive in `framing.hpp/.cpp`
to bound the total wait, not just mid-frame completion), and
`async def publish_event()` in `sdk/python/veyron/client.py` (also
regenerated `veyron_protocol_pb2.py`, which had never been rebuilt since
`EventPublish`/`EventPublishAck` were added to the proto in R6-01). Both
SDKs bumped to 0.1.1 and published (`veyron-sdk` on crates.io and PyPI).
5 test cases per SDK: OK ack, PERMISSION_DENY ack returned not raised,
kernel Error raises, timeout raises, unrelated envelope discarded.

### P7-02 — Streaming actions in C++ and Python SDKs ✅ done

Rust reference: `send_action_streaming()` (`client.rs:571`),
`send_request_chunk()`/`send_response_chunk()` (`:594`/`:618`), plus
`send_action()`'s handling of an inbound `ActionStreamAbort` for the
awaited `action_id`.

Scope grew during design (`docs/superpowers/specs/2026-07-14-p7-02-streaming-actions-sdk-design.md`):
neither SDK had plain non-streaming `send_action` yet either, and its
`ActionStreamAbort` handling can't be ported separately from the
deadline-loop it lives in, so this shipped all five of `send_action`,
`send_action_streaming`, `send_request_chunk`, `send_response_chunk`, and
`close_session` (send side) together. `ActionRequestChunk`/
`ActionResponseChunk` are a distinct wire message from `FLAG_FRAGMENTED`
fragmentation (T-18) — not reused, per the original scoping note above.

Shipped: all five methods in `sdk/cpp/include/veyron/client.hpp` + `.cpp`
and `sdk/python/veyron/client.py`. Both SDKs extracted a shared
deadline-loop helper (`wait_for_response` in C++, `_await_matching` in
Python) now used by both `publish_event` and `send_action`, per P7-01's
deferred note. 8 test cases per SDK
(`sdk/cpp/tests/test_send_action.cpp`, `tests/python/test_send_action.py`).

### P7-03 — Session close in C++ and Python SDKs ✅ done

Rust reference: `close_session()` (`client.rs:646`), sends `SessionClose{action_id, reason}`.
The send side shipped early as part of P7-02 (mechanically identical to the
chunk senders). `recv()` in both SDKs already decoded `SessionClose`
correctly as a plain `Envelope` oneof field (present since R6-04/P7-02) —
no dispatch code was needed, distinguishing it from `ActionStreamAbort` is
inherent to protobuf oneof `HasField`. This closed the test gap only:
`sdk/cpp/tests/test_session_close.cpp` (2 cases) and
`tests/python/test_session_close.py` (2 cases), mirroring Rust SDK test
`recv_distinguishes_session_close_from_stream_abort`.

### P7-04 — Cross-SDK integration coverage ✅ done

Design: `docs/superpowers/specs/2026-07-14-p7-04-cross-sdk-integration-design.md`.
Full matrix: 3 message types × 2 SDKs = 6 new integration tests, kernel
always the real Rust kernel (`SdkHarness`), counterpart always the Rust SDK
test harness driven directly from `#[tokio::test]` (not a second subprocess
— matches the existing `cpp_sdk_echo_plugin_round_trip` /
`python_sdk_register_and_ping` pattern).

Both `sdk/cpp/examples/echo_plugin.cpp` and `sdk/python/examples/echo_plugin.py`
gained a `stream_echo` action (accumulates `ActionRequestChunk`s by seq until
`final`, replies with 2 `ActionResponseChunk`s + a terminal `ActionResponse`),
a `publish_test` action (calls `publish_event`, replies OK/ERROR from the
ack), and a `SessionClose` → `session_closed:<reason>` stdout branch.
`stream_echo` also sends an early accepting `ActionResponse{OK}` immediately
on the initial streaming `ActionRequest` — discovered during planning that
the kernel's `SessionClose` handler (`src/ipc/protocol.rs`) rejects the
request until `PendingAction::session_accepted` flips true, which only
happens on a provider `ActionResponse{OK}` for a streaming action
(`src/plugins/registry.rs::resolve_action_response`); without the early
accept, a mid-stream `SessionClose` can never reach the plugin.

Shipped: `cpp_sdk_streaming_action_round_trip`, `cpp_sdk_publish_event_from_plugin`,
`cpp_sdk_session_close_dispatch` in `tests/integration/test_sdk_cpp.rs`;
`python_sdk_streaming_action_round_trip`, `python_sdk_publish_event_from_plugin`,
`python_sdk_session_close_dispatch` in `tests/integration/test_sdk_python.rs`
(the latter spawning the real `examples/echo_plugin.py` via
`python3 -m examples.echo_plugin`, a first for the Python integration
tests — prior ones only ran inline `-c` scripts). Each skips gracefully
without the relevant subprocess dependency, matching existing convention.

---

## Task Summary

| Item | Scope | Depends on |
|------|-------|------------|
| P7-01 | `publish_event` — C++ + Python ✅ done | none |
| P7-02 | streaming actions — C++ + Python ✅ done | none |
| P7-03 | `close_session` recv-side dispatch — C++ + Python ✅ done | P7-02 |
| P7-04 | cross-SDK integration tests ✅ done | P7-01..03 |

**Ship gate:** not yet scheduled — candidate work, pick up when a plugin
(e.g. `network`) actually needs streaming/session/event-publish from a
non-Rust SDK. No proto changes required; this is pure SDK client work.

## Definition of Done

- `cargo test --all --all-features` exits 0; new behavior has regression tests.
- `cargo clippy --all-targets --all-features -- -D warnings` clean; `cargo fmt --check` clean.
- C++: existing CMake test targets stay green; new tests follow the
  `sdk/cpp/tests/test_*.cpp` naming/registration pattern in `CMakeLists.txt`.
- Python: new tests follow the `tests/python/test_*.py` pattern.
- Docs updated in the same PR (README for operator-visible changes; no
  `docs/FRAMING.md` changes expected since the wire format doesn't change).
