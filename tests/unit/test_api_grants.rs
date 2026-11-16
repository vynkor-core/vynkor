//! K3: HTTP routes are guarded by the calling device's grants.

use crate::jwt_helper::create_test_token;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use std::time::Instant;
use tower::ServiceExt;
use vynkor::api::server::{create_router_full, RouterConfig};
use vynkor::auth::device_store::DeviceStore;
use vynkor::auth::grants::{Grant, Grants, Preset};
use vynkor::auth::jwt::JwtValidator;
use vynkor::plugins::manager::PluginManager;
use vynkor::plugins::registry::PluginRegistry;
use vynkor::plugins::supervisor::PluginSupervisor;

const SECRET: &[u8] = b"test-secret";
const MASTER: &str = "master-secret-master-secret-master!!";

struct Fx {
    _dir: tempfile::TempDir,
    store: Arc<DeviceStore>,
    app: axum::Router,
}

fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DeviceStore::new(dir.path(), MASTER));
    let manager = Arc::new(PluginManager::new(
        Arc::new(PluginSupervisor::new("/tmp/vynkor_test_grants.sock")),
        Arc::new(PluginRegistry::new()),
    ));
    let app = create_router_full(RouterConfig {
        manager,
        jwt_validator: Some(Arc::new(JwtValidator::new(SECRET))),
        device_store: Some(Arc::clone(&store)),
        pairing: None,
        ws_router_tx: None,
        ws_disconnect_tx: None,
        started_at: Instant::now(),
        rate_limit_rps: None,
        rate_limit_burst: None,
        plugin_defs: vec![],
        ws_handshake_timeout_secs: 5,
        max_ws_connections: 16,
        ws_register_timeout_secs: 10,
        ws_device_recheck_secs: 10,
    })
    .app;
    Fx {
        _dir: dir,
        store,
        app,
    }
}

fn pair(fx: &Fx, id: &str, preset: Preset) -> String {
    fx.store
        .issue(id, id, 3600, Some(&preset.grants()))
        .unwrap();
    // device JWTs carry no kernel-admin claim
    create_test_token(id, vec!["PERMISSION_IPC_SEND".into()], SECRET, 3600)
}

async fn call(fx: &Fx, method: &str, uri: &str, token: &str) -> StatusCode {
    fx.app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn device_guest_cannot_list_plugins() {
    let fx = fx();
    let t = pair(&fx, "guest-1", Preset::Guest);
    assert_eq!(
        call(&fx, "GET", "/plugins", &t).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&fx, "GET", "/plugins/x/logs", &t).await,
        StatusCode::FORBIDDEN
    );
    // /health stays public
    assert_eq!(call(&fx, "GET", "/health", &t).await, StatusCode::OK);
}

#[tokio::test]
async fn device_user_can_restart_but_not_pair() {
    let fx = fx();
    let t = pair(&fx, "user-1", Preset::User);
    assert_eq!(call(&fx, "GET", "/plugins", &t).await, StatusCode::OK);
    // passes the guard; unknown plugin => 404 (as before K3)
    assert_eq!(
        call(&fx, "POST", "/plugins/ghost/restart", &t).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&fx, "POST", "/devices/pair", &t).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&fx, "GET", "/devices", &t).await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn owner_device_can_list_devices() {
    let fx = fx();
    let t = pair(&fx, "owner-1", Preset::Owner);
    assert_eq!(call(&fx, "GET", "/devices", &t).await, StatusCode::OK);
}

#[tokio::test]
async fn device_grant_revoked_takes_effect_next_request() {
    let fx = fx();
    let t = pair(&fx, "user-1", Preset::User);
    assert_eq!(
        call(&fx, "POST", "/plugins/ghost/restart", &t).await,
        StatusCode::NOT_FOUND
    );
    let mut g: Grants = Preset::User.grants();
    g.remove(&Grant::PluginsControl);
    assert!(fx.store.set_grants("user-1", &g).unwrap());
    // same live token
    assert_eq!(
        call(&fx, "POST", "/plugins/ghost/restart", &t).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(call(&fx, "GET", "/plugins", &t).await, StatusCode::OK);
}

#[tokio::test]
async fn device_admin_claim_without_grant_is_denied() {
    let fx = fx();
    fx.store
        .issue("guest-2", "g", 3600, Some(&Preset::Guest.grants()))
        .unwrap();
    let t = create_test_token(
        "guest-2",
        vec!["PERMISSION_KERNEL_ADMIN".into()],
        SECRET,
        3600,
    );
    assert_eq!(
        call(&fx, "POST", "/plugins/ghost/restart", &t).await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn revoked_device_loses_all_grants_over_http() {
    let fx = fx();
    let t = pair(&fx, "user-1", Preset::User);
    fx.store.set_revoked("user-1", true).unwrap();
    assert_eq!(
        call(&fx, "GET", "/plugins", &t).await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn non_device_token_keeps_kernel_admin_rule() {
    let fx = fx();
    // sub not in the store: plugin / local CLI
    let admin = create_test_token(
        "local-cli",
        vec!["PERMISSION_KERNEL_ADMIN".into()],
        SECRET,
        3600,
    );
    let plain = create_test_token("a-plugin", vec![], SECRET, 3600);
    assert_eq!(
        call(&fx, "POST", "/plugins/ghost/restart", &admin).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&fx, "POST", "/plugins/ghost/restart", &plain).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(call(&fx, "GET", "/plugins", &plain).await, StatusCode::OK);
    assert_eq!(call(&fx, "GET", "/devices", &plain).await, StatusCode::OK);
}
