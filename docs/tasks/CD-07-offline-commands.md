# CD-07 — Fate of Commands to Offline Devices

*Track A — `vynkor` only · P2 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §7*

> **Decision (2026-09-24):** the original bug is effectively gone — when a
> device's plugin unregisters, its actions leave the action index and the
> router answers `ACTION_NOT_FOUND` immediately (no 30s hang). No queue, no
> new `ActionStatus`, no wire change. Rewritten scope:
> 1. distinguish **"device offline"** in the error message (still
>    `ACTION_NOT_FOUND`) when the target action belongs to a known-but-offline
>    device;
> 2. **fail in-flight actions immediately** when their provider disconnects,
>    instead of letting them run to `action_timeout_ms`.
>
> **Status:** DONE 2026-09-25 (kernel, 7e0a4a1).

## Already Exists

- `src/plugins/registry.rs` — `DeviceState::{Online, Offline, Revoked}`,
  `devices` map, `last_seen`; `unregister` flips the device to `Offline` and
  drops its actions from the index.
- `src/ipc/protocol/router.rs` — action lookup → `ACTION_NOT_FOUND` when no
  provider; `pending_actions` + `sweep_expired_actions`; `ActionStreamAbort`
  on disconnect for streaming sessions.

## Required

- [ ] Not-found path: if the requested action is `{device_id}.*` and that
      device is known and `Offline`, error text says `device '<id>' is offline`.
- [ ] Provider disconnect: every pending (non-streaming) action whose provider
      was that connection gets an immediate error `ActionResponse` to the
      caller and is removed from `pending_actions`.
- [ ] Tests: offline device → immediate `ACTION_NOT_FOUND` with "offline";
      provider drops mid-request → caller gets an error well before
      `action_timeout_ms`.

**Do not:** queue commands for offline devices; auto-retry (client decides).

| | Estimate |
|---|---|
| **Complexity** | S |
| **Time** | 3–5h |
| **Depends on** | None |
