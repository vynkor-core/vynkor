use crate::jwt_helper::create_test_token;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tower::ServiceExt;
use vynkor::api::server::{create_router, create_router_full, RouterConfig};
use vynkor::auth::jwt::JwtValidator;
use vynkor::plugins::manager::PluginManager;
use vynkor::plugins::registry::PluginRegistry;
use vynkor::plugins::supervisor::PluginSupervisor;
use vynkor::proto::vynkor::PluginManifest;
use vynkor::utils::config::PluginDef;

fn make_registry() -> Arc<PluginRegistry> {
    Arc::new(PluginRegistry::new())
}

fn make_supervisor() -> Arc<PluginSupervisor> {
    Arc::new(PluginSupervisor::new("/tmp/vynkor_test.sock"))
}

fn make_manager(
    registry: Arc<PluginRegistry>,
    supervisor: Arc<PluginSupervisor>,
) -> Arc<PluginManager> {
    Arc::new(PluginManager::new(supervisor, registry))
}

fn register(registry: &PluginRegistry, plugin_id: &str, conn_id: u64) {
    let (tx, _rx) = mpsc::channel::<vynkor::ipc::connection::Outbound>(1);
    registry
        .register(
            plugin_id.to_string(),
            conn_id,
            PluginManifest::default(),
            tx,
            "",
            "",
        )
        .unwrap();
}

// D-04: register like a remote device agent would off the wire
fn register_with_device(registry: &PluginRegistry, plugin_id: &str, conn_id: u64) {
    let (tx, _rx) = mpsc::channel::<vynkor::ipc::connection::Outbound>(1);
    registry
        .register_with_device(
            plugin_id.to_string(),
            conn_id,
            PluginManifest::default(),
            tx,
            vynkor::plugins::registry::DeviceMeta {
                device_id: "phone-1".to_string(),
                user_id: "default".to_string(),
                os: vynkor::proto::vynkor::DeviceOs::Android,
                arch: "aarch64".to_string(),
                os_version: "14".to_string(),
                capabilities: vec!["geo".to_string(), "battery".to_string()],
            },
        )
        .unwrap();
}

