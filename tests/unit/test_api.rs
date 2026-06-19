use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tokio::sync::mpsc;
use tower::ServiceExt;
use veyron::api::server::create_router;
use veyron::ipc::framing::Frame;
use veyron::plugins::registry::PluginRegistry;
use veyron::proto::veyron::PluginManifest;

fn make_registry() -> Arc<PluginRegistry> {
    Arc::new(PluginRegistry::new())
}

fn register(registry: &PluginRegistry, plugin_id: &str, conn_id: u64) {
    let (tx, _rx) = mpsc::channel::<Frame>(1);
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
    let app = create_router(make_registry());
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
async fn plugins_returns_empty_array_when_no_plugins() {
    let app = create_router(make_registry());
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

    let app = create_router(registry);
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

    let app = create_router(registry);
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
    let app = create_router(make_registry());
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

    let app = create_router(Arc::clone(&registry));
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
    let app = create_router(make_registry());
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
