# CD-04 — Assistant Session Contract

*Track C — `vynkor` + `vynkor-wire` + `vynkor-plugins` (ai+stt+tts) · P1 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §4*

## Goal

Hands-free scenario: wake-word on phone (KWS low-power, mic) triggers a host session. Need a contract around it so "turn off the light" executes as `capability-call`, not chat.

**Budget:** `<300ms` from wake to stream. Today session start is a full permission handshake.

---

## Already Exists

- `../vynkor-wire/proto` — `AudioStreamChunk{stream_id, codec, sample_rate, channels, data, end_of_stream}` (`FLAG_RAW_BINARY` already, `PERMISSION_AUDIO_STREAM`), `Event`/`EventPublish` (bus already `try_send`).
- `src/ipc/protocol.rs` — `pending_actions` sessions (R6-02/04), `SessionClose`/`ActionStreamAbort`, `idle_timeout`.
- `../vynkor-plugins/plugins/{stt,speech,tts,ai}` — separate plugins (STT → agent/router → TTS today via `ai` + `network`).
- Wake-word detection — stays on phone (kernel does nothing).

## Required

- [ ] **CD-04 — assistant-session contract (wire+kernel+plugins, 12–20h):**
  - **Fast mic-session reopen:** `WS` `register` already gives `session_nonce` → `derive_session_key`; `register` must not repeat full permission handshake on wake. Solution: `SessionResume{session_id, nonce}` (or reuse `ActionRequest{action:"assistant.open", streaming:true}`) — kernel checks `device_id` already in `active_secret` (E-01), without `check_permission` on each mic chunk (only on `assistant.open`).
  - **Events to phone (3 types, additive in `proto`):**
    - `partial_transcript {text, is_final: bool, seq}` — interim STT.
    - `turn_end {reason: "silence"|"endpoint"}` — server decided phrase ended.
    - `tts_interrupt {reason: "barge-in"}` — new utterance mutes current TTS on phone (client must stop `AudioTrack`).
  - **Unified host pipeline (without mandatory chat):**
    ```
    mic stream (AudioStreamChunk) → stt (partial_transcript) → router (capability vs chat) →
      capability-call (action_specs) OR ai.chat → TTS (AudioStreamChunk) → device.speaker
    ```
    Pipeline owner — separate `assistant` plugin (recommended) or `ai` plugin (open question 3). Kernel — just routes, no logic (dumb core, `DUMB_CORE_AUDIT.md` F2).

  - **Files:**
    - `../vynkor-wire/proto/vynkor_protocol.proto` — `message AssistantSessionOpen{string session_id; string device_id; repeated string caps}` + `PartialTranscript`, `TurnEnd`, `TtsInterrupt` (or as `Event` types `assistant.partial_transcript` — then no wire change, just convention).
    - `src/events/bus.rs` — deliver `assistant.*` events to `device_id` (filter by `device_id`, like CD-05).
    - `src/ipc/protocol.rs` — `assistant.open` as plain `ActionRequest{streaming:true}` (zero code, just `PERMISSION_AUDIO_STREAM`).
    - `../vynkor-plugins/plugins/assistant` (new) **or** `../vynkor-plugins/plugins/ai/src/handler.rs` — `handle_assistant_open`: loop `AudioStreamChunk` → `stt` action → `partial_transcript` events → `turn_end` → `router` → `tts` → `AudioStreamChunk` to `device.speaker`.
    - `../vynkor-plugins/plugins/{stt,tts}` — already exist, just contract.

  - **Acceptance:** wake → `assistant.open` → `partial_transcript` <300ms (p50 on LAN); `turn_end` → `capability_used` (light off) without `ai.chat`; `tts_interrupt` mutes `AudioTrack` on phone; test `test_assistant_session_contract` (mock stt/tts); `clippy -D warnings`.
  - **Do not:** embed KWS in kernel (on phone), do not store audio in kernel (stream).

## Implementation Plan

### Phase 1 — events (no pipeline, 4h)

1. `vynkor-wire/proto` — `Event` types `assistant.partial_transcript`/`assistant.turn_end`/`assistant.tts_interrupt` as convention (no proto bump — docs only), or separate `message` if type safety needed → bump 1.7→1.8.
2. `src/events/bus.rs` — `publish` for `assistant.*` (filter `device_id` — already `get_device`).

### Phase 2 — pipeline (8–16h)

3. Create `../vynkor-plugins/plugins/assistant` (or extend `ai`) — `manifest.actions = ["assistant.open","assistant.close"]`, `permissions = [AUDIO_STREAM, EVENT_PUBLISH]`.
4. `assistant/src/main.rs` — `ActionRequest{assistant.open, streaming:true}` → loop: `AudioStreamChunk` (mic) → `client.send_action("stt.transcribe", chunk)` → publish `partial_transcript` → on `turn_end` → `find_action_provider(router)` → `client.send_action(cap)` or `ai.chat` → `tts.speak` → `AudioStreamChunk` to `{device_id}.speaker`.

## Open Question 3 — Owner?

- **Separate `assistant` plugin** — clean, kernel dumb, `ai` not bloated.
- **`ai` plugin** — fewer repos, but `ai` already 2.5k LOC.

Recommendation: **new `assistant` plugin** (like `stt`/`tts`), `ai` stays `chat_completion`.

## Anticipate (verified in code)

- **KWS on phone:** kernel does nothing — wake-word low-power `AudioRecord` in `app/`, kernel only `assistant.open` (verified: `AudioStreamChunk` + `FLAG_RAW_BINARY` already exists).
- **<300ms budget:** `register` already <100ms on LAN (`WsGateway` + `derive_session_key`), otherwise tune `ws_handshake_timeout`. On WAN need `opus` (already `AudioCodec::OPUS`) + `ws` without head-of-line (QUIC later).
- **New plugin vs `ai`:** `ai` already 2.5k LOC — separate `assistant` like `stt`/`tts` is cleaner, kernel dumb (`DUMB_CORE_AUDIT.md` F2). Check `stt`/`tts` wire format: `AudioStreamChunk` vs `ActionRequest`.
- **Events without bump:** `Event` types `assistant.partial_transcript` can be convention `event_type` without proto bump (docs only) — bump only if type safety needed.

## Complexity / Value / Time

| | Estimate |
|---|---|
| **Complexity** | High (L) — new plugin + 3 event types + STT/TTS integration |
| **Value** | High — "command without screen" scenario (why agent exists) |
| **Time** | **12–20h** (4h events+wire, 8–16h pipeline) |
| **Risk** | Medium — `<300ms` realistic on LAN, on WAN needs `opus` + `ws` tuning |
| **Dependencies** | `stt`, `tts`, `ai` plugins; `wire` only for events; `vynkor` — bus only |
| **Depends on** | CD-03 (streaming) desirable but not required; CD-05 (audit) — nearby |
| **When** | Before client wake-word wave (P1) |

---

## Pre-Start Check

- [ ] Check `stt`/`tts` — which wire format they already use (`AudioStreamChunk` vs `ActionRequest`).
- [ ] Measure current `register` latency (should be <100ms on LAN, otherwise optimize `WsGateway`).
