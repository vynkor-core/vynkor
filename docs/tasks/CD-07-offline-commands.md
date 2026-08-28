# CD-07 — Fate of Commands to Offline Devices

*Track A — `vynkor`-only · P2 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §7*

## Goal

`host→device` requests must not hang in "timeout silence" for 30s when the device is offline. Need honest semantics: immediate error or queue with TTL.

---

## Already Exists

- `src/plugins/registry.rs` — `DeviceState::{Online,Offline,Revoked}` + `devices` map, `last_seen`, `unregister` flips to `Offline` when last plugin leaves.
- `src/ipc/protocol.rs` — `find_action_provider` → `ActionNotFound` / `ActionTimeout` (`action_timeout_ms 30s` default), `pending_actions` + `sweep_expired_actions` (30s), `ActionStreamAbort`/`SessionClose`.
- `src/kernel/commands.rs` — `list_devices` already returns `state`.

## Required

- [ ] **CD-07 — offline semantics (vynkor-only, 3–6h):**
  - Decide policy: **Option A — fail-fast** (`ACTION_DEVICE_OFFLINE` immediately) or **Option B — queue with TTL** (e.g. 30s, then `ACTION_TIMEOUT` with `reason:"device offline"`). Recommendation: start with A (simple, predictable); queue only if a real use case appears.
  - New `ActionStatus::ACTION_DEVICE_OFFLINE = 8` (or `ErrorCode::ERR_DEVICE_OFFLINE`) — additive in `vynkor-wire` (but can avoid bump by reusing `ACTION_NOT_FOUND` with `error:"device offline"` — then pure Track A).
  - In `forward()` before `find_action_provider`: if `provider.device_id != "local"` and `registry.get_device(provider.device_id).state == Offline` → immediate `ActionResponse{status: DEVICE_OFFLINE}` without creating `pending_actions`.

  - **Files:** `src/ipc/protocol.rs` (`ActionRequest` branch), optional `../vynkor-wire/proto/vynkor_protocol.proto` (new `ActionStatus`), `src/plugins/registry.rs` (helper `is_device_online`), `src/utils/config.rs` (optional `offline_queue_ttl_secs`).
  - **Acceptance:** device offline — `ActionRequest` to `{id}.geo` returns `DEVICE_OFFLINE` in <100ms (not 30s); test `test_offline_device_fails_fast` green; `cargo test --all` green.
  - **Do not:** store queue longer than TTL (memory), do not auto-retry (client decides).

## Implementation Plan

1. Decide: fail-fast (no queue) — single check + new status or string in `error`.
2. Add `device_of(plugin_id)` → `device_id` lookup (already `entry.device_id`).
3. In `forward`/`handle_kernel_message` — early return before `register_pending_action`.
4. Test: register `device-phone` plugin, `unregister` (→ Offline), `send ActionRequest` → expect `DEVICE_OFFLINE`.
5. If wire bump needed — one `reserved` → new constant, bump `PROTOCOL_VERSION` 1.7→1.8 in one commit (like D-01).

## Anticipate (verified in code)

- **Fail-fast vs queue:** queue `DashMap<device_id, Vec<Queued>>` + `sweep_offline_queue` → memory/OOM risk, retry is client responsibility. Start fail-fast. Verified: `find_action_provider` already returns `NotFound` when device offline (empty `by_plugin_id`).
- **New status:** `ACTION_DEVICE_OFFLINE=8` needs wire bump 1.7→1.8 + sync 6 copies. Can avoid bump — reuse `ACTION_NOT_FOUND` with `error:"device offline"` (then pure Track A).
- **Early return:** check before `register_pending_action` — otherwise `pending_actions` grows for nothing.

## Complexity / Value / Time

| | Estimate |
|---|---|
| **Complexity** | Low (S) — 10 lines in `forward`, no migrations |
| **Value** | Medium — removes "30s silence" UX bug, honest status in UI |
| **Time** | **3–6h** (2h logic+test, 1–2h proto bump if needed) |
| **Risk** | Low — does not change hot path, only early return |
| **Dependencies** | None — shippable with CD-05/CD-09 |

---

## Alternative (queue with TTL)

If product needs "send while offline, deliver when back" — add `DashMap<device_id, Vec<QueuedAction{envelope, deadline}>>` in `PluginRegistry` + `sweep_offline_queue` in `protocol.rs:prune_tick`. But that is a feature, not P2 hygiene; start fail-fast.
