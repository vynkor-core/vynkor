# Action Routing to Provider Plugins Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement R5-07 option (b) — kernel-targeted `ActionRequest`s route to a registered provider plugin that declared the action in `manifest.actions`, with responses correlated back to the original requester by `action_id`.

**Architecture:** No proto changes. `PluginRegistry` gains a provider-lookup method and a pending-actions table (keyed by a kernel-minted internal id, not the requester's `action_id`, to avoid cross-process collisions). `MessageRouter::handle_kernel_message` gains provider routing on `ActionRequest` and a new `ActionResponse` arm that proxies the provider's answer back to the original requester, rewriting the id back. Timeout eviction piggybacks on the router's existing 60s `prune_tick`. The old `action_to_permission()` builtin-name map is retired as dead code.

**Tech Stack:** Rust, Tokio, `dashmap`, `prost` (existing stack, no new dependencies).

## Global Constraints

- No `.proto` changes — `ActionRequest`, `ActionResponse`, `PluginManifest.actions` already carry everything needed.
- No SDK changes — requester side already targets `"kernel"` via `VeyronClient::send_action`; provider side uses the SDK's existing public `send(target, envelope)`.
- No new `PermissionType` — a provider declaring the action in `manifest.actions` is sufficient authorization.
- Ambiguous providers (>1 plugin declares the same action) → `ACTION_NOT_FOUND`, not a pick.
- Provider-side failure statuses (`ACTION_ERROR`, `ACTION_PERMISSION_DENY`, etc.) proxy through unchanged — the kernel does not reinterpret them.
- Timeout default 30s when `timeout_ms == 0` (matches existing proto doc comment); eviction precision is bounded by the 60s `prune_tick`, not exact.
- Full spec: `docs/superpowers/specs/2026-07-02-action-routing-design.md`.

---

### Task 1: `PluginRegistry` — provider lookup

**Files:**
- Modify: `src/plugins/registry.rs`
- Test: `tests/unit/test_registry.rs`

**Interfaces:**
- Produces: `pub enum ActionLookup { NotFound, Found(PluginEntry), Ambiguous(Vec<String>) }` and `pub fn PluginRegistry::find_action_provider(&self, action: &str) -> ActionLookup`, consumed by Task 3.

- [ ] **Step 1: Write the failing tests**

Append to `tests/unit/test_registry.rs` (the file already has `dummy_write_tx()` and `dummy_manifest()` helpers at the top — reuse them):

```rust
fn manifest_with_actions(actions: &[&str]) -> PluginManifest {
    PluginManifest {
        actions: actions.iter().map(|s| s.to_string()).collect(),
        ..dummy_manifest()
    }
}

#[test]
fn find_action_provider_returns_not_found_when_no_provider() {
    use veyron::plugins::registry::ActionLookup;

    let reg = PluginRegistry::new();
    reg.register(
        "weather".to_string(),
        1,
        manifest_with_actions(&["get_forecast"]),
        dummy_write_tx(),
    )
    .unwrap();

    assert!(matches!(
        reg.find_action_provider("get_weather"),
        ActionLookup::NotFound
    ));
}

#[test]
fn find_action_provider_returns_found_for_single_provider() {
    use veyron::plugins::registry::ActionLookup;

    let reg = PluginRegistry::new();
    reg.register(
        "weather".to_string(),
        1,
        manifest_with_actions(&["get_weather"]),
        dummy_write_tx(),
    )
    .unwrap();

    match reg.find_action_provider("get_weather") {
        ActionLookup::Found(entry) => assert_eq!(entry.plugin_id, "weather"),
        other => panic!("expected Found, got {other:?}"),
    }
}

#[test]
fn find_action_provider_returns_ambiguous_for_multiple_providers() {
    use veyron::plugins::registry::ActionLookup;

    let reg = PluginRegistry::new();
    reg.register(
        "weather-a".to_string(),
        1,
        manifest_with_actions(&["get_weather"]),
        dummy_write_tx(),
    )
    .unwrap();
    reg.register(
        "weather-b".to_string(),
        2,
        manifest_with_actions(&["get_weather"]),
        dummy_write_tx(),
    )
    .unwrap();

    match reg.find_action_provider("get_weather") {
        ActionLookup::Ambiguous(mut ids) => {
            ids.sort();
            assert_eq!(ids, vec!["weather-a".to_string(), "weather-b".to_string()]);
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}
```

`ActionLookup` needs `Debug` for the `panic!("... {other:?}")` calls — include it in the derive in Step 3.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test unit find_action_provider -- --nocapture`
Expected: compile error — `find_action_provider` and `ActionLookup` don't exist yet.

- [ ] **Step 3: Implement `find_action_provider`**

In `src/plugins/registry.rs`, add after the `PluginState` enum:

```rust
#[derive(Debug, Clone)]
pub enum ActionLookup {
    NotFound,
    Found(PluginEntry),
    /// Colliding plugin ids, for the caller to log.
    Ambiguous(Vec<String>),
}
```

Add inside `impl PluginRegistry` (after `get_by_conn_id`):

```rust
    /// Scan registered plugins for one whose `manifest.actions` declares
    /// `action`. Ambiguity (>1 declarer) is surfaced rather than resolved —
    /// picking a winner would hide a deploy misconfiguration.
    pub fn find_action_provider(&self, action: &str) -> ActionLookup {
        let matches: Vec<PluginEntry> = self
            .by_plugin_id
            .iter()
            .filter(|e| e.manifest.actions.iter().any(|a| a == action))
            .map(|e| e.value().clone())
            .collect();

        match matches.len() {
            0 => ActionLookup::NotFound,
            1 => ActionLookup::Found(matches.into_iter().next().unwrap()),
            _ => ActionLookup::Ambiguous(matches.into_iter().map(|e| e.plugin_id).collect()),
        }
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test unit find_action_provider -- --nocapture`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add src/plugins/registry.rs tests/unit/test_registry.rs
git commit -m "feat: PluginRegistry::find_action_provider for action routing (R5-07)"
```

---

### Task 2: `PluginRegistry` — pending-action tracking

**Files:**
- Modify: `src/plugins/registry.rs`
- Test: `tests/unit/test_registry.rs`

**Interfaces:**
- Consumes: `PluginEntry`, `Outbound` (existing).
- Produces: `pub struct PendingAction { pub requester_write_tx: mpsc::Sender<Outbound>, pub original_action_id: String, pub requester_id: String, pub deadline: Instant }`, and `PluginRegistry::{register_pending_action, take_pending_action, sweep_expired_actions}`, consumed by Task 3 and Task 6.

- [ ] **Step 1: Write the failing tests**

Append to `tests/unit/test_registry.rs`:

```rust
use std::time::{Duration, Instant};
use veyron::plugins::registry::PendingAction;

fn dummy_pending(original_action_id: &str, deadline: Instant) -> PendingAction {
    PendingAction {
        requester_write_tx: dummy_write_tx(),
        original_action_id: original_action_id.to_string(),
        requester_id: "requester".to_string(),
        deadline,
    }
}

#[test]
fn pending_action_round_trip_take_returns_and_removes() {
    let reg = PluginRegistry::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    reg.register_pending_action("kact-1".to_string(), dummy_pending("act-1", deadline));

    let taken = reg.take_pending_action("kact-1").expect("must be present");
    assert_eq!(taken.original_action_id, "act-1");
    assert!(reg.take_pending_action("kact-1").is_none(), "must be removed after take");
}

#[test]
fn pending_action_take_missing_returns_none() {
    let reg = PluginRegistry::new();
    assert!(reg.take_pending_action("does-not-exist").is_none());
}

#[test]
fn sweep_expired_actions_evicts_past_deadline_only() {
    let reg = PluginRegistry::new();
    let now = Instant::now();
    reg.register_pending_action(
        "kact-expired".to_string(),
        dummy_pending("act-expired", now - Duration::from_secs(1)),
    );
    reg.register_pending_action(
        "kact-fresh".to_string(),
        dummy_pending("act-fresh", now + Duration::from_secs(60)),
    );

    let expired = reg.sweep_expired_actions(now);
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].original_action_id, "act-expired");

    // Fresh entry must remain, expired one must be gone.
    assert!(reg.take_pending_action("kact-fresh").is_some());
    assert!(reg.take_pending_action("kact-expired").is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test unit pending_action -- --nocapture` and `cargo test --test unit sweep_expired -- --nocapture`
Expected: compile error — `PendingAction`, `register_pending_action`, `take_pending_action`, `sweep_expired_actions` don't exist yet.

- [ ] **Step 3: Implement pending-action tracking**

In `src/plugins/registry.rs`, add after the `ActionLookup` enum from Task 1:

```rust
/// A kernel-routed action awaiting the provider's reply. Keyed in
/// `PluginRegistry::pending_actions` by a kernel-minted internal id (not the
/// requester's own `action_id`, which is only unique per-process and could
/// collide across two different plugin connections).
pub struct PendingAction {
    pub requester_write_tx: mpsc::Sender<Outbound>,
    pub original_action_id: String,
    pub requester_id: String,
    pub deadline: Instant,
}
```

Add a field to `PluginRegistry`:

```rust
pub struct PluginRegistry {
    by_plugin_id: DashMap<String, PluginEntry>,
    by_conn_id: DashMap<u64, String>,
    pong_times: DashMap<String, Instant>,
    pending_actions: DashMap<String, PendingAction>,
}
```

Initialize it in `new()`:

```rust
    pub fn new() -> Self {
        PluginRegistry {
            by_plugin_id: DashMap::new(),
            by_conn_id: DashMap::new(),
            pong_times: DashMap::new(),
            pending_actions: DashMap::new(),
        }
    }
```

Add methods inside `impl PluginRegistry` (after `find_action_provider`):

```rust
    pub fn register_pending_action(&self, internal_id: String, pending: PendingAction) {
        self.pending_actions.insert(internal_id, pending);
    }

    pub fn take_pending_action(&self, internal_id: &str) -> Option<PendingAction> {
        self.pending_actions.remove(internal_id).map(|(_, v)| v)
    }

    /// Evict and return all pending actions whose deadline has passed as of `now`.
    pub fn sweep_expired_actions(&self, now: Instant) -> Vec<PendingAction> {
        let expired_keys: Vec<String> = self
            .pending_actions
            .iter()
            .filter(|e| e.deadline <= now)
            .map(|e| e.key().clone())
            .collect();

        expired_keys
            .into_iter()
            .filter_map(|k| self.take_pending_action(&k))
            .collect()
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test unit --lib` won't apply (this is the `unit` integration-test binary) — use:
`cargo test --test unit registry -- --nocapture`
Expected: all `test_registry` tests pass, including the 3 from Task 1 and 3 new ones from this task.

- [ ] **Step 5: Commit**

```bash
git add src/plugins/registry.rs tests/unit/test_registry.rs
git commit -m "feat: PluginRegistry pending-action tracking for action correlation (R5-07)"
```

---

### Task 3: Route kernel-targeted `ActionRequest` to provider, proxy `ActionResponse` back

**Files:**
- Modify: `src/ipc/protocol.rs:1-26` (imports), `src/ipc/protocol.rs:385-431` (`ActionRequest` arm), `src/ipc/protocol.rs` (new `ActionResponse` arm, inserted after the `ActionRequest` arm)
- Test: `tests/integration/test_kernel_commands.rs`

**Interfaces:**
- Consumes: `ActionLookup`, `PendingAction`, `PluginRegistry::{find_action_provider, register_pending_action, take_pending_action}` (Tasks 1–2); `MessageRouter::send_envelope(&mpsc::Sender<Outbound>, Envelope)` (existing, `src/ipc/protocol.rs:657`).
- Produces: kernel now actually routes and answers kernel-targeted `ActionRequest`s for declared actions — no interface other tasks depend on besides Task 6's reuse of `Instant`/`Duration` already imported here.

- [ ] **Step 1: Write the failing integration test**

Add to `tests/integration/test_kernel_commands.rs` (uses the existing `start_kernel` helper and `veyron_sdk::VeyronClient`, both already imported in this file):

```rust
#[tokio::test]
async fn kernel_routes_action_to_declared_provider_and_correlates_response() {
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_action_route.sock", 19217).await;

    // Provider registers first and declares the action.
    let mut provider = VeyronClient::connect("/tmp/veyron_integ_action_route.sock")
        .await
        .unwrap();
    provider
        .register(
            "weather-provider",
            PluginManifest {
                actions: vec!["get_weather".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let mut requester = VeyronClient::connect("/tmp/veyron_integ_action_route.sock")
        .await
        .unwrap();
    requester
        .register("weather-requester", PluginManifest::default())
        .await
        .unwrap();

    // Requester fires the action at "kernel" (existing SDK API, unaware of routing).
    let request_fut = requester.send_action("get_weather", br#"{"city":"nyc"}"#, 2000);

    // Provider receives the routed request and answers OK, targeted at "kernel".
    let received = timeout(Duration::from_secs(2), provider.recv())
        .await
        .expect("provider recv timed out")
        .expect("provider recv failed");
    let internal_action_id = match received.payload {
        Some(veyron::proto::veyron::envelope::Payload::ActionRequest(req)) => {
            assert_eq!(req.action, "get_weather");
            assert_eq!(req.params_json, br#"{"city":"nyc"}"#);
            req.action_id
        }
        other => panic!("expected ActionRequest, got {other:?}"),
    };

    let resp_env = veyron::proto::veyron::Envelope {
        payload: Some(veyron::proto::veyron::envelope::Payload::ActionResponse(
            veyron::proto::veyron::ActionResponse {
                action_id: internal_action_id,
                status: ActionStatus::ActionOk as i32,
                data_json: br#"{"temp_f":72}"#.to_vec(),
                error: String::new(),
            },
        )),
        ..Default::default()
    };
    provider.send("kernel", resp_env).await.unwrap();

    let resp = timeout(Duration::from_secs(2), request_fut)
        .await
        .expect("timed out")
        .expect("send_action failed");

    assert_eq!(resp.status, ActionStatus::ActionOk as i32);
    assert_eq!(resp.data_json, br#"{"temp_f":72}"#);

    let _ = shutdown_tx.send(());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test integration kernel_routes_action_to_declared_provider -- --nocapture`
Expected: FAIL — requester's `send_action` times out (kernel still returns `ACTION_NOT_FOUND` immediately from the old stub; the test's outer `timeout(...).expect("timed out")` will actually panic on the *unwrap of `ACTION_OK`* assertion first, since the stub responds fast with `ACTION_NOT_FOUND` — either way, `assert_eq!(resp.status, ActionStatus::ActionOk as i32)` fails).

- [ ] **Step 3: Rewrite the `ActionRequest` arm and add the `ActionResponse` arm**

In `src/ipc/protocol.rs`, update the import block (lines 1–15):

```rust
use crate::auth::jwt::JwtValidator;
use crate::auth::permissions::{check_ipc_send, check_ipc_target, check_permission};
use crate::events::bus::EventBus;
use crate::events::store::EventStore;
use crate::ipc::connection::{out_frame, Outbound};
use crate::ipc::framing::{target_as_str, Frame, FLAG_RAW_BINARY};
use crate::ipc::messages::IncomingMessage;
use crate::kernel::commands::{CommandHandler, CommandOutcome};
use crate::plugins::registry::{ActionLookup, PendingAction, PluginRegistry};
use crate::proto::veyron::{
    envelope, ActionRequest, ActionResponse, ActionStatus, Envelope, ErrorCode, ErrorMessage,
    Event, KernelCommandAck, PermissionType, PluginRegisterAck, Pong,
};
```

(`action_to_permission` is dropped from the `permissions` import — Task 3 is its last caller. `ActionRequest` is added — needed to build the forwarded request.)

Add a new correlation-id counter next to `MSG_SEQ` (around line 28):

```rust
static MSG_SEQ: AtomicU64 = AtomicU64::new(0);
static ACTION_CORRELATION_SEQ: AtomicU64 = AtomicU64::new(0);

/// Default action timeout when the requester passes `timeout_ms: 0` (matches
/// the proto doc comment on `ActionRequest.timeout_ms`).
const DEFAULT_ACTION_TIMEOUT_MS: u32 = 30_000;
```

Replace the entire `Some(envelope::Payload::ActionRequest(req)) => { ... }` arm (currently `src/ipc/protocol.rs:385-431`) with:

```rust
            Some(envelope::Payload::ActionRequest(req)) => {
                let action_start = Instant::now();
                let action_id = req.action_id.clone();
                let sender_id = registry
                    .get_by_conn_id(msg.conn_id)
                    .map(|e| e.plugin_id.clone())
                    .unwrap_or_default();

                // R5-07 (option b): route to a plugin that declared this action in
                // its manifest — "declared it" is the entire authorization model,
                // no extra permission check. Ambiguous declarations (>1 provider)
                // are refused rather than arbitrarily resolved.
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
                    ActionLookup::Found(provider) => {
                        let internal_id = format!(
                            "kact-{}",
                            ACTION_CORRELATION_SEQ.fetch_add(1, Ordering::Relaxed)
                        );
                        let effective_timeout_ms = if req.timeout_ms == 0 {
                            DEFAULT_ACTION_TIMEOUT_MS
                        } else {
                            req.timeout_ms
                        };
                        registry.register_pending_action(
                            internal_id.clone(),
                            PendingAction {
                                requester_write_tx: msg.write_tx.clone(),
                                original_action_id: action_id.clone(),
                                requester_id: sender_id.clone(),
                                deadline: Instant::now()
                                    + Duration::from_millis(effective_timeout_ms as u64),
                            },
                        );

                        let forwarded = Envelope {
                            payload: Some(envelope::Payload::ActionRequest(ActionRequest {
                                action_id: internal_id,
                                action: req.action.clone(),
                                params_json: req.params_json.clone(),
                                timeout_ms: req.timeout_ms,
                            })),
                            ..Default::default()
                        };
                        Self::send_envelope(&provider.write_tx, forwarded).await;
                        None
                    }
                };

                if let Some(status) = not_found_status {
                    let response = Envelope {
                        payload: Some(envelope::Payload::ActionResponse(ActionResponse {
                            action_id,
                            status: status as i32,
                            data_json: vec![],
                            error: format!("{:?}", status),
                        })),
                        ..Default::default()
                    };
                    Self::send_envelope(&msg.write_tx, response).await;
                }
                histogram!("action_request_duration_ms")
                    .record(action_start.elapsed().as_millis() as f64);
                false
            }

            Some(envelope::Payload::ActionResponse(resp)) => {
                // A provider plugin answering a kernel-routed ActionRequest always
                // targets "kernel" (it doesn't know who really asked) — this is
                // where the kernel translates the internal correlation id back to
                // the original requester's action_id and proxies the response.
                match registry.take_pending_action(&resp.action_id) {
                    Some(pending) => {
                        let response = Envelope {
                            payload: Some(envelope::Payload::ActionResponse(ActionResponse {
                                action_id: pending.original_action_id,
                                status: resp.status,
                                data_json: resp.data_json,
                                error: resp.error,
                            })),
                            ..Default::default()
                        };
                        Self::send_envelope(&pending.requester_write_tx, response).await;
                    }
                    None => {
                        warn!(
                            action_id = %resp.action_id,
                            "action response with no matching pending request (late, duplicate, or already timed out), dropping"
                        );
                    }
                }
                false
            }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test integration kernel_routes_action_to_declared_provider -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Confirm the existing not-found regression test still passes**

Run: `cargo test --test integration kernel_targeted_action_request_returns_not_found_not_fake_ok -- --nocapture`
Expected: PASS unchanged (`get_cpu` still has no provider).

- [ ] **Step 6: Commit**

```bash
git add src/ipc/protocol.rs tests/integration/test_kernel_commands.rs
git commit -m "feat: route kernel-targeted ActionRequest to declared provider plugin (R5-07)"
```

---

### Task 4: Ambiguous-provider integration test

**Files:**
- Test: `tests/integration/test_kernel_commands.rs`

**Interfaces:**
- Consumes: routing behavior from Task 3 (already implemented — this task only adds test coverage for the `ActionLookup::Ambiguous` branch, which Task 3's implementation already handles).

- [ ] **Step 1: Write the test**

Add to `tests/integration/test_kernel_commands.rs`:

```rust
#[tokio::test]
async fn kernel_action_with_ambiguous_providers_returns_not_found() {
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_action_ambiguous.sock", 19218).await;

    for id in ["dup-provider-a", "dup-provider-b"] {
        let mut provider = VeyronClient::connect("/tmp/veyron_integ_action_ambiguous.sock")
            .await
            .unwrap();
        provider
            .register(
                id,
                PluginManifest {
                    actions: vec!["get_weather".to_string()],
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // Keep the connection alive for the duration of the test.
        std::mem::forget(provider);
    }

    let mut requester = VeyronClient::connect("/tmp/veyron_integ_action_ambiguous.sock")
        .await
        .unwrap();
    requester
        .register("ambiguous-requester", PluginManifest::default())
        .await
        .unwrap();

    let resp = timeout(
        Duration::from_secs(2),
        requester.send_action("get_weather", b"{}", 2000),
    )
    .await
    .expect("timed out")
    .expect("send_action failed");

    assert_eq!(resp.status, ActionStatus::ActionNotFound as i32);

    let _ = shutdown_tx.send(());
}
```

`std::mem::forget(provider)` intentionally leaks the client so its connection (and therefore its registration) stays alive for the test's duration without needing a variable name per provider — the harness's own `shutdown_tx` tears down the whole kernel process at the end regardless.

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test --test integration kernel_action_with_ambiguous_providers -- --nocapture`
Expected: PASS (Task 3's `ActionLookup::Ambiguous` branch already returns `ActionNotFound`).

- [ ] **Step 3: Commit**

```bash
git add tests/integration/test_kernel_commands.rs
git commit -m "test: ambiguous action provider declaration returns ACTION_NOT_FOUND (R5-07)"
```

---

### Task 5: Provider-side failure proxies through unchanged

**Files:**
- Test: `tests/integration/test_kernel_commands.rs`

**Interfaces:**
- Consumes: routing/proxy behavior from Task 3 (already implemented — this task verifies the proxy does not reinterpret non-OK statuses).

- [ ] **Step 1: Write the test**

Add to `tests/integration/test_kernel_commands.rs`:

```rust
#[tokio::test]
async fn kernel_proxies_provider_action_error_unchanged() {
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_action_error.sock", 19219).await;

    let mut provider = VeyronClient::connect("/tmp/veyron_integ_action_error.sock")
        .await
        .unwrap();
    provider
        .register(
            "flaky-provider",
            PluginManifest {
                actions: vec!["flaky_action".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let mut requester = VeyronClient::connect("/tmp/veyron_integ_action_error.sock")
        .await
        .unwrap();
    requester
        .register("flaky-requester", PluginManifest::default())
        .await
        .unwrap();

    let request_fut = requester.send_action("flaky_action", b"{}", 2000);

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
                status: ActionStatus::ActionError as i32,
                data_json: vec![],
                error: "upstream API unreachable".to_string(),
            },
        )),
        ..Default::default()
    };
    provider.send("kernel", resp_env).await.unwrap();

    let resp = timeout(Duration::from_secs(2), request_fut)
        .await
        .expect("timed out")
        .expect("send_action failed");

    assert_eq!(resp.status, ActionStatus::ActionError as i32);
    assert_eq!(resp.error, "upstream API unreachable");

    let _ = shutdown_tx.send(());
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test --test integration kernel_proxies_provider_action_error_unchanged -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/integration/test_kernel_commands.rs
git commit -m "test: provider action failures proxy through kernel unchanged (R5-07)"
```

---

### Task 6: Wire timeout sweep into the router's periodic tick

**Files:**
- Modify: `src/ipc/protocol.rs:100-113` (the `prune_tick.tick()` branch inside `run_with_context`'s `select!`)

**Interfaces:**
- Consumes: `PluginRegistry::sweep_expired_actions` (Task 2), `MessageRouter::send_envelope` (existing).

- [ ] **Step 1: Update the `prune_tick` branch**

In `src/ipc/protocol.rs`, inside `run_with_context`'s main loop, replace:

```rust
                _ = prune_tick.tick() => {
                    if let Some(limiter) = &ipc_limiter {
                        limiter.retain_recent();
                    }
                    continue;
                }
```

with:

```rust
                _ = prune_tick.tick() => {
                    if let Some(limiter) = &ipc_limiter {
                        limiter.retain_recent();
                    }
                    for pending in registry.sweep_expired_actions(Instant::now()) {
                        let response = Envelope {
                            payload: Some(envelope::Payload::ActionResponse(ActionResponse {
                                action_id: pending.original_action_id,
                                status: ActionStatus::ActionTimeout as i32,
                                data_json: vec![],
                                error: "action timed out".to_string(),
                            })),
                            ..Default::default()
                        };
                        Self::send_envelope(&pending.requester_write_tx, response).await;
                    }
                    continue;
                }
```

There is intentionally no new integration test here that waits out a real timeout: the tick is 60s, and `PluginRegistry::sweep_expired_actions`'s eviction logic is already unit-tested with synthetic `Instant`s in Task 2 (`sweep_expired_actions_evicts_past_deadline_only`). This step only wires that already-correct function into the existing tick — verified by the build and full suite passing, not a new slow test.

- [ ] **Step 2: Run the full test suite to confirm no regressions**

Run: `cargo test --all --all-features`
Expected: all tests pass (no new failures introduced by this wiring change).

- [ ] **Step 3: Commit**

```bash
git add src/ipc/protocol.rs
git commit -m "feat: sweep expired pending actions on the router's periodic tick (R5-07)"
```

---

### Task 7: Retire `action_to_permission`

**Files:**
- Modify: `src/auth/permissions.rs`
- Modify: `tests/unit/test_permissions.rs`

**Interfaces:**
- None — this is pure removal. Task 3 already dropped the only production call site (`src/ipc/protocol.rs`'s old `ActionRequest` arm), so `action_to_permission` is now dead code with no callers anywhere in `src/`.

- [ ] **Step 1: Confirm there are no remaining callers**

Run: `grep -rn "action_to_permission" src/`
Expected: only the definition in `src/auth/permissions.rs` (no call sites — Task 3 removed the last one from `protocol.rs`'s import and usage).

- [ ] **Step 2: Delete the function**

In `src/auth/permissions.rs`, delete:

```rust
pub fn action_to_permission(action: &str) -> Option<PermissionType> {
    match action {
        "http_get" | "http_post" | "http_put" | "http_delete" | "http_patch" => {
            Some(PermissionType::PermissionNetwork)
        }
        "read_file" | "list_dir" => Some(PermissionType::PermissionFilesRead),
        "write_file" | "delete_file" => Some(PermissionType::PermissionFilesWrite),
        "get_cpu" | "get_memory" | "get_disk" => Some(PermissionType::PermissionSystem),
        "play_audio" | "record_audio" => Some(PermissionType::PermissionAudio),
        "send_notification" => Some(PermissionType::PermissionNotify),
        "set_timer" | "create_alarm" => Some(PermissionType::PermissionScheduler),
        "browser_navigate" | "browser_screenshot" => Some(PermissionType::PermissionBrowser),
        _ => None,
    }
}
```

- [ ] **Step 3: Delete its tests and fix the import**

In `tests/unit/test_permissions.rs`, change the import line:

```rust
use veyron::auth::permissions::{action_to_permission, check_ipc_target, check_permission};
```

to:

```rust
use veyron::auth::permissions::{check_ipc_target, check_permission};
```

Delete these four test functions:

```rust
#[test]
fn action_http_get_maps_to_network_permission() {
    assert_eq!(
        action_to_permission("http_get"),
        Some(PermissionType::PermissionNetwork)
    );
}

#[test]
fn action_read_file_maps_to_files_read() {
    assert_eq!(
        action_to_permission("read_file"),
        Some(PermissionType::PermissionFilesRead)
    );
}

#[test]
fn action_write_file_maps_to_files_write() {
    assert_eq!(
        action_to_permission("write_file"),
        Some(PermissionType::PermissionFilesWrite)
    );
}

#[test]
fn unknown_action_maps_to_none() {
    assert_eq!(action_to_permission("fly_to_moon"), None);
}
```

- [ ] **Step 4: Run the full test suite and clippy**

Run: `cargo test --all --all-features`
Expected: all pass, no failures from the removed tests (they're gone, not failing).

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean — no dead_code warning for the deleted function (it's gone, not just unused).

- [ ] **Step 5: Commit**

```bash
git add src/auth/permissions.rs tests/unit/test_permissions.rs
git commit -m "chore: retire action_to_permission, superseded by provider-declared routing (R5-07)"
```

---

### Task 8: Update `ROADMAP.md` to mark R5-07 done

**Files:**
- Modify: `ROADMAP.md`

- [ ] **Step 1: Update the R5-07 entry**

Change the `### R5-07 ◐ — ... — IN PROGRESS` heading to `### R5-07 ✓ — ...` (drop the `— IN PROGRESS` suffix, matching the `✓` convention used by every other completed item in this file, e.g. R5-04/R5-05/R5-06 above it and R5-08 through R5-16 below it).

Add a `**Done:**` paragraph after the existing `**Effort:**` line (following the file's established pattern — see R5-06's `**Done:**` paragraph for the level of detail expected), summarizing: provider lookup + ambiguity handling (Task 1), pending-action correlation table (Task 2), routing + response proxy (Task 3), timeout sweep on the existing `prune_tick` (Task 6), `action_to_permission` retirement (Task 7), and the test names added (Tasks 3–5).

Update the `Current baseline` test count near the top of the file (`| Tests | ... 263 passing ...`) to the new total — run `cargo test --all --all-features 2>&1 | tail -20` and copy the final passing count.

- [ ] **Step 2: Commit**

```bash
git add ROADMAP.md
git commit -m "docs: mark R5-07 action routing done (AUDIT H-05)"
```

---

## Self-Review Notes

- **Spec coverage:** provider lookup (Task 1) ✓, ambiguity → `ACTION_NOT_FOUND` + warn (Task 1 impl / Task 4 test) ✓, correlation via kernel-minted id (Task 2/3) ✓, provider-declared = sole authorization / no new permission (Task 3, `action_to_permission` retired in Task 7) ✓, provider errors proxy unchanged (Task 3 impl / Task 5 test) ✓, timeout sweep on existing tick (Task 6) ✓, disconnect edge cases (handled by construction per spec — no dedicated task, covered by existing `send_envelope`/timeout-sweep tolerance, called out in Task 6) ✓, regression test for the interim `ACTION_NOT_FOUND` behavior stays green (Task 3 Step 5) ✓.
- **No placeholders:** every step has complete code, exact file paths, and exact `cargo test`/`grep` commands with expected output.
- **Type consistency:** `ActionLookup`, `PendingAction`, `find_action_provider`, `register_pending_action`, `take_pending_action`, `sweep_expired_actions` are named and typed identically everywhere they're introduced (Tasks 1–2) and consumed (Task 3, Task 6).
