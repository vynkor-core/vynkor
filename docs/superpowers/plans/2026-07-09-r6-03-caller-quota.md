# R6-03 Per-Caller Action Quota Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bound how many concurrent/frequent actions one calling plugin can push through a shared provider, so one caller can't starve others of the same provider.

**Architecture:** Two independent gates enforced in `src/ipc/protocol.rs`'s `ActionRequest` handling, keyed by `(requester_id, provider_id)`: a concurrency cap (scan `PluginRegistry.pending_actions`) and a rate limit (`governor` keyed limiter, same crate/pattern as the existing `ipc_rate_limit_rps`). Both off by default. Exceeding either returns a new `ActionStatus::ACTION_QUOTA_EXCEEDED` without forwarding the request to the provider.

**Tech Stack:** Rust, tokio, `governor` (keyed rate limiter, already a dependency), `dashmap` (already backs `PluginRegistry`), `prost`-generated protobuf.

## Global Constraints

- Both new config fields are `Option<u32>`, default `None` = unlimited (spec: "Off by default").
- Quota is keyed by `(requester_id, provider_id)`, never per-caller-globally (spec: "Per (caller, provider) pair").
- Concurrency check uses a scan of `PluginRegistry.pending_actions`, not a separately maintained counter (spec: avoids 3-site desync risk).
- New `ActionStatus::ACTION_QUOTA_EXCEEDED = 5` is additive only — no renumbering of existing values (spec: "not a renumber").
- Proto change must land identically in `wire/proto/veyron_protocol.proto`, `sdk/cpp/proto/veyron_protocol.proto`, `sdk/python/proto/veyron_protocol.proto` in the same commit (T-17 CI drift check enforces this).
- `cargo test --all --all-features` must exit 0; `cargo clippy --all-targets --all-features -- -D warnings` and `cargo fmt --check` must be clean before each commit (project `Definition of Done`).

---

### Task 1: Add `ACTION_QUOTA_EXCEEDED` to the protocol

**Files:**
- Modify: `wire/proto/veyron_protocol.proto:141-147`
- Modify: `sdk/cpp/proto/veyron_protocol.proto` (mirror)
- Modify: `sdk/python/proto/veyron_protocol.proto` (mirror)

**Interfaces:**
- Produces: `veyron::proto::veyron::ActionStatus::ActionQuotaExceeded` (Rust, via `prost-build` codegen from the new enum value), used by Task 4 and Task 5.

- [ ] **Step 1: Edit the enum in all three proto files identically**

In `wire/proto/veyron_protocol.proto`, change:

```protobuf
enum ActionStatus {
  ACTION_OK              = 0;
  ACTION_ERROR           = 1;
  ACTION_TIMEOUT         = 2;
  ACTION_PERMISSION_DENY = 3;  // plugin didn't declare the required permission
  ACTION_NOT_FOUND       = 4;  // no such action in the kernel
}
```

to:

```protobuf
enum ActionStatus {
  ACTION_OK              = 0;
  ACTION_ERROR           = 1;
  ACTION_TIMEOUT         = 2;
  ACTION_PERMISSION_DENY = 3;  // plugin didn't declare the required permission
  ACTION_NOT_FOUND       = 4;  // no such action in the kernel
  ACTION_QUOTA_EXCEEDED  = 5;  // caller's per-provider rate/concurrency quota exceeded (R6-03)
}
```

Apply the exact same change to `sdk/cpp/proto/veyron_protocol.proto` and `sdk/python/proto/veyron_protocol.proto`.

- [ ] **Step 2: Verify all three files are identical**

Run: `diff wire/proto/veyron_protocol.proto sdk/cpp/proto/veyron_protocol.proto && diff wire/proto/veyron_protocol.proto sdk/python/proto/veyron_protocol.proto`
Expected: no output (both diffs empty).

- [ ] **Step 3: Build to regenerate Rust bindings**

Run: `cargo build`
Expected: exits 0. This regenerates `ActionStatus` in the `prost`-generated code to include `ActionQuotaExceeded`.

- [ ] **Step 4: Commit**

```bash
git add wire/proto/veyron_protocol.proto sdk/cpp/proto/veyron_protocol.proto sdk/python/proto/veyron_protocol.proto
git commit -m "feat: add ACTION_QUOTA_EXCEEDED status for R6-03 caller quotas"
```

---

### Task 2: Add caller-quota config fields

**Files:**
- Modify: `src/utils/config.rs` (struct fields + `Default` impl)
- Modify: `config.yaml` (documented, commented-out example)

