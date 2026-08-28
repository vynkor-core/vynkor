# CD-05 — Capability Call Audit on Device

*Track A — `vynkor`-only · P1 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §5*

## Goal

Phone shows a screen "who and when requested my location". Without it, sensitive permissions are a black box and scary to grant.

**Event back to phone:** `capability_used { cap, ts, origin }` via the existing device event channel.

---

## Already Exists

- WS event channel to phone already exists (`events/bus.rs` → `registry.get(device_id)` → `write_tx`).
- `src/auth/permissions.rs` — `check_ipc_send`/`check_ipc_target`/`check_permission`; `src/plugins/registry.rs` — `device_id`/`user_id` on `PluginEntry`.
- `src/ipc/protocol.rs:forward()` — decision point for `ActionRequest` → provider; already has `counter!("ipc_send_denied_total")`.

## Required

- [ ] **CD-05 — `capability_used` event (vynkor-only, 4–6h):**
  - On every successful `ActionRequest`/`ActionRequestChunk`/`EventPublish`/`AudioStreamChunk` that passed permission checks, publish `capability_used` (or `device.capability_used`) with `cap` (action/capability name), `ts` (unix millis), `origin` (requester `plugin_id`/`device_id`), `target` (provider), `device_id`.
  - Deliver only to devices (filter by owner `device_id`), not broadcast.
  - No dedup in kernel — client dedups if needed; kernel just dumb-publishes.

  - **Files:** `src/ipc/protocol.rs` (in `forward`/`handle_kernel_message` after `find_action_provider` OK), `src/events/bus.rs` (`publish` + `Event{event_type:"capability_used"}`), optional `src/plugins/registry.rs` (helper `device_of(plugin_id)`).
  - **Acceptance:** `ai.chat` call from phone → same phone receives `capability_used{cap:"ai.chat", ts:>0, origin:"com.vynkor.android/geo"}` in WS event stream; `cargo test` — new `test_capability_audit_event_emitted`; `clippy -D warnings` green.
  - **Do not:** store history in kernel (client stores locally), do not interpret `cap` (plain string).

## Implementation Plan

1. `src/events/bus.rs` — helper `capability_used_payload(cap, origin, target)` → `serde_json::json!`.
2. `src/ipc/protocol.rs` — after resolving `ActionLookup::Found` and before `send_envelope(provider)` call `event_bus.publish(Event{event_type:"capability_used", payload_json: ...}, registry).await` (fire-and-forget, `try_send` already in `bus::deliver`).
3. Same for `EventPublish` path (`EventPublish` in `handle_kernel_message`) and `AudioStreamChunk` (`FLAG_RAW_BINARY` branch).
4. Test: `tests/unit/test_events.rs` — `subscribe("capability_used")` → `send ActionRequest` → expect event.

## Anticipate (verified in code)

- **Do not spam:** `AudioStreamChunk` (mic 50/sec) must not generate `capability_used` — only `ActionRequest`/`EventPublish` successes. Otherwise phone WS channel drowns, even though kernel `try_send` does not block (PERF-1).
- **Filter by `device_id`:** event is not broadcast; only owner `device_id` (like `get_device`). Verified: `bus.publish` currently broadcasts via `subscriptions`, needs filter.
- **Dumb-core:** kernel only publishes `cap` string, no policy interpretation — OK (`DUMB_CORE_AUDIT.md` F2).

## Complexity / Value / Time

| | Estimate |
|---|---|
| **Complexity** | Low (S) — one publish point, no proto changes, no migrations |
| **Value** | High — unblocks granting sensitive permissions (geo, mic, contacts) |
| **Time** | **4–6h** (1h publish, 2h tests, 1h docs) |
| **Risk** | Low — event bus already `try_send`, does not block router (PERF-1) |
| **Dependencies** | None — `vynkor`-only, shippable right after ticket |

## Dumb-Core Check

- Kernel only publishes string `cap`, does not interpret policy — OK (see `DUMB_CORE_AUDIT.md` F2).
