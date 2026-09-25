# CD-04 — Assistant Session Contract

*Track C — `vynkor-plugins` (agent + stt + tts) · P1 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §4*

> **Decision (2026-09-24):** pipeline owner = the existing **`agent`** plugin
> (tool-calling engine — it is the conversational/assistant layer; `ai` stays
> plain request/response). **No new `assistant` plugin.** Downlink to the
> device goes through actions / streaming responses to the device's
> `{device_id}.*` actions — not device-targeted events — so the kernel needs
> **no change** (no `device_id` event filter, no proto messages).
>
> **Status:** kernel — no change. Open in `vynkor-plugins` (+ client).

## Goal

Hands-free: wake-word on the phone opens a host session; "turn off the light"
runs as a capability call, not a chat. Budget: < 300ms wake → stream on LAN.

## Already Exists (verified)

- Wire — `AudioStreamChunk{stream_id, codec, sample_rate, channels, data,
  end_of_stream}` (`PERMISSION_AUDIO_STREAM`), generic streaming sessions
  (`ActionRequest{streaming:true}`, chunks, `SessionClose`).
- Plugin events are namespaced by the kernel as `plugin.<id>.<type>`.
- **stt** (`vynkor-plugins/plugins/stt`): actions `stt_listen_start`,
  `stt_listen_stop`, `stt_transcribe`, `stt_models`; consumes
  `AudioStreamChunk`; emits a final transcript plus VAD events
  `plugin.stt.stt_speech_started` / `plugin.stt.stt_speech_ended`
  (≈ `turn_end`). **No interim/partial transcripts.**
- **tts**: `tts_synthesize`, `tts_voices`, `tts_speak`, `tts_speak_stream`.
- **agent**: goal/tool-calling engine (`goal_start`, `tools_list`, memory …).
- Wake-word detection stays on the phone.

## Pre-Start Checks — answered

- *stt/tts wire format?* stt ingests `AudioStreamChunk` and reports via events
  (`plugin.stt.*`) + action responses; tts exposes `tts_speak_stream` for
  streamed audio out.
- *Register latency?* Not measured yet — measure before tuning.

## Required (vynkor-plugins)

- [ ] **stt:** interim/partial transcripts (`plugin.stt.stt_partial` or
      streaming `stt_transcribe` chunks) — the only real gap in stt.
- [ ] **agent:** assistant-session action (e.g. `assistant_open`, streaming):
      mic `AudioStreamChunk` → `stt` → on `stt_speech_ended` route intent →
      capability call via `action_specs` **or** `chat_completion` →
      `tts_speak_stream` → audio back to the device's `{device_id}.*` speaker
      action. Barge-in: new speech stops current TTS (session close / stop action).
- [ ] Client: KWS + mic stream, speaker action, barge-in handling.

**Acceptance:** wake → session open → partial transcript < 300ms (LAN p50);
"light off" executes a capability without `chat_completion`; barge-in mutes
TTS on the phone; mock stt/tts contract test in `agent`.

**Do not:** put KWS, audio storage or intent routing in the kernel.

| | Estimate |
|---|---|
| **Complexity** | L — stt partials + agent pipeline |
| **Value** | High — screen-less command scenario |
| **Time** | 12–20h in `vynkor-plugins`; kernel 0h |
| **Depends on** | CD-03 desirable (streaming chat), not required |