**Interfaces:**
- Produces: `Config.action_caller_rate_limit_rps: Option<u32>`, `Config.action_caller_max_concurrent: Option<u32>`, consumed by Task 4 (protocol.rs) and Task 5 (orchestrator.rs wiring), and by test `Config` construction in Task 6.

- [ ] **Step 1: Add the two fields to `Config`**

In `src/utils/config.rs`, immediately after the existing `ipc_rate_limit_rps` field (around line 80-82):

```rust
    /// Per-plugin IPC send rate limit (messages per second per connection). None = unlimited.
    /// Exceeding the limit sends ERR_RATE_LIMITED without disconnecting the plugin.
    #[serde(default)]
    pub ipc_rate_limit_rps: Option<u32>,
    /// R6-03: per-(caller, provider) action rate limit — requests/second one calling
    /// plugin may send through one action provider. None = unlimited. Exceeding sends
    /// ActionResponse{status: ACTION_QUOTA_EXCEEDED} without forwarding to the provider.
    #[serde(default)]
    pub action_caller_rate_limit_rps: Option<u32>,
    /// R6-03: per-(caller, provider) max simultaneous pending actions one calling
    /// plugin may have in flight against one action provider. None = unlimited.
    #[serde(default)]
    pub action_caller_max_concurrent: Option<u32>,
```

- [ ] **Step 2: Add both fields to the `Default` impl**

In `src/utils/config.rs`, immediately after `ipc_rate_limit_rps: None,` in the `Default for Config` block:

```rust
            ipc_rate_limit_rps: None,
            action_caller_rate_limit_rps: None,
            action_caller_max_concurrent: None,
```

- [ ] **Step 3: Verify it builds**

Run: `cargo build`
Expected: exits 0.

- [ ] **Step 4: Document the new fields in `config.yaml`**

In `config.yaml`, immediately after the `ipc_rate_limit_rps` comment block (line 26-27):

```yaml
# Per-plugin IPC send rate limit. Exceeding returns ERR_RATE_LIMITED without disconnect.
# ipc_rate_limit_rps: 500    # messages per second per plugin connection; default: unlimited

# Per-(caller, provider) action quota (R6-03) — bounds one calling plugin from
# starving others via actions routed through a shared provider (e.g. `network`).
# Exceeding either returns ActionResponse{status: ACTION_QUOTA_EXCEEDED}.
# action_caller_rate_limit_rps: 50    # action requests/second per (caller, provider); default: unlimited
# action_caller_max_concurrent: 10    # simultaneous pending actions per (caller, provider); default: unlimited
```

- [ ] **Step 5: Commit**

```bash
git add src/utils/config.rs config.yaml
git commit -m "feat: add action_caller_rate_limit_rps/action_caller_max_concurrent config (R6-03)"
```

---

### Task 3: `PluginRegistry::count_pending_actions_for`

**Files:**
- Modify: `src/plugins/registry.rs` (new method, after `sweep_expired_actions`)
- Test: `tests/unit/test_registry.rs`

**Interfaces:**
- Consumes: `PluginRegistry.pending_actions: DashMap<String, PendingAction>` (existing), `PendingAction.requester_id: String`, `PendingAction.provider_id: String` (existing fields).
- Produces: `PluginRegistry::count_pending_actions_for(&self, requester_id: &str, provider_id: &str) -> u32`, consumed by Task 4.

- [ ] **Step 1: Write the failing test**

In `tests/unit/test_registry.rs`, add after `take_pending_action_if_provider_mismatched_provider_leaves_it_in_place` (end of file, after line 429):

