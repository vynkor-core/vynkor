# Action Routing to Provider Plugins (R5-07, option b)

**Status:** approved 2026-07-02
**Roadmap item:** `ROADMAP.md` R5-07 (AUDIT H-05)

## Problem

`ActionRequest` sent to `target: "kernel"` has no executor. The interim fix (done) reports `ACTION_NOT_FOUND` instead of a fake `ACTION_OK`. This spec covers the real implementation: option (b) from the roadmap — route the action to a provider plugin that declared it in `manifest.actions`, correlating the response back to the original requester by `action_id`.

Option (a) (kernel-executed built-ins) was rejected — the manifesto specifies "dumb core," and built-in business logic in the kernel contradicts that.

## Non-goals

- No proto changes. `ActionRequest`, `ActionResponse`, `PluginManifest.actions` already exist and are sufficient.
- No SDK code changes required. The requester side (`VeyronClient::send_action`) already targets `"kernel"` and matches by `action_id` — it is unaware routing changed. The provider side uses the SDK's existing public `send(target, envelope)` method with `target: "kernel"` — no new SDK API needed, only a documented convention.
- No action-name uniqueness enforcement at registration time. Collisions are only detected (and rejected) at call time.
- No new `PermissionType`. Declaring an action in `manifest.actions` is itself the authorization.

## Design

### Provider lookup

On `ActionRequest` with `target: "kernel"`, the kernel scans `PluginRegistry` for registered plugins whose `manifest.actions` contains the requested action name:

- **0 matches** → `ActionStatus::ActionNotFound` (same as today's interim behavior).
- **>1 matches** → `ActionStatus::ActionNotFound` + `warn!` logging the action name and the colliding plugin ids. This is a deploy misconfiguration; the kernel does not guess which provider to prefer.
- **Exactly 1 match** → route (see below).

`PluginRegistry` gains:

```rust
pub enum ActionLookup {
    NotFound,
    Found(PluginEntry),
    Ambiguous(Vec<String>), // colliding plugin ids, for the warn! log
}

pub fn find_action_provider(&self, action: &str) -> ActionLookup
```

Implementation scans `by_plugin_id` (small N — plugin counts are not expected to be large; no secondary index needed).

### Correlation

The requester's `action_id` is chosen client-side (SDK auto-generates it per-process: `{prefix}-{unix_millis}-{seq}`) and is not guaranteed globally unique across separate plugin processes. Rather than trust it as a cross-process correlation key, the kernel mints its own internal id per hop:

- Counter: `static ACTION_CORRELATION_SEQ: AtomicU64` in `protocol.rs`, format `kact-{seq}`.
- `PluginRegistry` gains a pending-actions table:

```rust
pub struct PendingAction {
    pub requester_write_tx: mpsc::Sender<Outbound>,
    pub original_action_id: String,
    pub requester_id: String,   // for logging
    pub deadline: Instant,
}

pending_actions: DashMap<String, PendingAction>, // keyed by internal kact-N id

pub fn register_pending_action(&self, internal_id: String, pending: PendingAction)
pub fn take_pending_action(&self, internal_id: &str) -> Option<PendingAction>
pub fn sweep_expired_actions(&self, now: Instant) -> Vec<PendingAction>
```

On routing: insert `PendingAction` keyed by the internal id, then build a fresh `ActionRequest` envelope (same `action`/`params_json`/`timeout_ms`, `action_id` replaced with the internal id) and push it to the provider's `write_tx` via the existing `send_envelope` helper (already sets `sender_id`, `message_id`, `timestamp`, framing — no duplication needed).

### Provider convention

A provider plugin implementing a declared action receives the `ActionRequest` exactly like any other kernel-pushed message (existing `recv()` pattern, no SDK change). It must reply with `ActionResponse{action_id: <the id it was given>, ...}` **targeted at `"kernel"`** — not at the original requester, because it doesn't know who that is. This is the one new behavioral contract introduced by this feature; it is orthogonal to the existing peer-to-peer `ActionRequest`/`ActionResponse` pattern (e.g. the `echo` reference plugin), which is unaffected and continues to target a known peer directly.

### Response handling

`handle_kernel_message` gains an `ActionResponse` arm (previously absent — only `ActionRequest` was handled at `target: "kernel"`):

1. Look up `resp.action_id` in `pending_actions`.
2. **No match** (already timed out and swept, or a bogus/duplicate response) → drop silently. Not counted against the sender's protocol-error budget — this can legitimately race the timeout sweep.
3. **Match** → remove the entry, rewrite the envelope with `action_id: original_action_id`, and proxy `status`/`data_json`/`error` through **unchanged** — provider-side failures (`ACTION_ERROR`, `ACTION_PERMISSION_DENY`, etc.) are relayed as-is, not reinterpreted or translated by the kernel. Send via `send_envelope` to `requester_write_tx`.

### Timeout sweep

Reuses the router's existing 60 s `prune_tick` (currently used only to age out rate-limiter state). Each tick, `sweep_expired_actions(Instant::now())` returns all entries past `deadline` (computed at insert time from `timeout_ms`, default 30 s per the proto doc comment on `ActionRequest.timeout_ms`). For each expired entry, send `ActionResponse{action_id: original_action_id, status: ActionTimeout, error: "action timed out"}` to `requester_write_tx` and evict.

This bounds timeout precision to the tick interval (up to ~60 s late in the worst case) rather than firing exactly at `timeout_ms`. Acceptable: this is a coarse liveness backstop, not a latency-sensitive path, and avoids a second timer task.

### Disconnect edge cases

Handled by construction, no special-casing:

- **Provider disconnects mid-flight** → no response ever arrives → requester gets `ACTION_TIMEOUT` at the next sweep.
- **Requester disconnects while pending** → entry lingers until swept; the `send_envelope` to its now-closed `write_tx` silently no-ops (same tolerance pattern already used elsewhere in `protocol.rs` for slow/gone connections).

### Permission model

None beyond "a provider plugin declared the action." The old `action_to_permission()` map (`src/auth/permissions.rs`) — a fixed table of builtin-looking action names like `http_get`, `get_cpu`, `play_audio` mapped to `PermissionType`s — is retired along with its unit test (`tests/unit/test_permissions.rs`). It was only consulted by the now-replaced `ActionRequest` stub in `protocol.rs`; once routing is purely provider-lookup driven, it has no callers and becomes genuinely dead code, consistent with this repo's R5-12 precedent of retiring dead surface rather than leaving unused pub fns in place for a hypothetical option (a).

## Testing

- **Integration:** provider registers `manifest.actions: ["get_weather"]`; requester calls `client.send_action("get_weather", ...)` (existing SDK API, unchanged); provider receives the request, replies targeting `"kernel"`; requester receives the correct data with its original `action_id` intact.
- **Integration:** ambiguous providers (two plugins both declare the same action) → `ACTION_NOT_FOUND`.
- **Integration:** provider-side failure (`ACTION_ERROR`/`ACTION_PERMISSION_DENY`) proxies through to the requester unchanged.
- **Unit:** `PluginRegistry::sweep_expired_actions` / correlation id round-trip, using synthetic `Instant` values rather than real elapsed time — avoids a slow or flaky 60 s+ test.
- **Regression:** existing `kernel_targeted_action_request_returns_not_found_not_fake_ok` (`tests/integration/test_kernel_commands.rs`) stays green unchanged — no provider is registered for `get_cpu` in that test, so it still resolves to `ACTION_NOT_FOUND`.
