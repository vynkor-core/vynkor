use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tower::ServiceExt;
use veyron::api::server::{create_router, create_router_full};
use veyron::auth::jwt::{create_test_token, JwtValidator};
use veyron::plugins::manager::PluginManager;
use veyron::plugins::registry::PluginRegistry;
use veyron::plugins::supervisor::PluginSupervisor;
use veyron::proto::veyron::PluginManifest;

fn make_registry() -> Arc<PluginRegistry> {
    Arc::new(PluginRegistry::new())
}

fn make_supervisor() -> Arc<PluginSupervisor> {
    Arc::new(PluginSupervisor::new("/tmp/veyron_test.sock"))
}

fn make_manager(
    registry: Arc<PluginRegistry>,
    supervisor: Arc<PluginSupervisor>,
) -> Arc<PluginManager> {
    Arc::new(PluginManager::new(supervisor, registry))
}

fn register(registry: &PluginRegistry, plugin_id: &str, conn_id: u64) {
    let (tx, _rx) = mpsc::channel::<veyron::ipc::connection::Outbound>(1);
    registry
        .register(
            plugin_id.to_string(),
            conn_id,
            PluginManifest::default(),
            tx,
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

    // Valid token → 200
    let token = create_test_token("admin", vec![], SECRET, 3600);
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
    let app = create_router_full(
        make_manager(make_registry(), make_supervisor()),
        Some(validator),
        None,
        None,
        Instant::now(),
        Some(1), // 1 rps
        Some(1), // burst of 1
    );

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
    let app = create_router_full(
        make_manager(make_registry(), make_supervisor()),
        Some(validator),
        None,
        None,
        Instant::now(),
        Some(1), // 1 rps
        Some(1), // burst of 1
    );

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
