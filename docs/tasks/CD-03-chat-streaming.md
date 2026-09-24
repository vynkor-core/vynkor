# CD-03 — AI Response Streaming (`chat_completion`)

*Track C — `vynkor-plugins` (ai + network) · P0 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §3*

> **Decision (2026-09-24):** Option A only — reuse the existing generic
> streaming (`ActionRequest{streaming:true}` → `ActionResponseChunk` →
> `SessionClose`). Option B (`ChatDelta`/`ChatCancel` frames) is **dropped**:
> it would re-add an AI-specific wire surface, contradicting F4, and
> `ai_stream_chunk` is already `reserved` in the proto from the last time.
> Kernel + SDK plumbing already exists. All work is in `vynkor-plugins`.
>
> **Status:** kernel — no change. Open in `vynkor-plugins`.

## Goal

AI response arrives as a token stream, with cancel. First token < 1s instead of
the whole answer after 5–10s.

## Already Exists

- Wire — generic streaming (R6-02/R6-04): `ActionRequest{streaming:true}`,
  `ActionRequestChunk`/`ActionResponseChunk` (seq, chunk, final), `SessionClose`
  (peer cancel), `ActionStreamAbort` (kernel: backpressure/disconnect/idle).
- Kernel — `src/plugins/registry.rs` (`PendingAction.session_accepted`,
  `sweep_idle_sessions`, `sweep_expired_actions`); `src/ipc/protocol/router.rs`
  forwards chunks / `SessionClose`; `session_idle_timeout_secs`.
- Rust SDK — `vynkor-sdk/src/client.rs:706` `send_action_streaming`.
- `vynkor-plugins/plugins/ai/src/handler.rs:handle_chat_completion` — one
  buffered `send_action("http_request", ...)` → parse → reply. **Not streaming.**

## Required (vynkor-plugins)

- [ ] **ai providers:** `anthropic.rs` / `openai_compat.rs` send `stream:true`;
      SSE parser (`parse_stream_chunk(line) -> Option<delta>`).
- [ ] **ai handler:** when the request is `streaming:true`, accept the session,
      emit one `ActionResponseChunk` per delta, final chunk at end. No buffering.
- [ ] **cancel:** on `SessionClose` from the caller, drop the upstream stream so
      "stop" stops spending tokens.
- [ ] **network plugin:** streaming `http_request` (chunked/SSE body →
      `ActionResponseChunk`s) if it does not exist yet.
- Client (`vynkor-client-android`): append chunks to the chat view.

**Acceptance:** `ActionRequest{action:"chat_completion", streaming:true}` →
client receives `ActionResponseChunk`s token-by-token; `SessionClose` aborts the
upstream request; non-streaming path unchanged; tests in `ai` + `network`.

**Do not:** add wire messages; buffer the whole response in `ai`.

| | Estimate |
|---|---|
| **Complexity** | M — SSE parser + network streaming |
| **Value** | Very high — most visible UX win |
| **Time** | 8–12h, all in `vynkor-plugins`; kernel 0h |
| **Depends on** | None |
