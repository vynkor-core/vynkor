use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;
use veyron::api::server::create_router;
use veyron::metrics::init_metrics;
use veyron::plugins::manager::PluginManager;
use veyron::plugins::registry::PluginRegistry;
use veyron::plugins::supervisor::PluginSupervisor;

fn make_app() -> axum::Router {
    init_metrics();
    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_test_metrics.sock"));
    let reg = Arc::new(PluginRegistry::new());
    let mgr = Arc::new(PluginManager::new(sup, reg));
    create_router(mgr, None)
}

async fn get_metrics_response() -> axum::response::Response {
    make_app()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn metrics_endpoint_returns_200() {
    let resp = get_metrics_response().await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn metrics_endpoint_has_prometheus_content_type() {
    let resp = get_metrics_response().await;
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("text/plain"),
        "content-type must contain text/plain, got: {ct}"
    );
    assert!(
        ct.contains("version=0.0.4"),
        "content-type must include Prometheus version, got: {ct}"
    );
}

#[tokio::test]
async fn metrics_endpoint_body_is_valid_utf8() {
    let resp = get_metrics_response().await;
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(
        String::from_utf8(bytes.to_vec()).is_ok(),
        "/metrics body must be valid UTF-8"
    );
}

#[tokio::test]
async fn metrics_endpoint_body_is_empty_or_prometheus_format() {
    let resp = get_metrics_response().await;
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    // Empty is valid (no counters incremented yet in this process). If non-empty,
    // each non-blank, non-comment line must match `name{...} value` or `name value`.
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        assert!(
            trimmed.contains(' '),
            "non-comment metric line must have a value: {trimmed}"
        );
    }
}
