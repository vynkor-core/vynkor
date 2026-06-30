# VULN-008: HTTP Control Plane Auth Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When `jwt_secret` is configured, all HTTP endpoints except `/health` require a valid `Authorization: Bearer <jwt>` token; unauthenticated enumeration of `/plugins`, `/plugins/:id`, and `/metrics` is blocked.

**Architecture:** Move `GET /plugins`, `GET /plugins/:id`, and `GET /metrics` from the `public` router into the existing `protected` router in `server.rs`. The `auth_middleware` already short-circuits to `next.run` when `jwt_validator` is `None`, so `allow_no_auth` deployments see zero change. `/health` stays public — it is a standard liveness probe.

**Tech Stack:** Rust, Axum, existing `auth_middleware` + `JwtValidator` from `src/api/middleware.rs` / `src/auth/jwt.rs`

## Global Constraints

- Only `src/api/server.rs` and `tests/unit/test_api.rs` change — zero other files
- `/health` MUST remain open unconditionally (liveness probe, no token required even when jwt_secret is set)
- When `jwt_validator` is `None` (`allow_no_auth: true`), all endpoints must still return 200 without a token (existing behaviour preserved)
- `cargo test --all` must pass; `cargo clippy -- -D warnings` must pass; `cargo fmt --check` must pass

---

## File Map

- **Modify:** `src/api/server.rs` lines 47–66 — restructure `public`/`protected` router split
- **Modify:** `tests/unit/test_api.rs` — add 3 new auth-gate tests; update `read_only_endpoints_open_without_token` name/assertion to reflect `/health`-only public surface

---

## Task 1: Write failing auth tests for read-only endpoints

**Files:**
- Modify: `tests/unit/test_api.rs`

**Interfaces:**
- Consumes: `create_router(manager, Some(validator))` — existing function, unchanged signature
- Consumes: `create_test_token(sub, perms, secret, exp_secs)` — returns `String`
- Produces: 3 new `#[tokio::test]` functions that FAIL until Task 2 is implemented:
  - `get_plugins_requires_auth_when_jwt_set`
  - `get_plugin_by_id_requires_auth_when_jwt_set`
  - `get_metrics_requires_auth_when_jwt_set`

- [ ] **Step 1: Add the three failing tests to `tests/unit/test_api.rs`**

Append after the last test in the file (after the closing `}` of `read_only_endpoints_open_without_token`):

```rust
#[tokio::test]
async fn get_plugins_requires_auth_when_jwt_set() {
    const SECRET: &[u8] = b"test-secret";
    let validator = Arc::new(JwtValidator::new(SECRET));
    let app = create_router(make_manager(make_registry(), make_supervisor()), Some(validator.clone()));

    // No token → 401
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/plugins")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Valid token → 200
    let token = create_test_token("admin", vec![], SECRET, 3600);
    let res2 = create_router(make_manager(make_registry(), make_supervisor()), Some(validator))
        .oneshot(
            Request::builder()
                .uri("/plugins")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
}

#[tokio::test]
async fn get_plugin_by_id_requires_auth_when_jwt_set() {
    const SECRET: &[u8] = b"test-secret";
    let validator = Arc::new(JwtValidator::new(SECRET));
    let registry = make_registry();
    register(&registry, "echo", 1);
    let app = create_router(
        make_manager(Arc::clone(&registry), make_supervisor()),
        Some(validator.clone()),
    );

    // No token → 401
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/plugins/echo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Valid token → 200
    let token = create_test_token("admin", vec![], SECRET, 3600);
    let registry2 = make_registry();
    register(&registry2, "echo", 1);
    let res2 = create_router(make_manager(registry2, make_supervisor()), Some(validator))
        .oneshot(
            Request::builder()
                .uri("/plugins/echo")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
}

#[tokio::test]
async fn get_metrics_requires_auth_when_jwt_set() {
    const SECRET: &[u8] = b"test-secret";
    let validator = Arc::new(JwtValidator::new(SECRET));
    let app = create_router(make_manager(make_registry(), make_supervisor()), Some(validator.clone()));

    // No token → 401
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Valid token → 200
    let token = create_test_token("admin", vec![], SECRET, 3600);
    let res2 = create_router(make_manager(make_registry(), make_supervisor()), Some(validator))
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
}
```

- [ ] **Step 2: Run the new tests — expect FAIL**

```bash
cargo test -p veyron --test '*' get_plugins_requires_auth_when_jwt_set get_plugin_by_id_requires_auth_when_jwt_set get_metrics_requires_auth_when_jwt_set 2>&1 | tail -20
```