```rust
fn dummy_pending_with_requester_and_provider(
    original_action_id: &str,
    deadline: Instant,
    requester_id: &str,
    provider_id: &str,
) -> PendingAction {
    PendingAction {
        requester_write_tx: dummy_write_tx(),
        original_action_id: original_action_id.to_string(),
        requester_id: requester_id.to_string(),
        deadline,
        provider_id: provider_id.to_string(),
    }
}

#[test]
fn count_pending_actions_for_counts_only_matching_requester_and_provider() {
    let reg = PluginRegistry::new();
    let deadline = Instant::now() + Duration::from_secs(30);

    // caller-a -> provider-x (2 in flight)
    reg.register_pending_action(
        "kact-1".to_string(),
        dummy_pending_with_requester_and_provider("act-1", deadline, "caller-a", "provider-x"),
    );
    reg.register_pending_action(
        "kact-2".to_string(),
        dummy_pending_with_requester_and_provider("act-2", deadline, "caller-a", "provider-x"),
    );
    // caller-a -> provider-y (different provider, must not count toward provider-x)
    reg.register_pending_action(
        "kact-3".to_string(),
        dummy_pending_with_requester_and_provider("act-3", deadline, "caller-a", "provider-y"),
    );
    // caller-b -> provider-x (different caller, must not count toward caller-a)
    reg.register_pending_action(
        "kact-4".to_string(),
        dummy_pending_with_requester_and_provider("act-4", deadline, "caller-b", "provider-x"),
    );

    assert_eq!(
        reg.count_pending_actions_for("caller-a", "provider-x"),
        2,
        "only caller-a's actions against provider-x must count"
    );
    assert_eq!(reg.count_pending_actions_for("caller-a", "provider-y"), 1);
    assert_eq!(reg.count_pending_actions_for("caller-b", "provider-x"), 1);
    assert_eq!(reg.count_pending_actions_for("caller-c", "provider-x"), 0);
}

#[test]
fn count_pending_actions_for_reflects_removal() {
    let reg = PluginRegistry::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    reg.register_pending_action(
        "kact-1".to_string(),
        dummy_pending_with_requester_and_provider("act-1", deadline, "caller-a", "provider-x"),
    );
    assert_eq!(reg.count_pending_actions_for("caller-a", "provider-x"), 1);

    reg.take_pending_action("kact-1");
    assert_eq!(
        reg.count_pending_actions_for("caller-a", "provider-x"),
        0,
        "count must drop to 0 once the pending action is taken/removed"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test unit count_pending_actions_for -- --nocapture`
Expected: FAIL — `count_pending_actions_for` not found on `PluginRegistry`.

- [ ] **Step 3: Implement the method**

In `src/plugins/registry.rs`, immediately after `sweep_expired_actions` (after its closing `}`, around line 210-215 — check the exact end of that method body first with `Read`):

```rust
    /// Count in-flight pending actions for a given `(requester_id, provider_id)`
    /// pair (R6-03). Used to enforce the per-caller concurrency cap against a
    /// shared provider. A scan, not a maintained counter — bounded by total
    /// kernel-wide in-flight actions, which `sweep_expired_actions` already
    /// keeps bounded, and can't desync the way a separately incremented/
    /// decremented counter could across the three existing removal sites.
    pub fn count_pending_actions_for(&self, requester_id: &str, provider_id: &str) -> u32 {
        self.pending_actions
            .iter()
            .filter(|e| e.requester_id == requester_id && e.provider_id == provider_id)
            .count() as u32
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test unit count_pending_actions_for -- --nocapture`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/plugins/registry.rs tests/unit/test_registry.rs
git commit -m "feat: add PluginRegistry::count_pending_actions_for (R6-03)"
```

---

### Task 4: Enforce the quota in `ActionRequest` routing

**Files:**
- Modify: `src/ipc/protocol.rs` (`run_with_context`, `handle_kernel_message`, the `ActionRequest` match arm)

**Interfaces:**
- Consumes: `Config.action_caller_rate_limit_rps`, `Config.action_caller_max_concurrent` (Task 2), `PluginRegistry::count_pending_actions_for` (Task 3), `ActionStatus::ActionQuotaExceeded` (Task 1).
- Produces: `MessageRouter::run_with_context` gains two new trailing params `action_caller_rate_limit_rps: Option<u32>, action_caller_max_concurrent: Option<u32>` — consumed by Task 5's orchestrator wiring and Task 6's tests via `start_kernel_with_config`.

- [ ] **Step 1: Add the two new params to `run_with_context`**

In `src/ipc/protocol.rs`, in the `run_with_context` signature (around line 61-81), add after `ipc_rate_limit_rps: Option<u32>,`:

```rust
        ipc_rate_limit_rps: Option<u32>,
        // R6-03: per-(caller, provider) action quota. Both None = unlimited,
        // matching ipc_rate_limit_rps's existing opt-in convention.
        action_caller_rate_limit_rps: Option<u32>,
        action_caller_max_concurrent: Option<u32>,
        action_timeout_ms: u32,
