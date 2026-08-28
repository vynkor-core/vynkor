# CD-03 — AI Response Streaming (`{id}.chat`)

*Track C — `vynkor-wire` + `vynkor` + `vynkor-plugins/ai` · P0 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §3*

## Goal

AI response arrives as a token stream, not one chunk. Local typewriter masks latency but still feels "slow". Need real streaming + cancel.

---

## Already Exists

- `../vynkor-wire/proto` — generic streaming already DONE (R6-02/R6-04):
  - `ActionRequest{streaming:true}` → `PluginRegisterAck{session_nonce}` → `ActionResponse{OK, streaming}` → `ActionRequestChunk`/`ActionResponseChunk` (seq, chunk, final) → `SessionClose` (peer-initiated) / `ActionStreamAbort` (kernel, backpressure/disconnect/idle).
  - `src/plugins/registry.rs` — `PendingAction{streaming, session_accepted, last_activity}`, `resolve_action_response` (flips `session_accepted` on `OK`), `sweep_idle_sessions` (idle timeout), `sweep_expired_actions`.
  - `src/ipc/protocol.rs` — `forward`/`ActionRequestChunk`/`ActionResponseChunk`/`SessionClose` + `action_limiter` + `idle_timeout`.
  - `../vynkor-plugins/plugins/ai/src/handler.rs:handle_chat_completion` — single `send_action("http_request", ...)` → `network` → parse `NetworkHttpResponse` → `serde_json::to_vec(&result)` at once. **Not streaming.**
  - `../vynkor-plugins/plugins/ai/src/provider/{anthropic,openai_compat}.rs` — `build_http_request`/`parse_response` (whole).
  - Client — typewriter over non-stream response (1h adapt → real stream).

## Required

- [ ] **CD-03 — token stream + cancel (wire+ai+vynkor, 8–12h):**
  - **Option A — reuse event bus (recommended for MVP):** `ai` publishes `Event{event_type:"ai.delta", payload_json:{action_id, delta, seq}}` per token + `ai.done` / `ai.error` at end. Client subscribed to `ai.delta`. Kernel — zero changes (only `network`→`ai` already handles http_stream? check below).
  - **Option B — new frames `CHAT_DELTA` (as in plan):** new `proto` types `ChatDelta{action_id, seq, delta}` / `ChatDone{...}` / `ChatCancel{action_id}` — kernel also zero-parse (like chunks), just routes. Requires wire bump.
  - **Cancel:** `chat.cancel {action_id}` → `SessionClose{action_id, reason:"cancel"}` already exists; `ai` must forward `AbortSignal` to `network` `http_request` (or `fetch` abort). User tapped "stop" → request must not burn tokens.
  - **`ai` plugin:** `handle_chat_completion_stream` — instead of single `send_action("http_request")` whole `JSON`, open **streaming** `http_request` (SSE/chunked from `network` plugin — check if `network` supports `streaming:true`; if not — add) and as `data: {"delta":"..."}` arrives send `ActionResponseChunk` (or `Event ai.delta`) outward. At end — `ActionResponse{OK}` (R6-04 accept) + `SessionClose`.

  - **Files:**
    - `../vynkor-wire/proto/vynkor_protocol.proto` — if B: `message ChatDelta {string action_id; uint32 seq; string delta; bool final}` + `ChatCancel` (additive, `reserved` 4 in Envelope already busy — pick 60+ near `AudioStreamChunk`).
    - `src/ipc/protocol.rs` — if B: new `envelope::Payload::ChatDelta` branch (like `ActionResponseChunk`); if A — nothing (event bus already exists).
    - `../vynkor-plugins/plugins/ai/src/handler.rs` — `handle_chat_completion_stream` + `provider` SSE parser; `src/provider/anthropic.rs` — `stream_response` (Anthropic `stream:true`), `openai_compat.rs` — `stream:true`.
    - `../vynkor-plugins/plugins/network/src/handler.rs` — support for `streaming http_request` (chunked response → `ActionResponseChunk`); if already exists — zero work.
    - Client `vynkor-client-android/rust` — `ChatChunk` handler (1h).

  - **Acceptance:** `ActionRequest{action:"chat_completion", streaming:true, params:{stream:true}}` → client receives `ActionResponseChunk` per token (or `ai.delta` events) within <100ms of first token; `SessionClose{reason:"cancel"}` → `ai` aborts upstream fetch, `ai.done` does not arrive; `cargo test` — `test_chat_streaming_chunks_delivered`; `clippy -D warnings`.
  - **Do not:** buffer whole response in `ai` (stream), do not hold `pending_actions` longer than `session_idle_timeout_secs` (already exists).

