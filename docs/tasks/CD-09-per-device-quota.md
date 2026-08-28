# CD-09 — Quota / Rate Limit on `ai.chat` per Device

*Track A — `vynkor`-only · P2 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §9*

## Goal

A friend's phone must not burn the host's tokens. Need a per-device quota specifically on `ai.chat` (and broadly — any action of provider `ai`).

---

## Already Exists

- `src/utils/config.rs` — `api_rate_limit_rps/burst` (per JWT `sub`, `VerifiedSub`, governor + `retain_recent` 60s), `ipc_rate_limit_rps` (per `conn_id`), `action_caller_rate_limit_rps` + `action_caller_max_concurrent` (per `(caller, provider)` tuple in `src/ipc/protocol.rs:179`).
- `src/ipc/protocol.rs` — `action_limiter: DefaultKeyedRateLimiter<(String,String)>` + `count_pending_actions_for(caller, provider)`; `ActionStatus::ACTION_QUOTA_EXCEEDED` already exists (R6-03).
- `src/plugins/registry.rs` — `PluginEntry.device_id` already stores owner; `get_device()` available.
- `src/api/server.rs` — `TokenRateLimiter` + `spawn_rate_limiter_prune` (M-01 bound).

## Required

- [ ] **CD-09 — per-device quota on `ai.chat` (vynkor-only, 4–8h):**
  - New config `ai_chat_per_device_rps: Option<u32>` + `ai_chat_per_device_burst: Option<u32>` + `ai_chat_per_device_max_concurrent: Option<u32>` (None = unlimited, like others).
  - In `MessageRouter::run_with_context` — second limiter `ai_chat_limiter: DefaultKeyedRateLimiter<String>` (key = `device_id` from `registry.get_by_conn_id(conn_id).device_id`).
  - In `ActionRequest` branch: if `req.action == "chat_completion" || req.action == "ai.chat"` (confirm name in `ai` plugin) → `ai_chat_limiter.check_key(&device_id).is_err()` → `ACTION_QUOTA_EXCEEDED` with `error:"per-device ai.chat quota exceeded"` (do not touch `(caller,provider)` limiter).
  - Same for `max_concurrent` — `count_pending_ai_chat_for(device_id)` (new helper in `registry.rs`, like `count_pending_actions_for` but by `device_id`).
  - Metrics: `counter!("action_quota_denied_total", "reason"=>"per_device_ai_chat_rate"/"per_device_ai_chat_concurrency")`.

  - **Files:** `src/utils/config.rs` (new fields + `Default`), `src/ipc/protocol.rs` (new limiter + `ActionRequest` branch), `src/plugins/registry.rs` (`count_pending_actions_for_device(device_id, provider)`), `src/kernel/orchestrator.rs` (pass config).
  - **Acceptance:** `ai.chat` from one `device_id` at 10 rps → 11th in same second `QUOTA_EXCEEDED`; other `device_id` unaffected; test `test_per_device_ai_chat_rate_limited_per_device` green; `cargo clippy -D warnings` green.
  - **Do not:** rate-limit all actions per device (only `ai.chat`), do not persist state to disk.

## Implementation Plan

1. `config.rs` — 3 fields + `default_*` (None), no clamp needed (unsigned).
2. `registry.rs` — `count_pending_for_device(device_id, provider) -> u32` (scan `pending_actions`, like existing `count_pending_actions_for`).
3. `protocol.rs` — add `ai_chat_limiter` in `run_with_context` (optional), `retain_recent()` in `prune_tick`; per-device check in `ActionRequest` branch after `(caller,provider)` check — only for `ai.chat`.
4. Test: `tests/unit/test_router.rs` — two `conn_id` with same `device_id` → first passes, second in burst → `QUOTA_EXCEEDED`; second `device_id` → passes.

## Anticipate (verified in code)

- **Key is `device_id`:** existing `action_limiter` keyed `(caller_plugin_id, provider_id)` — for `ai`, caller is always `ai`. Per-device needs `device_id` from `registry.get_by_conn_id(conn_id).device_id` (verified: `PluginEntry.device_id` exists, but `""` → `"local"` — do not limit `local`).
- **Helper scan:** `count_pending_for_device(device_id, provider)` like `count_pending_actions_for` but by `device_id` — scan `pending_actions` (already 3 scans, fourth is OK).
- **Optional:** `ai_chat_per_device_rps=None` → unlimited, like other `action_caller_*` — does not break existing limits.
- **Do not broaden:** limit only `ai.chat`/`chat_completion`, otherwise break `geo`/`battery`.

## Complexity / Value / Time

| | Estimate |
|---|---|
| **Complexity** | Low (S) — copy existing `(caller,provider)` limiter, new key `device_id` |
| **Value** | Medium — protects host from APK-friend spamming `ai.chat` |
| **Time** | **4–8h** (2h limiter, 2h test, 1h config/docs) |
| **Risk** | Low — optional, `None` = off, does not break existing limits |
| **Dependencies** | None — `vynkor`-only; parallel with CD-05/CD-07 |

---

## Note

If product decides to limit all `device→host` actions per device, not just `ai.chat` — broaden key to `device_id` instead of `caller_plugin_id` in existing `action_limiter`. But start narrow (`ai.chat`) — safer.
