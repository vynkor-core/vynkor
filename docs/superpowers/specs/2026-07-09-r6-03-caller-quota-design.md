# R6-03 — Per-caller action quota at the kernel level

**Date:** 2026-07-09
**Status:** approved, pre-implementation
**Roadmap item:** `ROADMAP.md` R6-03

## Problem

`max_procs`/`max_vmem_mb` (R5-10) bound one plugin's own process resource use, but nothing
bounds how many concurrent/frequent actions one *calling* plugin can push through a shared
provider (e.g. `network` acting as the standard network path for all plugins). A single
misbehaving or heavy caller can starve every other caller of the same provider.

R6-03's original open question — does `ActionRequest` routing carry a caller/requester id
today? — is resolved: yes. `PendingAction.requester_id` (`src/plugins/registry.rs:28`) is
already populated from `sender_id` at the `ActionRequest` routing site
(`src/ipc/protocol.rs:544`). No proto change needed to identify the caller. This design covers
the quota enforcement itself, which is unbuilt.

## Architecture

Enforcement lives in `src/ipc/protocol.rs`, inside the `ActionLookup::Found(provider)` arm of
`ActionRequest` handling — after the existing T-19 permission check
(`required_permission_for_action` / `check_permission`), before `register_pending_action`.

Two independent gates, both keyed by **`(requester_id, provider.plugin_id)`** — not per-caller
globally. Rationale: the roadmap's motivating scenario is one caller starving others *via a
specific shared provider*; keying per-provider means hammering `network` doesn't burn a
caller's budget against an unrelated provider it also legitimately calls.

1. **Concurrency cap** — max simultaneous pending actions for a `(caller, provider)` pair.
   Directly targets "one caller holds N provider slots open, starving everyone else waiting on
   that provider's response."
2. **Rate limit** — max action-requests/sec for a `(caller, provider)` pair. Bounds burst rate
   independent of how fast the provider responds.

Both gates are **off by default** (`None` = unlimited), matching the existing
`ipc_rate_limit_rps` / `api_rate_limit_rps` config convention — no behavior change for
deployments that don't opt in.

Order of checks: concurrency cap first (cheaper — a DashMap scan, no token-bucket state touch),
then rate limit.

## Components

- **`src/utils/config.rs`** — two new `Option<u32>` fields on `Config`:
  - `action_caller_rate_limit_rps` — analogous to `ipc_rate_limit_rps`.
  - `action_caller_max_concurrent`.
  Both `#[serde(default)]` → `None`.

- **`src/plugins/registry.rs`** — new method:
  ```rust
  pub fn count_pending_actions_for(&self, requester_id: &str, provider_id: &str) -> u32
  ```
  Scans `pending_actions` (a `DashMap`), counts entries where `requester_id` and `provider_id`
  both match. No new struct fields, no counter to keep in sync. `n` is bounded by total
  kernel-wide in-flight actions (itself bounded by `action_timeout_ms` sweeping expired
  entries), not by request volume — a scan is the correct trade-off here since it can't desync
  the way a separately-maintained increment/decrement counter could (three existing removal
  sites — `take_pending_action`, `take_pending_action_if_provider`,
  `sweep_expired_actions` — would each need to remember to decrement).

- **`src/ipc/protocol.rs`** — `run_with_context` gains two new params,
  `action_caller_rate_limit_rps: Option<u32>` and `action_caller_max_concurrent: Option<u32>`,
  threaded the same way `ipc_rate_limit_rps` is today. Builds
  `action_limiter: Option<Arc<DefaultKeyedRateLimiter<(String, String)>>>` once, keyed by
  `(requester_id, provider_id)`, pruned on the existing 60s `prune_tick` alongside the current
  `ipc_limiter.retain_recent()`.

- **`wire/proto/veyron_protocol.proto`** — additive new variant on `ActionStatus`:
  `ACTION_QUOTA_EXCEEDED = 5` (next value after `ACTION_NOT_FOUND = 4`). Not a renumber (unlike
  the T-16 footgun), purely additive — safe under the existing `reserved` discipline. Mirrored
  to `sdk/cpp/proto/veyron_protocol.proto`, `sdk/python/proto/veyron_protocol.proto` in the same
  change (T-17's CI drift check enforces this).

- **`src/kernel/orchestrator.rs`** — reads the two new config fields, passes through to
  `run_with_context`'s new params.

## Data flow

On `ActionRequest`, in the `ActionLookup::Found(provider)` arm, after the permission check
passes:

1. `registry.count_pending_actions_for(&sender_id, &provider.plugin_id)` — if `>=` configured
   cap, deny.
2. Else if a rate limiter is configured, `action_limiter.check_key(&(sender_id.clone(),
   provider.plugin_id.clone()))` — if it errors (bucket empty), deny.
3. Either denial: send `ActionResponse { status: ActionQuotaExceeded, ... }` directly to the
   requester (same shape as the existing early-return `ActionNotFound`/`ActionPermissionDeny`
   paths just above this arm). The action is **never forwarded** to the provider and **no**
   `pending_actions` entry is created for it.
4. Neither denial: existing behavior unchanged (register pending action, forward to provider).

## Error handling

No new failure modes. Both checks are pure reads — DashMap scan can't error; governor's
`check_key` is lock-free and only ever reports allowed/denied, never a hard error. New metrics
mirroring the existing `ipc_send_denied_total` pattern:

```
counter!("action_quota_denied_total", "reason" => "concurrency").increment(1);
counter!("action_quota_denied_total", "reason" => "rate").increment(1);
```

## Testing

New tests alongside the existing T-19 test in `tests/integration/test_kernel_commands.rs` (or a
new `tests/unit/test_action_quota.rs` if that file gets crowded):

- Concurrency: caller with `action_caller_max_concurrent = 2` gets a 3rd concurrent
  `ActionRequest` to the *same* provider denied with `ActionQuotaExceeded`; a concurrent request
  to a *different* provider still succeeds (proves per-`(caller, provider)` keying, not global).
- Concurrency releases: once one of the two in-flight actions completes (provider responds) or
  times out, the next request from that caller to that provider succeeds immediately — proves
  the scan-based count self-corrects with no explicit decrement.
- Rate limit: burst above configured rps from the same `(caller, provider)` denied; a different
  provider from the same caller is unaffected.
- Unset config (`None` for both): existing action-routing tests unaffected — no behavior change
  for deployments that don't opt in.

## Out of scope

- Per-plugin/per-provider override of the quota values (config.yaml stays global-only, matching
  `ipc_rate_limit_rps`'s existing non-overridable pattern). Can be revisited later if a real
  need for differentiated caller quotas emerges.
- Any change to `max_procs`/`max_vmem_mb` (R5-10) — those remain process-level resource limits,
  orthogonal to this action-routing-level quota.