```

- [ ] **Step 2: Build the keyed rate limiter alongside `ipc_limiter`**

In `src/ipc/protocol.rs`, immediately after the existing `ipc_limiter` construction (around line 96-100):

```rust
        // Per-connection IPC send rate limiter keyed by conn_id.
        let ipc_limiter: Option<Arc<DefaultKeyedRateLimiter<u64>>> =
            ipc_rate_limit_rps.and_then(|rps| {
                NonZeroU32::new(rps).map(|r| Arc::new(RateLimiter::keyed(Quota::per_second(r))))
            });

        // R6-03: per-(caller, provider) action rate limiter. Keyed by a tuple so
        // hammering one provider doesn't burn a caller's budget against an
        // unrelated provider it also legitimately calls.
        let action_limiter: Option<Arc<DefaultKeyedRateLimiter<(String, String)>>> =
            action_caller_rate_limit_rps.and_then(|rps| {
                NonZeroU32::new(rps).map(|r| Arc::new(RateLimiter::keyed(Quota::per_second(r))))
            });
```

- [ ] **Step 3: Prune `action_limiter` on the existing tick, and pass both new values into `handle_kernel_message`**

In `src/ipc/protocol.rs`, in the `prune_tick.tick()` arm (around line 112-115), add the retain call next to the existing one:

```rust
                _ = prune_tick.tick() => {
                    if let Some(limiter) = &ipc_limiter {
                        limiter.retain_recent();
                    }
                    if let Some(limiter) = &action_limiter {
                        limiter.retain_recent();
                    }
```

In the `"kernel" =>` match arm (around line 177-191), add the two new args to the `handle_kernel_message` call:

```rust
                "kernel" => {
                    counter!("messages_routed_total", "routing" => "kernel").increment(1);
                    Self::handle_kernel_message(
                        msg,
                        &registry,
                        &event_bus,
                        &jwt_validator,
                        start_time,
                        config_path.as_deref(),
                        event_store.as_deref(),
                        &mac_secret,
                        config_permissions.as_deref(),
                        action_limiter.as_deref(),
                        action_caller_max_concurrent,
                        action_timeout_ms,
                    )
                    .await
                }
```

- [ ] **Step 4: Add the two new params to `handle_kernel_message`**

In `src/ipc/protocol.rs`, in the `handle_kernel_message` signature (around line 228-239), add after `config_permissions: Option<&HashMap<String, Vec<String>>>,`:

```rust
    async fn handle_kernel_message(
        msg: IncomingMessage,
        registry: &PluginRegistry,
        event_bus: &EventBus,
        jwt_validator: &Option<Arc<JwtValidator>>,
        start_time: Instant,
        config_path: Option<&str>,
        event_store: Option<&EventStore>,
        mac_secret: &Option<Arc<Vec<u8>>>,
        config_permissions: Option<&HashMap<String, Vec<String>>>,
        action_limiter: Option<&DefaultKeyedRateLimiter<(String, String)>>,
        action_caller_max_concurrent: Option<u32>,
        action_timeout_ms: u32,
    ) -> bool {
```

- [ ] **Step 5: Add the two quota-check match arms in the `ActionRequest` handling**

In `src/ipc/protocol.rs`, in the `ActionRequest` match (around line 511-529), insert two new guarded arms between the existing `ActionPermissionDeny` arm and the success arm:

```rust
                let not_found_status = match registry.find_action_provider(&req.action) {
                    ActionLookup::NotFound => Some(ActionStatus::ActionNotFound),
                    ActionLookup::Ambiguous(providers) => {
                        warn!(
                            action = %req.action,
                            providers = ?providers,
                            "ambiguous action declaration: multiple providers, refusing to route"
                        );
                        Some(ActionStatus::ActionNotFound)
                    }
                    ActionLookup::Found(provider)
                        if required_permission_for_action(&req.action).is_some_and(|perm| {
                            check_permission(registry, &provider.plugin_id, perm).is_err()
                                || check_permission(registry, &sender_id, perm).is_err()
                        }) =>
                    {
                        Some(ActionStatus::ActionPermissionDeny)
                    }
                    // R6-03: concurrency cap — checked before the rate limit since it's
                    // the direct fix for "one caller holds N provider slots open" and is
                    // cheaper (a DashMap scan, no token-bucket state touch) to fail fast on.
                    ActionLookup::Found(ref provider)
                        if action_caller_max_concurrent.is_some_and(|cap| {
                            registry.count_pending_actions_for(&sender_id, &provider.plugin_id)
                                >= cap
                        }) =>
                    {
                        counter!("action_quota_denied_total", "reason" => "concurrency")
                            .increment(1);
                        Some(ActionStatus::ActionQuotaExceeded)
                    }
                    // R6-03: rate limit — keyed by (caller, provider), same governor
                    // crate/pattern as the existing per-conn ipc_limiter.
                    ActionLookup::Found(ref provider)
                        if action_limiter.is_some_and(|limiter| {
                            limiter
                                .check_key(&(sender_id.clone(), provider.plugin_id.clone()))
                                .is_err()
                        }) =>
                    {
                        counter!("action_quota_denied_total", "reason" => "rate").increment(1);
                        Some(ActionStatus::ActionQuotaExceeded)
                    }
                    ActionLookup::Found(provider) => {
```

(The rest of the `ActionLookup::Found(provider) => { ... }` success body and the closing `};` are unchanged.)

- [ ] **Step 6: Build**

Run: `cargo build`
Expected: exits 0. (If it fails on a moved/borrowed `provider` in the new guards, the `ref provider` bindings above are required since the guards only borrow — the final success arm still takes `provider` by value.)

- [ ] **Step 7: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add src/ipc/protocol.rs
git commit -m "feat: enforce per-(caller, provider) action quota in ActionRequest routing (R6-03)"
```

---

### Task 5: Wire config through the orchestrator

**Files:**
- Modify: `src/kernel/orchestrator.rs`

**Interfaces:**
- Consumes: `Config.action_caller_rate_limit_rps`, `Config.action_caller_max_concurrent` (Task 2), `MessageRouter::run_with_context`'s new params (Task 4).

- [ ] **Step 1: Add the two new args to the `run_with_context` call**

In `src/kernel/orchestrator.rs` (around line 164-178), insert the two new config values in the same position `run_with_context`'s signature expects them (right after `config.ipc_rate_limit_rps,`):

```rust
        tokio::spawn(MessageRouter::run_with_context(
            router_rx,
            Arc::clone(&registry),
            Arc::clone(&event_bus),
            jwt_validator.clone(),
            kernel_start,
            config_path,
            event_store.clone(),
            mac_secret,
            Some(config_permissions),
            config.ipc_rate_limit_rps,
            config.action_caller_rate_limit_rps,
            config.action_caller_max_concurrent,
            config.action_timeout_ms,
            config.max_conn_errors,
            config.max_tracked_error_conns,
        ));
```

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: exits 0.

- [ ] **Step 3: Run full test suite to confirm nothing existing broke from the signature change**

Run: `cargo test --all --all-features`
Expected: exits 0, same pass count as baseline (266+ passing) plus the 2 new registry tests from Task 3.

- [ ] **Step 4: Commit**

```bash
git add src/kernel/orchestrator.rs
git commit -m "feat: thread action_caller_rate_limit_rps/action_caller_max_concurrent into MessageRouter (R6-03)"
```

---

### Task 6: Integration tests for the quota gates

**Files:**
- Test: `tests/integration/test_kernel_commands.rs`

**Interfaces:**
- Consumes: `helpers::start_kernel_with_config` (existing, `tests/integration/helpers.rs:34`), `helpers::test_config` (existing, `tests/integration/helpers.rs:12`), `veyron_sdk::VeyronClient::{connect, register, send, recv, send_action}` (existing SDK — note there is no provider-side reply helper; providers build a raw `Envelope{ Payload::ActionResponse(...) }` and call `.send("kernel", env)`, exactly as the existing `weather_action_round_trip`-style test at `tests/integration/test_kernel_commands.rs:255-304` already does), `ActionStatus::{ActionOk, ActionQuotaExceeded}` (Task 1).

- [ ] **Step 1: Add the two new helper imports**

At the top of `tests/integration/test_kernel_commands.rs`, change:

```rust
use super::helpers::start_kernel;
```

to:

```rust
use super::helpers::{start_kernel, start_kernel_with_config, test_config};
```

- [ ] **Step 2: Write the concurrency-cap test**

Add to `tests/integration/test_kernel_commands.rs` (near the existing T-19 test, e.g. right after `kernel_denies_action_when_requester_lacks_required_permission`). This drives raw `ActionRequest`/`ActionResponse` envelopes directly (like the existing `weather_action_round_trip`-style test) instead of `send_action`, because `send_action` blocks its connection awaiting its own response — to hold 2 requests pending at once from one caller, the requests must be fired without waiting:

```rust
#[tokio::test]
async fn action_concurrency_cap_denies_third_concurrent_call_to_same_provider() {
    // R6-03: a caller with action_caller_max_concurrent = 2 gets a 3rd concurrent
    // ActionRequest to the SAME provider denied, but a concurrent request to a
    // DIFFERENT provider still succeeds — proves per-(caller, provider) keying.
    let mut cfg = test_config("/tmp/veyron_integ_action_concurrency_cap.sock", 19230);
    cfg.action_caller_max_concurrent = Some(2);
    let (shutdown_tx, _registry, _bus) = start_kernel_with_config(cfg).await;

    let mut provider_x = VeyronClient::connect("/tmp/veyron_integ_action_concurrency_cap.sock")
        .await
        .unwrap();
    provider_x
        .register(
            "provider-x",
            PluginManifest {
                actions: vec!["slow_action".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let mut provider_y = VeyronClient::connect("/tmp/veyron_integ_action_concurrency_cap.sock")
        .await
        .unwrap();
    provider_y
        .register(
            "provider-y",
            PluginManifest {
                actions: vec!["other_action".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let mut caller = VeyronClient::connect("/tmp/veyron_integ_action_concurrency_cap.sock")
        .await
        .unwrap();
    caller
        .register("caller-a", PluginManifest::default())
        .await
        .unwrap();

    // Fire 2 raw ActionRequests to provider-x without waiting for a response —
    // provider-x never replies to these, so both stay pending and fill the cap.
    for i in 0..2 {
        let env = veyron::proto::veyron::Envelope {
            payload: Some(veyron::proto::veyron::envelope::Payload::ActionRequest(
                veyron::proto::veyron::ActionRequest {
                    action_id: format!("fill-{i}"),
                    action: "slow_action".to_string(),
                    params_json: b"{}".to_vec(),
                    timeout_ms: 5000,
                },
            )),
            ..Default::default()
        };
        caller.send("kernel", env).await.unwrap();
    }

    // A 3rd ActionRequest to the SAME provider must be denied immediately —
    // the kernel never forwards it, so no provider recv() is needed here.
    let deny_env = veyron::proto::veyron::Envelope {
        payload: Some(veyron::proto::veyron::envelope::Payload::ActionRequest(
            veyron::proto::veyron::ActionRequest {
                action_id: "act-3".to_string(),
                action: "slow_action".to_string(),
                params_json: b"{}".to_vec(),
                timeout_ms: 5000,
            },
        )),
        ..Default::default()
    };
    caller.send("kernel", deny_env).await.unwrap();

    let deny_resp = timeout(Duration::from_secs(2), caller.recv())
        .await
        .expect("must not hang — denial is synchronous")
        .expect("recv failed");
    match deny_resp.payload {
        Some(veyron::proto::veyron::envelope::Payload::ActionResponse(resp)) => {
            assert_eq!(resp.action_id, "act-3");
            assert_eq!(
                resp.status,
                ActionStatus::ActionQuotaExceeded as i32,
                "3rd concurrent action to the same provider must be denied once cap is reached"
            );
        }
        other => panic!("expected ActionResponse, got {other:?}"),
    }

    // A request to the DIFFERENT provider (provider-y) must still succeed —
    // proves the cap is per-(caller, provider), not global.
    let other_env = veyron::proto::veyron::Envelope {
        payload: Some(veyron::proto::veyron::envelope::Payload::ActionRequest(
            veyron::proto::veyron::ActionRequest {
                action_id: "act-4".to_string(),
                action: "other_action".to_string(),
                params_json: b"{}".to_vec(),
                timeout_ms: 2000,
            },
        )),
        ..Default::default()
    };
    caller.send("kernel", other_env).await.unwrap();

    let received = timeout(Duration::from_secs(2), provider_y.recv())
        .await
        .expect("provider-y recv timed out")
        .expect("provider-y recv failed");
    let internal_action_id = match received.payload {
        Some(veyron::proto::veyron::envelope::Payload::ActionRequest(req)) => {
            assert_eq!(req.action, "other_action");
            req.action_id
        }
        other => panic!("expected ActionRequest, got {other:?}"),
    };
    let resp_env = veyron::proto::veyron::Envelope {
        payload: Some(veyron::proto::veyron::envelope::Payload::ActionResponse(
            veyron::proto::veyron::ActionResponse {
                action_id: internal_action_id,
                status: ActionStatus::ActionOk as i32,
                data_json: b"{}".to_vec(),
                error: String::new(),
            },
        )),
        ..Default::default()
    };
    provider_y.send("kernel", resp_env).await.unwrap();

    let resp_y = timeout(Duration::from_secs(2), caller.recv())
        .await
        .expect("must not hang")
        .expect("recv failed");
    match resp_y.payload {
        Some(veyron::proto::veyron::envelope::Payload::ActionResponse(resp)) => {
            assert_eq!(resp.action_id, "act-4");
            assert_eq!(
                resp.status,
                ActionStatus::ActionOk as i32,
                "a different provider must be unaffected by the caller's cap against provider-x"
            );
        }
        other => panic!("expected ActionResponse, got {other:?}"),
    }

    let _ = shutdown_tx.send(());
}
```

- [ ] **Step 3: Run the concurrency test**

Run: `cargo test --test integration action_concurrency_cap_denies_third_concurrent_call_to_same_provider -- --nocapture`
Expected: PASS (Task 4/5 already landed earlier in this plan, so the enforcement exists). If it fails, check the failure is a real logic bug, not a wrong assumption about message ordering — the test relies on `caller.recv()` returning `act-3`'s denial before `act-4`'s OK because `act-3`'s response is generated synchronously by the kernel before `act-4` is even sent, so ordering is deterministic.

- [ ] **Step 4: Write the rate-limit test**

Add to `tests/integration/test_kernel_commands.rs`:

```rust
#[tokio::test]
async fn action_rate_limit_denies_burst_above_configured_rps() {
    // R6-03: with action_caller_rate_limit_rps = 1, a rapid second request from
    // the same (caller, provider) within the same second is denied.
    let mut cfg = test_config("/tmp/veyron_integ_action_rate_limit.sock", 19231);
    cfg.action_caller_rate_limit_rps = Some(1);
    let (shutdown_tx, _registry, _bus) = start_kernel_with_config(cfg).await;

    let mut provider = VeyronClient::connect("/tmp/veyron_integ_action_rate_limit.sock")
        .await
        .unwrap();
    provider
        .register(
            "rl-provider",
            PluginManifest {
                actions: vec!["ping_action".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let mut caller = VeyronClient::connect("/tmp/veyron_integ_action_rate_limit.sock")
        .await
        .unwrap();
    caller
        .register("rl-caller", PluginManifest::default())
        .await
        .unwrap();

    // First request: routes through fine (rps=1 allows one immediately).
    let request_fut = tokio::spawn(async move {
        let resp = caller
            .send_action("ping_action", b"{}", 2000)
            .await
            .unwrap();
        (caller, resp)
    });

    let received = timeout(Duration::from_secs(2), provider.recv())
        .await
        .expect("provider recv timed out")
        .expect("provider recv failed");
    let internal_action_id = match received.payload {
        Some(veyron::proto::veyron::envelope::Payload::ActionRequest(req)) => req.action_id,
        other => panic!("expected ActionRequest, got {other:?}"),
    };
    let resp_env = veyron::proto::veyron::Envelope {
        payload: Some(veyron::proto::veyron::envelope::Payload::ActionResponse(
            veyron::proto::veyron::ActionResponse {
                action_id: internal_action_id,
                status: ActionStatus::ActionOk as i32,
                data_json: b"{}".to_vec(),
                error: String::new(),
            },
        )),
        ..Default::default()
    };
    provider.send("kernel", resp_env).await.unwrap();

    let (mut caller, first) = timeout(Duration::from_secs(2), request_fut)
        .await
        .expect("timed out")
        .expect("task panicked");
    assert_eq!(first.status, ActionStatus::ActionOk as i32);

    // Immediately send a second request — with rps=1 the bucket should be empty.
    let second = timeout(
        Duration::from_secs(2),
        caller.send_action("ping_action", b"{}", 2000),
    )
    .await
    .expect("must not hang")
    .expect("send_action failed");
    assert_eq!(
        second.status,
        ActionStatus::ActionQuotaExceeded as i32,
        "immediate second request must be denied by the rps=1 limiter"
    );

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn action_quota_unset_leaves_routing_unlimited() {
    // R6-03: with both quota configs left at their None default, action routing
    // behaves exactly as before this feature (regression guard for the opt-in
    // convention).
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_action_quota_unset.sock", 19232).await;

    let mut provider = VeyronClient::connect("/tmp/veyron_integ_action_quota_unset.sock")
        .await
        .unwrap();
    provider
        .register(
            "unlimited-provider",
            PluginManifest {
                actions: vec!["ping_action".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let mut caller = VeyronClient::connect("/tmp/veyron_integ_action_quota_unset.sock")
        .await
        .unwrap();
    caller
        .register("unlimited-caller", PluginManifest::default())
        .await
        .unwrap();

    for i in 0..5 {
        let request_fut = tokio::spawn(async move {
            let resp = caller
                .send_action("ping_action", b"{}", 2000)
                .await
                .unwrap();
            (caller, resp)
        });

        let received = timeout(Duration::from_secs(2), provider.recv())
            .await
            .unwrap_or_else(|_| panic!("provider recv timed out on iteration {i}"))
            .expect("provider recv failed");
        let internal_action_id = match received.payload {
            Some(veyron::proto::veyron::envelope::Payload::ActionRequest(req)) => req.action_id,
            other => panic!("expected ActionRequest, got {other:?}"),
        };
        let resp_env = veyron::proto::veyron::Envelope {
            payload: Some(veyron::proto::veyron::envelope::Payload::ActionResponse(
                veyron::proto::veyron::ActionResponse {
                    action_id: internal_action_id,
                    status: ActionStatus::ActionOk as i32,
                    data_json: b"{}".to_vec(),
                    error: String::new(),
                },
            )),
            ..Default::default()
        };
        provider.send("kernel", resp_env).await.unwrap();

        let (c, resp) = timeout(Duration::from_secs(2), request_fut)
            .await
            .unwrap_or_else(|_| panic!("timed out on iteration {i}"))
            .expect("task panicked");
        caller = c;
        assert_eq!(
            resp.status,
            ActionStatus::ActionOk as i32,
            "with no quota configured, no request should ever be denied (iteration {i})"
        );
    }

    let _ = shutdown_tx.send(());
}
```

- [ ] **Step 5: Run all three new tests**

Run: `cargo test --test integration action_concurrency_cap_denies_third_concurrent_call_to_same_provider action_rate_limit_denies_burst_above_configured_rps action_quota_unset_leaves_routing_unlimited -- --nocapture`
Expected: all 3 PASS.

- [ ] **Step 6: Run the full suite**

Run: `cargo test --all --all-features`
Expected: exits 0, no regressions.

- [ ] **Step 7: Run clippy and fmt**

Run: `cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --check`
Expected: both clean.

- [ ] **Step 8: Commit**

```bash
git add tests/integration/test_kernel_commands.rs
git commit -m "test: cover R6-03 per-(caller, provider) action quota concurrency/rate/unset paths"
```

---

### Task 7: Update ROADMAP.md

**Files:**
- Modify: `ROADMAP.md`

- [ ] **Step 1: Mark R6-03 done**

In `ROADMAP.md`, change the `### R6-03 — Per-caller resource/rate limits at the kernel level` heading and body to match the `✅ done` style used by R6-01/T-01..T-20 (state what was fixed, list the new config fields, new `ActionStatus::ACTION_QUOTA_EXCEEDED`, the design doc path, and the test names from Task 6). Follow the exact prose pattern already used for R6-01 in `ROADMAP.md` (a "Fixed:" paragraph after the original problem description — do not delete the original problem description, append below it).

- [ ] **Step 2: Update the Task Summary table**

In `ROADMAP.md`'s `## Task Summary` table and the `**Ship gate:**` paragraph beneath it, note R6-03 as done, matching how R6-01 was folded into that paragraph.

- [ ] **Step 3: Commit**

```bash
git add ROADMAP.md
git commit -m "docs: mark R6-03 done in ROADMAP.md"
```

---

## Self-Review Notes

- **Spec coverage:** concurrency cap (Task 3/4/6), rate limit (Task 4/6), per-(caller, provider) keying (Task 3/4/6 all key by the tuple), off-by-default (Task 2's `None` defaults + Task 6's unset-test), new `ACTION_QUOTA_EXCEEDED` status (Task 1), metrics (`action_quota_denied_total`, Task 4 Step 5), config docs (Task 2 Step 4), roadmap update (Task 7) — all spec sections have a task.
- **Placeholder scan:** none found. Task 6's tests were revised after confirming against `sdk/rust/src/client.rs` that no provider-side reply helper exists (`send_action` only covers the requester side) and that two connections cannot register the same `plugin_id` concurrently (`src/plugins/registry.rs`'s `register` reserves one `by_conn_id` + one `by_plugin_id` slot atomically) — the concurrency test instead drives raw `ActionRequest`/`ActionResponse` envelopes on a single connection, matching the existing `weather_action_round_trip`-style pattern already in `tests/integration/test_kernel_commands.rs:255-304`.
- **Type consistency:** `action_caller_rate_limit_rps: Option<u32>` and `action_caller_max_concurrent: Option<u32>` used identically across Task 2 (declaration), Task 4 (consumption), Task 5 (wiring), Task 6 (test config). `count_pending_actions_for(&self, requester_id: &str, provider_id: &str) -> u32` signature matches between Task 3's definition and Task 4's call site.
