# Veyron ROADMAP — Phase 6

**Baseline:** 2026-07-06 · Kernel `0.1.0` · Audit ~78/100 (see `AUDIT.md`)
**Branch:** `develop`
**Previous phases:** `docs/archive/` (Phase 1–2: `ROADMAP_phase1.md`/`ROADMAP_v2.md`/`ROADMAP_v3.md` · Phase 3–4: `ROADMAP_v4.md` · Phase 5: `ROADMAP_v5.md`, all items complete)

---

## Manifesto (non-negotiable)

- Kernel = dumb byte router + process supervisor. Zero business logic. Zero AI. Zero application databases.
- Intra-host IPC = UDS only. No TCP, no Redis, no queues.
- Protocol = single `.proto` file. Changes propagate to all SDKs.
- Plugin = isolated OS process. Cannot bypass kernel. Speaks only UDS.
- External access = WebSocket/HTTP gateway only (Axum).

---

## Current baseline — 2026-07-06

| Metric | Value |
|--------|-------|
| Kernel version | 0.1.0 |
| Audit score | ~78/100 (`AUDIT.md`, 2026-07-02) |
| Tests | `cargo test --all --all-features`: 266 passing, 0 failing |
| Clippy | clean (`--all-targets --all-features -D warnings`) |
| Phase 5 | ✅ all items complete — see `docs/archive/ROADMAP_v5.md` |

---

## Phase 6 — Network Plugin Protocol Support (candidate, not yet scheduled)

Source: `veyron-plugins/plugins/network/KERNEL_PROTOCOL_TODO.md` (gitignored local notes in that repo). All four items require changes here (proto and/or kernel), not in `veyron-plugins`.

### R6-01 — Plugin → event-bus publish path

`EventBus::publish` (`src/events/bus.rs`) is only called from kernel-internal code (`src/ipc/protocol.rs`, `src/kernel/orchestrator.rs`, `src/plugins/supervisor.rs`). No wire message lets a plugin push an event in. Needed for `network` to emit `network.request_completed` (status, host, latency_ms, retry_count) instead of stdout-only logging.

**Needed:** new `EventPublish` envelope variant (or `Event` with a plugin→kernel direction), handled in `src/ipc/connection.rs`/`src/ipc/protocol.rs` next to `Subscribe`/`Unsubscribe`. Gate behind a new permission (e.g. `PERMISSION_EVENT_PUBLISH`) so a plugin can't spoof `system.*` events.

### R6-02 — Streaming action support (chunked request/response)

`ActionRequest`/`ActionResponse` are single envelopes (`bytes params_json`/`data_json`) — no framing for a large body across multiple frames tied to one `action_id`. `send_fragmented` is client-side reassembly for one logical frame, not a multi-message stream. Needed for a real `http_request_stream` action.

**Options:** (a) new `ActionStreamChunk` message (`action_id`, `seq`, `bytes`, `final: bool`) routed by the kernel to the same requester across frames, or (b) let actions open a raw IPC channel (generalize `send_raw_audio`'s `FLAG_RAW_BINARY` path beyond audio) both sides drive manually.

### R6-03 — Per-caller resource/rate limits at the kernel level

`max_procs`/`max_vmem_mb` exist per-plugin in `config.yaml` (R5-10), but nothing limits one *calling* plugin from starving others via actions on a shared provider (e.g. `network` as the standard network path for all plugins).

**Needed:** kernel-enforced per-action-caller quotas, or a documented convention that the provider tracks caller ids from `ActionRequest` itself. **Open question to resolve first:** confirm whether `ActionRequest` carries a caller/requester id field today (proto + `src/ipc/protocol.rs` routing) — R5-07's action routing may already thread `requester_id` through `PendingAction`, worth checking before scoping new proto work.

### R6-04 — WebSocket / long-lived-connection action model

Single `ActionRequest`→`ActionResponse` doesn't fit a persistent WebSocket-style session. Needed for any provider (e.g. `network`) that wants to expose a WS-like connection to callers.

**Options:** dedicated `Event`-based push model (open via action, kernel delivers frames as `Event`s to subscribers) or a new bidirectional-stream primitive. Largest of the four — needs its own design doc in this repo (mirrors `docs/superpowers/specs/2026-07-02-action-routing-design.md`'s process for R5-07) before any `network` work depends on it.

---

## Task Summary

| Phase | Items | Severity | Est. effort |
|-------|-------|----------|--------------|
| 6 Network plugin protocol support | R6-01..04 | Candidate, unscheduled | ~1 decision + 1 design doc + impl TBD |

**Ship gate:** none set yet — R6-03's open question and R6-04's design doc should resolve before effort estimates firm up.

## Definition of Done

- `cargo test --all --all-features` exits 0; new behavior has regression tests.
- `cargo clippy --all-targets --all-features -- -D warnings` clean; `cargo fmt --check` clean.
- Protocol changes: `proto/veyron_protocol.proto` updated with `reserved` discipline, all three SDKs updated in the same change.
- Docs updated in the same PR (`docs/FRAMING.md` for wire changes, README for operator-visible changes).