async fn body_string(body: axum::body::Body) -> String {
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

// ── tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn health_returns_ok() {
    let app = create_router(make_manager(make_registry(), make_supervisor()), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn health_reports_status_version_and_plugin_count() {
    let registry = make_registry();
    register(&registry, "alpha", 1);
    register(&registry, "beta", 2);

    let app = create_router(make_manager(registry, make_supervisor()), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_string(response.into_body()).await;
    assert!(body.contains("\"status\":\"ok\""), "body: {body}");
    assert!(body.contains("\"plugins\":2"), "body: {body}");
    assert!(
        body.contains(&format!("\"version\":\"{}\"", env!("CARGO_PKG_VERSION"))),
        "body: {body}"
    );
    assert!(body.contains("\"uptime_secs\":"), "body: {body}");
}

#[tokio::test]
async fn plugins_returns_empty_array_when_no_plugins() {
    let app = create_router(make_manager(make_registry(), make_supervisor()), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/plugins")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response.into_body()).await;
    assert_eq!(body.trim(), "[]");
}

#[tokio::test]
async fn plugins_returns_registered_plugins() {
    let registry = make_registry();
    register(&registry, "weather", 1);
    register(&registry, "timer", 2);

    let app = create_router(make_manager(registry, make_supervisor()), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/plugins")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response.into_body()).await;
    assert!(body.contains("weather"), "body: {body}");
    assert!(body.contains("timer"), "body: {body}");
    // UX-2: stable lowercase string, never a Rust Debug enum name
    assert!(body.contains("\"state\":\"registered\""), "body: {body}");
}

#[tokio::test]
async fn get_plugin_by_id_returns_plugin() {
    let registry = make_registry();
    register(&registry, "echo", 1);

    let app = create_router(make_manager(registry, make_supervisor()), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/plugins/echo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response.into_body()).await;
    assert!(body.contains("echo"), "body: {body}");
    // UX-2: stable lowercase string, never a Rust Debug enum name
    assert!(body.contains("\"state\":\"registered\""), "body: {body}");
}

// ── devices (D-04) ─────────────────────────────────────────────────────────

#[tokio::test]
async fn devices_returns_empty_array_when_no_devices() {
    let app = create_router(make_manager(make_registry(), make_supervisor()), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/devices")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_string(response.into_body()).await.trim(), "[]");
}

#[tokio::test]
async fn devices_returns_registered_device_fields() {
    let registry = make_registry();
    register_with_device(&registry, "geo", 1);

    let app = create_router(make_manager(registry, make_supervisor()), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/devices")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response.into_body()).await;
    assert!(body.contains("\"device_id\":\"phone-1\""), "body: {body}");
    assert!(body.contains("\"os\":\"android\""), "body: {body}");
    assert!(body.contains("\"arch\":\"aarch64\""), "body: {body}");
    assert!(body.contains("\"os_version\":\"14\""), "body: {body}");
    assert!(
        body.contains("\"capabilities\":[\"geo\",\"battery\"]"),
        "body: {body}"
    );
    assert!(body.contains("\"state\":\"online\""), "body: {body}");
    assert!(body.contains("\"last_seen\":"), "body: {body}");
}

#[tokio::test]
async fn plugins_include_device_id_and_last_seen() {
    let registry = make_registry();
    register_with_device(&registry, "geo", 1);

    let app = create_router(make_manager(registry, make_supervisor()), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/plugins")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response.into_body()).await;
    assert!(body.contains("\"device_id\":\"phone-1\""), "body: {body}");
    assert!(body.contains("\"last_seen\":"), "body: {body}");
}

#[tokio::test]
async fn devices_requires_auth_when_jwt_set() {
    const SECRET: &[u8] = b"test-secret";
    let validator = Arc::new(JwtValidator::new(SECRET));

    // No token → 401
    let app = create_router(
        make_manager(make_registry(), make_supervisor()),
        Some(validator.clone()),
    );
    let res = app
        .oneshot(
            Request::builder()
                .uri("/devices")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Valid token → 200
    let token = create_test_token("admin", vec![], SECRET, 3600);
    let res2 = create_router(
        make_manager(make_registry(), make_supervisor()),
        Some(validator),
    )
    .oneshot(
        Request::builder()
            .uri("/devices")
            .header("Authorization", format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
}

#[tokio::test]
async fn get_plugin_by_id_returns_404_for_unknown() {
    let app = create_router(make_manager(make_registry(), make_supervisor()), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/plugins/ghost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stop_plugin_returns_200_and_unregisters() {
    let registry = make_registry();
    register(&registry, "stoppable", 1);
    assert!(registry.get("stoppable").is_some());

    let app = create_router(make_manager(Arc::clone(&registry), make_supervisor()), None);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/stoppable/stop")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        registry.get("stoppable").is_none(),
        "plugin must be unregistered after stop"
    );
}

#[tokio::test]
async fn stop_nonexistent_plugin_returns_404() {
    let app = create_router(make_manager(make_registry(), make_supervisor()), None);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/ghost/stop")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn restart_nonexistent_plugin_returns_404() {
    let app = create_router(make_manager(make_registry(), make_supervisor()), None);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/ghost/restart")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn restart_plugin_not_in_supervisor_returns_422() {
    let registry = make_registry();
    register(&registry, "self-connected", 1);

    let app = create_router(make_manager(Arc::clone(&registry), make_supervisor()), None);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/self-connected/restart")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Plugin exists in registry but not in supervisor (connected on its own)
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn post_endpoint_requires_auth_when_jwt_secret_set() {
    const SECRET: &[u8] = b"test-secret";
    let validator = Arc::new(JwtValidator::new(SECRET));
    let registry = make_registry();
    register(&registry, "guarded", 1);

    let app = create_router(
        make_manager(Arc::clone(&registry), make_supervisor()),
        Some(validator.clone()),
    );

    // No token → 401
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/guarded/stop")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Valid token with PERMISSION_KERNEL_ADMIN → 200
    let token = create_test_token(
        "admin",
        vec!["PERMISSION_KERNEL_ADMIN".to_string()],
        SECRET,
        3600,
    );
    let registry2 = make_registry();
    register(&registry2, "guarded", 1);
    let app2 = create_router(make_manager(registry2, make_supervisor()), Some(validator));
    let res2 = app2
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/guarded/stop")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
}

#[tokio::test]
async fn admin_route_rejects_valid_token_lacking_kernel_admin_permission() {
    // T-01: auth_middleware only checked JWT validity, never claims.permissions.
    // Any valid JWT — even one scoped to an unrelated permission — could
    // start/stop/restart any plugin. Must be gated on PERMISSION_KERNEL_ADMIN,
    // mirroring the IPC KernelCommand check (src/ipc/protocol.rs:536).
    const SECRET: &[u8] = b"test-secret";
    let validator = Arc::new(JwtValidator::new(SECRET));
    let registry = make_registry();
    register(&registry, "guarded", 1);

    let app = create_router(
        make_manager(Arc::clone(&registry), make_supervisor()),
        Some(validator),
    );

    let token = create_test_token(
        "low-priv",
        vec!["PERMISSION_NETWORK".to_string()],
        SECRET,
        3600,
    );
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/guarded/stop")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(
        registry.get("guarded").is_some(),
        "plugin must not be stopped by an unprivileged token"
    );
}

#[tokio::test]
async fn admin_route_allows_token_with_kernel_admin_permission() {
    const SECRET: &[u8] = b"test-secret";
    let validator = Arc::new(JwtValidator::new(SECRET));
    let registry = make_registry();
    register(&registry, "guarded", 1);

    let app = create_router(
        make_manager(Arc::clone(&registry), make_supervisor()),
        Some(validator),
    );

    let token = create_test_token(
        "admin",
        vec!["PERMISSION_KERNEL_ADMIN".to_string()],
        SECRET,
        3600,
    );
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/guarded/stop")
                .header("Authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn logs_endpoint_requires_auth_when_jwt_secret_set() {
    const SECRET: &[u8] = b"test-secret";
    let validator = Arc::new(JwtValidator::new(SECRET));
    let registry = make_registry();
    register(&registry, "guarded", 1);

    let app = create_router(
        make_manager(Arc::clone(&registry), make_supervisor()),
        Some(validator),
    );

    // logs can leak sensitive output — must be rejected without a token
    let res = app
        .oneshot(
            Request::builder()
                .uri("/plugins/guarded/logs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logs_endpoint_clamps_huge_lines_param_instead_of_erroring() {
    let registry = make_registry();
    register(&registry, "guarded", 1);
    let app = create_router(make_manager(registry, make_supervisor()), None);

    // T-10: `lines` was bounded only incidentally by the ring buffer's own
    // capacity. A caller-supplied `usize::MAX` must still be accepted (clamped
    // server-side), not rejected or used to drive an oversized allocation.
    let res = app
        .oneshot(
            Request::builder()
                .uri(format!("/plugins/guarded/logs?lines={}", usize::MAX))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_string(res.into_body()).await, "[]");
}

#[tokio::test]
async fn read_only_endpoints_open_without_token() {
    const SECRET: &[u8] = b"test-secret";
    let validator = Arc::new(JwtValidator::new(SECRET));
    let app = create_router(
        make_manager(make_registry(), make_supervisor()),
        Some(validator),
    );

    let res = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn get_plugins_requires_auth_when_jwt_set() {
    const SECRET: &[u8] = b"test-secret";
    let validator = Arc::new(JwtValidator::new(SECRET));
    let app = create_router(
        make_manager(make_registry(), make_supervisor()),
        Some(validator.clone()),
    );

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
    let res2 = create_router(
        make_manager(make_registry(), make_supervisor()),
        Some(validator),
    )
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
async fn rate_limit_applies_only_to_verified_sub_not_forged_tokens() {
    // BUG-004 regression: rate limiting must run after JWT signature
    // verification and key on the verified sub, not a self-decoded field an
    // attacker can rotate to dodge the limit or forge to burn someone else's
    // quota.
    const SECRET: &[u8] = b"test-secret";
    let validator = Arc::new(JwtValidator::new(SECRET));
    let app = create_router_full(RouterConfig {
        manager: make_manager(make_registry(), make_supervisor()),
        device_store: None,
        jwt_validator: Some(validator),
        ws_router_tx: None,
        ws_disconnect_tx: None,
        started_at: Instant::now(),
        rate_limit_rps: Some(1),   // 1 rps
        rate_limit_burst: Some(1), // burst of 1
        plugin_defs: vec![],
        ws_handshake_timeout_secs: 5,
        max_ws_connections: 1024,
        ws_register_timeout_secs: 10,
    })
    .app;

    // Forged tokens signed with the wrong secret, rotating `sub` every request,
    // must never bypass auth to reach the rate limiter — always 401.
    for sub in ["victim-a", "victim-b", "victim-c", "victim-d", "victim-e"] {
        let forged = create_test_token(sub, vec![], b"wrong-secret", 3600);
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/plugins")
                    .header("Authorization", format!("Bearer {}", forged))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "forged token must never bypass auth regardless of sub"
        );
    }
}

#[tokio::test]
async fn rate_limit_enforced_per_verified_sub() {
    const SECRET: &[u8] = b"test-secret";
    let validator = Arc::new(JwtValidator::new(SECRET));
    let app = create_router_full(RouterConfig {
        manager: make_manager(make_registry(), make_supervisor()),
        device_store: None,
        jwt_validator: Some(validator),
        ws_router_tx: None,
        ws_disconnect_tx: None,
        started_at: Instant::now(),
        rate_limit_rps: Some(1),   // 1 rps
        rate_limit_burst: Some(1), // burst of 1
        plugin_defs: vec![],
        ws_handshake_timeout_secs: 5,
        max_ws_connections: 1024,
        ws_register_timeout_secs: 10,
    })
    .app;

    let token = create_test_token("admin", vec![], SECRET, 3600);
    let request = || {
        Request::builder()
            .uri("/plugins")
            .header("Authorization", format!("Bearer {}", token))
            .body(Body::empty())
            .unwrap()
    };

    let res1 = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(res1.status(), StatusCode::OK);

    let res2 = app.oneshot(request()).await.unwrap();
    assert_eq!(
        res2.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "second request within the same window must be throttled for a verified sub"
    );
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
    let app = create_router(
        make_manager(make_registry(), make_supervisor()),
        Some(validator.clone()),
    );

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
    let res2 = create_router(
        make_manager(make_registry(), make_supervisor()),
        Some(validator),
    )
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

fn sleep_def(id: &str) -> PluginDef {
    PluginDef {
        id: id.to_string(),
        binary: "/bin/sleep".to_string(),
        args: vec!["60".to_string()],
        restart: "never".to_string(),
        ..Default::default()
    }
}

#[tokio::test]
async fn start_plugin_spawns_process_declared_in_config() {
    let registry = make_registry();
    let manager = make_manager(registry, make_supervisor());
    let app = create_router_full(RouterConfig {
        manager: Arc::clone(&manager),
        device_store: None,
        jwt_validator: None,
        ws_router_tx: None,
        ws_disconnect_tx: None,
        started_at: Instant::now(),
        rate_limit_rps: None,
        rate_limit_burst: None,
        plugin_defs: vec![sleep_def("startable")],
        ws_handshake_timeout_secs: 5,
        max_ws_connections: 1024,
        ws_register_timeout_secs: 10,
    })
    .app;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/startable/start")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        manager.is_supervised("startable"),
        "plugin must be supervised after start"
    );
    manager.stop("startable").await.ok();
}

#[tokio::test]
async fn start_unknown_plugin_returns_404() {
    let app = create_router_full(RouterConfig {
        manager: make_manager(make_registry(), make_supervisor()),
        device_store: None,
        jwt_validator: None,
        ws_router_tx: None,
        ws_disconnect_tx: None,
        started_at: Instant::now(),
        rate_limit_rps: None,
        rate_limit_burst: None,
        plugin_defs: vec![sleep_def("declared-elsewhere")],
        ws_handshake_timeout_secs: 5,
        max_ws_connections: 1024,
        ws_register_timeout_secs: 10,
    })
    .app;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/ghost/start")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn start_plugin_rejects_manifest_requesting_ungranted_permission() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("plugin.json"),
        r#"{
            "plugin_id": "restricted",
            "version": "1.0.0",
            "permissions": ["audio_stream"],
            "binary": "restricted",
            "kernel_compatibility_range": {"min": "0.1.0", "max": "*"}
        }"#,
    )
    .unwrap();
    let mut def = sleep_def("restricted");
    def.binary = tmp.path().join("restricted").display().to_string();
    // Operator's config.yaml grants no permissions — manifest requests audio_stream.
    def.permissions = vec![];
    let def = PluginDef {
        permissions: vec!["network".to_string()],
        ..def
    };

    let app = create_router_full(RouterConfig {
        manager: make_manager(make_registry(), make_supervisor()),
        device_store: None,
        jwt_validator: None,
        ws_router_tx: None,
        ws_disconnect_tx: None,
        started_at: Instant::now(),
        rate_limit_rps: None,
        rate_limit_burst: None,
        plugin_defs: vec![def],
        ws_handshake_timeout_secs: 5,
        max_ws_connections: 1024,
        ws_register_timeout_secs: 10,
    })
    .app;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/restricted/start")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn start_already_running_plugin_returns_conflict() {
    let registry = make_registry();
    let manager = make_manager(registry, make_supervisor());
    manager
        .start(vynkor::plugins::supervisor::PluginConfig {
            plugin_id: "already-up".to_string(),
            binary_path: "/bin/sleep".into(),
            args: vec!["60".to_string()],
            restart_policy: vynkor::plugins::supervisor::RestartPolicy::Never,
            ..Default::default()
        })
        .await
        .unwrap();

    let app = create_router_full(RouterConfig {
        manager: Arc::clone(&manager),
        device_store: None,
        jwt_validator: None,
        ws_router_tx: None,
        ws_disconnect_tx: None,
        started_at: Instant::now(),
        rate_limit_rps: None,
        rate_limit_burst: None,
        plugin_defs: vec![sleep_def("already-up")],
        ws_handshake_timeout_secs: 5,
        max_ws_connections: 1024,
        ws_register_timeout_secs: 10,
    })
    .app;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/already-up/start")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    manager.stop("already-up").await.ok();
}