## Implementation Plan

### MVP — reuse `ActionResponseChunk` (no wire bump, 6h)

1. `ai/handler.rs` — branch `if params.stream { handle_stream } else { handle_once }`. `handle_stream`: `client.send_action_streaming("http_request", http_req{stream:true})` → loop `recv chunk` → `client.send(provider, ActionResponseChunk{action_id: original, seq, chunk: delta})`.
2. Provider: `openai_compat::build_stream_request` (add `"stream":true`), `parse_stream_chunk(SSE line) -> Option<delta>`.
3. Client: `handle ActionResponseChunk` → append to typewriter.
4. Cancel: client `send SessionClose` → kernel forwards `SessionClose` → `ai` catches `envelope::Payload::SessionClose` → abort `reqwest` Stream (drop).

### Full — `CHAT_DELTA` wire (if type-safe delta needed)

1. `vynkor-wire/proto` — `message ChatDelta/ChatCancel`, bump 1.7→1.8 in one commit (like D-01).
2. `vynkor/src/ipc/protocol.rs` — `ChatDelta` branch like `ActionResponseChunk` (verify `provider_id`, `touch_pending_action`).
3. `ai` — send `ChatDelta` instead of `ActionResponseChunk`.

## Open Question 2 — Which to Choose?

- **A: `ActionResponseChunk`** — zero wire changes, kernel already handles it, client already parses chunks (R6-02). Minus — no semantics "this is delta vs final".
- **B: `CHAT_DELTA` + event bus** — semantics, but bump + 6 proto copies.

Recommendation: **A** for MVP (8h, one `ai` repo), **B** — when `ai.delta` as event is needed for other subscribers.

## Anticipate (verified in code)

- **Buffering in `ai`:** current `handler.rs:handle_chat_completion` does `await send_action("http_request")` at once → `parse_response` → `to_vec`. For streaming do not buffer — immediate `ActionResponseChunk` per token, otherwise first-token p50 goes from <100ms to 3s.
- **`network` not streaming:** `grep streaming plugins/network/src` — if `http_request` does not accept `streaming:true`, +4h on `network` plugin. Verified: `ai` currently does not send `stream:true`.
- **Cancel:** `SessionClose{reason:"cancel"}` already exists, but `ai` must forward `AbortSignal` to `reqwest` Stream (drop). Otherwise "stop" does not save tokens.
- **Proto bump:** `CHAT_DELTA` needs `reserved` in `Envelope` (check `reserved 4,50,51,52` — pick 60+ near `AudioStreamChunk`).

## Complexity / Value / Time

| | Estimate |
|---|---|
| **Complexity** | Medium (M) — SSE parser + `streaming http_request` in `network`, but kernel zero |
| **Value** | **Very high** — most visible UX win (first token <1s vs 5–10s) |
| **Time** | **8–12h** (2h provider SSE, 3h ai stream handler, 2h network streaming, 2h test) |
| **Risk** | Medium — `network` streaming may already exist (check), otherwise +4h |
| **Dependencies** | `network` plugin (`http_request` streaming), `ai` plugin; `vynkor` — zero if A |
| **Depends on** | None; parallel with CD-01 |

---

## Pre-Start Check

- [ ] Check `network` `http_request` — does it accept `streaming:true` and emit chunks (`grep streaming` in `plugins/network/src`).
- [ ] Check `provider` — does it send `stream:true` (Anthropic/OpenAI both support).
