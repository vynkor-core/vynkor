# R6-04 — WebSocket / long-lived-connection action model

**Date:** 2026-07-10
**Status:** approved, ready for implementation plan
**Roadmap item:** `ROADMAP.md` Phase 6, R6-04

## Problem

`ActionRequest`/`ActionResponse` is a single request/response pair. R6-02 added `ActionRequestChunk`/`ActionResponseChunk` for incremental bodies, but a stream still terminates the moment `ActionResponse` fires — there's no primitive for a session that stays open indefinitely and exchanges chunks in both directions over its lifetime (e.g. a WS-like connection a `network` provider exposes to callers). This spec covers that primitive.

Source: `veyron-plugins/plugins/network/KERNEL_PROTOCOL_TODO.md` item 4 (gitignored local notes, not in this repo) — no existing implementation to anchor on, this is speculative roadmap work.

## Non-goals

- No renumbering of `ActionStatus`/`CommandStatus` (T-16, separately deferred).
- No C++/Python SDK session helpers in this change — kernel + Rust SDK only, matching R6-01/02/03 precedent.
- No new `PermissionType`. Opening a session reuses the existing action-declaration model (`manifest.actions` + T-19's requester-permission check) unchanged.
- No per-chunk permission re-check — same one-time-check precedent as R6-02.
- No partial-session resume. A session that ends (gracefully or forced) is over; a new one requires a fresh `ActionRequest`.
- No change to `ActionRequestChunk`/`ActionResponseChunk`/`ActionStreamAbort` wire shape — R6-02's messages are reused as-is.

## Design

### Lifecycle

```
Requester                          Kernel                          Provider
    |-- ActionRequest{streaming:true} -->|-- (routed as today) -->|
    |                                    |<-- ActionResponse{OK} --|   (accepts; session open)
    |<-- ActionResponse{OK} -------------|                         |
    |                                    |                         |
    |<==== ActionRequestChunk / ActionResponseChunk, either direction, any time ====>|
    |                                    |                         |
    |-- SessionClose{action_id} -------->|-- SessionClose -------->|   (graceful, either side)
```

- `ActionRequest{streaming: true}` opens a session exactly as R6-02 opens a stream.
- Provider's first (and only mandatory) `ActionResponse` decides accept/reject:
  - `status: ACTION_OK` → session accepted, stays open, `pending_actions` entry is **not** evicted (diverges from R6-02, where the terminal `ActionResponse` always evicts).
  - Any error status → session rejected, evicted immediately, identical to today's non-streaming failure path. No `SessionClose` needed for a rejection.
- After acceptance, `ActionRequestChunk`/`ActionResponseChunk` flow in either direction, any number of times, in any order relative to each other (kernel does not enforce `seq` ordering, per the manifesto — dumb byte router).
- Session ends one of two ways:
  1. **Graceful** — either peer sends `SessionClose{action_id, reason}`. Kernel forwards it to the other side and evicts the `pending_actions` entry.
  2. **Forced** — kernel sends `ActionStreamAbort{action_id, reason}` to the surviving side and evicts. Covers: `try_send` failure to either connection (reused from R6-02's backpressure handling), a peer's connection dropping, or idle timeout (below).

### Wire changes

`wire/proto/veyron_protocol.proto` (mirrored verbatim to `sdk/cpp/proto/`, `sdk/python/proto/`):

```proto
// R6-04: graceful termination of a long-lived streaming session, sent by
// either peer. Kernel forwards to the other side and evicts pending_actions.
// Forced termination (backpressure, disconnect, idle timeout) still uses
// the existing ActionStreamAbort (R6-02) — SessionClose is peer-initiated only.
message SessionClose {
  string action_id = 1;
  string reason     = 2;  // human-readable, e.g. "done", "client closed"
}
```

No changes to `ActionRequest`, `ActionResponse`, `ActionRequestChunk`, `ActionResponseChunk`, `ActionStreamAbort`, or `ActionStatus`. `ActionRequest.streaming` (added in R6-02) is the sole trigger — this spec changes what happens *after* acceptance, not the open handshake.

### Kernel state (`src/plugins/registry.rs`)

`PendingAction` gains:

```rust
pub struct PendingAction {
    // ...existing fields (requester_write_tx, original_action_id, requester_id, deadline)...
    pub provider_id: String,       // already tracked as of T-19/R6-02, confirm reused not duplicated
    pub last_activity: Instant,    // updated on every ActionRequestChunk/ActionResponseChunk in either direction
    pub session_accepted: bool,    // false until the provider's first ActionResponse{OK}; gates SessionClose/chunk routing
}
```

- Pre-acceptance, the entry is still subject to the existing R5-07 dead-action sweep (`deadline` computed from `ActionRequest.timeout_ms`) — an unaccepted session that never gets a response times out exactly like a non-streaming action does today.
- Post-acceptance (`session_accepted = true`), the entry is exempt from that sweep — it may legitimately live far longer than `timeout_ms`, which only ever governed the accept/reject window. It becomes subject to the new idle-timeout sweep instead.

### Idle timeout

New `Config::session_idle_timeout_secs: Option<u32>` (`src/utils/config.rs`, `config.yaml`), default `None` = disabled (matches R6-03's "unset = unlimited" convention). When set, the existing 60s prune tick additionally scans accepted sessions: any `PendingAction` with `session_accepted: true` and `now - last_activity > session_idle_timeout_secs` gets `ActionStreamAbort{reason: "idle timeout"}` sent to both requester and provider, then evicted. Bounded to tick-interval precision, same tradeoff R5-07 already accepted for its own sweep.

### Kernel routing (`src/ipc/protocol.rs`)

- **`ActionResponse` arm** (existing, from R5-07): when the resolved `PendingAction` corresponds to a `streaming: true` request and `status == ACTION_OK`, set `session_accepted = true` and **do not** evict — this is the one behavioral branch added to existing code. Every other `ActionResponse` path (non-streaming, or streaming+error) evicts exactly as today.
- **`ActionRequestChunk`/`ActionResponseChunk` arms** (existing, from R6-02): update `last_activity` on the matched `PendingAction` in addition to forwarding. No other change — same sender-identity verification, same `try_send`-failure-triggers-abort behavior.
- **New `SessionClose` arm**: look up `action_id`; verify `sender_id` matches either the recorded `requester_id` or `provider_id` (either peer may initiate); if `session_accepted` is false, reject as a protocol error (nothing to close yet — the action either hasn't been accepted or was already rejected/evicted). On match: forward the `SessionClose` envelope to the *other* side via `try_send` (best-effort — if the other side is already gone, the forward silently no-ops, same tolerance pattern as R5-07's disconnect handling), then evict.

No new permission enum. No reassembly, no `seq` enforcement — unchanged from R6-02.

### Rust SDK (`sdk/rust/src/client.rs`)

- `VeyronClient::send_action_streaming` (from R6-02) is unchanged in signature; its returned handle now also exposes `close_session(reason: &str) -> Result<(), VeyronError>` (sends `SessionClose`) once the initial `ActionResponse{OK}` has been observed.
- Provider side: `respond_streaming` (from R6-02) — after the accepting `ActionResponse` is sent, the returned `ActionResponseChunkSender` gains the same `close_session(reason: &str)` method.
- Both directions: an inbound `SessionClose` for the relevant `action_id` surfaces as a clean end-of-stream (e.g. the chunk-receiving async stream terminates normally, distinct from the `Err(VeyronError::StreamAborted(reason))` R6-02 already defined for `ActionStreamAbort`) — callers can distinguish "peer closed cleanly" from "kernel forced it."

## Testing

- `tests/unit/test_registry.rs`: `PendingAction` acceptance transition (`session_accepted` flips on `ActionResponse{OK}` for a streaming request, entry survives); idle-timeout sweep only touches accepted sessions past the configured bound, using synthetic `Instant`s (same precedent as R5-07's sweep test — avoid a real 300s+ test).
- `tests/integration/test_kernel_commands.rs`:
  - End-to-end: open session, exchange chunks both directions over multiple round trips, graceful `SessionClose` from the requester forwards to the provider and evicts.
  - Rejection: `ActionResponse{status != OK}` for a streaming request evicts immediately, no `SessionClose` required, matches non-streaming failure behavior.
  - `SessionClose` before acceptance is rejected as a protocol error.
  - `SessionClose` from a third party (not the recorded requester/provider) is rejected.
  - Idle timeout (with `session_idle_timeout_secs` set low for the test) fires `ActionStreamAbort{reason:"idle timeout"}` to both sides and evicts.
  - `session_idle_timeout_secs` unset leaves an accepted session open indefinitely (bounded test duration, just asserts no abort fires within a short window).
- `sdk/rust/tests/`: `close_session` round trip both directions; inbound `SessionClose` distinguished from inbound `ActionStreamAbort` at the receiving stream.

## Definition of done

- `cargo test --all --all-features` exits 0, new tests above included.
- `cargo clippy --all-targets --all-features -- -D warnings` clean, `cargo fmt --check` clean.
- All three proto copies updated identically (T-17 drift check stays green).
- `docs/FRAMING.md` untouched (envelope-level change, no frame-flag changes).
- `ROADMAP.md` R6-04 marked done with a summary in the same style as R6-01/02/03.
