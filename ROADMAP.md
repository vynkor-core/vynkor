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

### P7-01 — `publish_event` in C++ and Python SDKs

Rust reference: `VeyronClient::publish_event()` (`sdk/rust/src/client.rs:465`).
Sends `EventPublish`, awaits `EventPublishAck`, surfaces
`EventPublishStatus` (`EVENT_PUBLISH_OK`/`ERROR`/`PERMISSION_DENY`) as a
typed result/exception.

**Needed:** `VeyronClient::publish_event()` in `sdk/cpp/include/veyron/client.hpp`
+ `.cpp`, and `async def publish_event()` in `sdk/python/veyron/client.py`.
Neither SDK has any trace of `EventPublish` today (checked — zero matches).

### P7-02 — Streaming actions in C++ and Python SDKs

Rust reference: `send_action_streaming()` (`client.rs:571`),
`send_request_chunk()`/`send_response_chunk()` (`:594`/`:618`), plus
`send_action()`'s handling of an inbound `ActionStreamAbort` for the
awaited `action_id`.

**Needed:** port all three to both SDKs. Chunk send/recv must reuse each
SDK's existing fragmentation buffer plumbing (`sdk/cpp` gained
`FLAG_FRAGMENTED` support in T-18; `sdk/python/veyron/client.py`'s
`send_fragmented` already exists) — streaming chunks are a distinct
wire message (`ActionRequestChunk`/`ActionResponseChunk`), not the same
mechanism, so don't conflate the two, but the reassembly-buffer pattern
(`stream_id -> pending bytes`, idle prune, cap) is directly reusable.

### P7-03 — Session close in C++ and Python SDKs

Rust reference: `close_session()` (`client.rs:646`), sends `SessionClose{action_id, reason}`.
Depends on P7-02 landing first (sessions are built on the streaming
primitive — accept-in-place via the first `ActionResponse{OK}` on a
streaming request).

**Needed:** `close_session()` in both SDKs, plus each SDK's `recv()`/dispatch
loop distinguishing an inbound `SessionClose` from `ActionStreamAbort`
(mirrors Rust SDK test `recv_distinguishes_session_close_from_stream_abort`).

### P7-04 — Cross-SDK integration coverage

Once P7-01..03 land in both SDKs, add integration tests exercising each
combination that matters in practice: Rust-kernel + C++-plugin streaming
round trip, Python-plugin publish_event with a Rust subscriber, etc. — same
shape as the existing `tests/integration/test_sdk_cpp.rs` /
`test_sdk_python.rs` harnesses, extended to cover the new message types
instead of just ping/echo.

---

## Task Summary

| Item | Scope | Depends on |
|------|-------|------------|
| P7-01 | `publish_event` — C++ + Python | none |
| P7-02 | streaming actions — C++ + Python | none |
| P7-03 | `close_session` — C++ + Python | P7-02 |
| P7-04 | cross-SDK integration tests | P7-01..03 |

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
