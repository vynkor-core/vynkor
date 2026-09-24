# CD-09 — Per-Caller Quota on `chat_completion`

*Track C — `vynkor-plugins/ai` · P2 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §9*

> **Decision (2026-09-24):** the quota is enforced **inside the `ai` plugin**,
> keyed by `caller_plugin_id` (kernel-stamped, unspoofable — for a remote
> device this is its `device_id`). The kernel does **not** learn action names
> like `chat_completion` — that is the same anti-pattern F5 removed from
> `src/auth/permissions.rs` (dumb core). Work moves to `vynkor-plugins`.
>
> **Status:** kernel — no change. Open in `vynkor-plugins`.

## Goal

A friend's phone must not burn the host's model tokens.

## Already Exists

- Kernel (generic, action-agnostic): `action_caller_rate_limit_rps` and
  `action_caller_max_concurrent` — per-`(caller, provider)` limits in
  `src/ipc/protocol/router.rs` (limiter built ~L132, enforced ~L732-757),
  answering `ACTION_QUOTA_EXCEEDED`. These already cap how hard any one
  device can hit the `ai` provider overall.
- Router stamps `caller_plugin_id` on every forwarded `ActionRequest`
  (~L788-791).
- `ai` plugin — `usage_stats` action + DB already track usage.

## Required (vynkor-plugins/ai)

- [ ] Per-caller quota in `ai` for `chat_completion` (rate and/or token budget
      per window), keyed by `caller_plugin_id`; config via plugin env/config;
      unset = unlimited; host-local callers exemptable.
- [ ] Reply `ACTION_QUOTA_EXCEEDED` with a clear error when exceeded.
- [ ] Tests: two callers, one over quota, the other unaffected.

**Do not:** add action-name checks or per-action limiters to the kernel.

| | Estimate |
|---|---|
| **Complexity** | S |
| **Time** | 3–5h in `vynkor-plugins`; kernel 0h |
| **Depends on** | None |