Expected: 3 FAILED (assertions on `UNAUTHORIZED` fail because routes are still public).

- [ ] **Step 3: Commit the failing tests**

```bash
git add tests/unit/test_api.rs
git commit -m "test(api): add failing auth-gate tests for GET /plugins, /plugins/:id, /metrics (VULN-008)"
```

---

## Task 2: Move read-only endpoints into the protected router

**Files:**
- Modify: `src/api/server.rs` lines 47–66

**Interfaces:**
- Consumes: `auth_middleware` from `crate::api::middleware` — unchanged
- Produces: restructured `create_router_full` where `public` contains only `/health` and `protected` contains `/metrics`, `GET /plugins`, `GET /plugins/:id`, `/plugins/:id/logs`, `/plugins/:id/stop`, `/plugins/:id/restart`

- [ ] **Step 1: Replace the public/protected block in `src/api/server.rs`**

Replace lines 47–66 (the `public` and `protected` router definitions, up to and including `.merge(protected)`):

Old:
```rust
    let public = Router::new()
        .route("/health", get(health_check))
        .route("/metrics", get(get_metrics))
        .route("/plugins", get(list_plugins))
        .route("/plugins/:id", get(get_plugin));

    // Plugin logs can contain sensitive output, and stop/restart mutate state —
    // all require auth when a jwt_secret is configured.
    let protected = Router::new()
        .route("/plugins/:id/logs", get(get_plugin_logs))
        .route("/plugins/:id/stop", post(stop_plugin))
        .route("/plugins/:id/restart", post(restart_plugin))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ));

    let mut app = Router::new()
        .merge(public)
        .merge(protected)
        .with_state(state);
```

New:
```rust
    let public = Router::new().route("/health", get(health_check));

    // All non-health endpoints require auth when jwt_secret is configured.
    // auth_middleware short-circuits to next.run when jwt_validator is None,
    // so allow_no_auth deployments see no change in behaviour.
    let protected = Router::new()
        .route("/metrics", get(get_metrics))
        .route("/plugins", get(list_plugins))
        .route("/plugins/:id", get(get_plugin))
        .route("/plugins/:id/logs", get(get_plugin_logs))
        .route("/plugins/:id/stop", post(stop_plugin))
        .route("/plugins/:id/restart", post(restart_plugin))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ));

    let mut app = Router::new()
        .merge(public)
        .merge(protected)
        .with_state(state);
```

- [ ] **Step 2: Run the three new tests — expect all PASS**

```bash
cargo test -p veyron --test '*' get_plugins_requires_auth_when_jwt_set get_plugin_by_id_requires_auth_when_jwt_set get_metrics_requires_auth_when_jwt_set 2>&1 | tail -10
```

Expected: 3 PASSED.

- [ ] **Step 3: Run the full test suite — expect all PASS**

```bash
cargo test --all 2>&1 | tail -20
```

Expected: all pass. The pre-existing `plugins_returns_*`, `get_plugin_by_id_*` tests use `None` jwt_validator so auth_middleware passes through — they stay green.

- [ ] **Step 4: Check clippy and fmt**

```bash
cargo clippy -- -D warnings 2>&1 | tail -10
cargo fmt --check 2>&1
```

Expected: no output / zero exit code.

- [ ] **Step 5: Commit**

```bash
git add src/api/server.rs tests/unit/test_api.rs
git commit -m "fix(api): gate GET /plugins, /plugins/:id, /metrics behind auth when jwt_secret set — closes VULN-008"
```

---

## Task 3: Close VULN-008 in ROADMAP.md

**Files:**
- Modify: `ROADMAP.md` line 42

**Interfaces:**
- Produces: VULN-008 row status updated from `◐ Mitigated` to `✅ Fixed`

- [ ] **Step 1: Update VULN-008 row in `ROADMAP.md`**

Find line 42:
```
| VULN-008 | Info | HTTP control plane unauthenticated by default | REST endpoints require JWT only when configured | ◐ Mitigated — bound to `127.0.0.1`; enable `jwt_secret` for shared hosts |
```

Replace with:
```
| VULN-008 | Info | HTTP control plane unauthenticated by default | REST endpoints require JWT only when configured | ✅ Fixed — `GET /plugins`, `GET /plugins/:id`, `GET /metrics` moved to protected router; only `/health` remains public; `auth_middleware` passes through when `allow_no_auth: true` |
```

- [ ] **Step 2: Commit**

```bash
git add ROADMAP.md
git commit -m "docs: close VULN-008 in ROADMAP — HTTP read-only endpoints now auth-gated"
```
