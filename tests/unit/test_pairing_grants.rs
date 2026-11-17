//! K3: grant presets at ticket pairing.

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
use vynkor::auth::pairing::{PairRequest, PairingConfig, PairingService};
use vynkor::plugins::manager::PluginManager;
use vynkor::plugins::registry::PluginRegistry;
use vynkor::plugins::supervisor::PluginSupervisor;
use vynkor::utils::errors::VynkorError;

const SECRET: &str = "pairing-test-secret-pairing-test-secret!";

struct Fx {
    _dir: tempfile::TempDir,
    store: Arc<DeviceStore>,
    svc: Arc<PairingService>,
    app: axum::Router,
}

fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DeviceStore::new(dir.path(), SECRET));
    let svc = Arc::new(PairingService::new(
        dir.path(),
        Arc::clone(&store),
        PairingConfig {
            jwt_secret: SECRET.to_string(),
            audience: "vynkor".to_string(),
            port: 8443,
            tls: false,
            cert_path: None,
            device_ttl_secs: 86_400,
        },
    ));
    let manager = Arc::new(PluginManager::new(
        Arc::new(PluginSupervisor::new("/tmp/vynkor_pairing_grants.sock")),
        Arc::new(PluginRegistry::new()),
    ));
    let app = create_router_full(RouterConfig {
        manager,
        jwt_validator: Some(Arc::new(JwtValidator::new(SECRET.as_bytes()))),
        device_store: Some(Arc::clone(&store)),
        pairing: Some(Arc::clone(&svc)),
        ws_router_tx: None,
        ws_disconnect_tx: None,
        started_at: Instant::now(),
        rate_limit_rps: None,
        rate_limit_burst: None,
        plugin_defs: vec![].into(),
        ws_handshake_timeout_secs: 5,
        max_ws_connections: 16,
        ws_register_timeout_secs: 10,
        ws_device_recheck_secs: 10,
    })
    .app;
    Fx {
        _dir: dir,
        store,
        svc,
        app,
    }
}

fn req(preset: Option<&str>) -> PairRequest {
    PairRequest {
        host: Some("10.0.0.5".into()),
        preset: preset.map(String::from),
        ..Default::default()
    }
}

fn consume_grants(fx: &Fx, preset: Option<&str>) -> Grants {
    let t = fx.svc.create_ticket(req(preset), None).unwrap().ticket;
    let payload = fx.svc.consume(&t).unwrap();
    fx.store.grants(&payload.device_id).unwrap().unwrap()
}

#[test]
fn ticket_preset_applied_on_consume() {
    let fx = fx();
    assert!(consume_grants(&fx, Some("guest")).is_empty());
    assert_eq!(consume_grants(&fx, Some("owner")), Preset::Owner.grants());
    assert_eq!(consume_grants(&fx, Some("user")), Preset::User.grants());
}

#[test]
fn ticket_without_preset_defaults_to_user() {
    let fx = fx();
    assert_eq!(consume_grants(&fx, None), Preset::User.grants());
}

#[test]
fn unknown_preset_is_rejected_at_ticket_creation() {
    let fx = fx();
    let err = fx.svc.create_ticket(req(Some("admin")), None).unwrap_err();
    assert!(matches!(err, VynkorError::InvalidInput(_)));
}

#[test]
fn device_cannot_mint_ticket_above_its_grants() {
    let fx = fx();
    let caller: Grants = [Grant::DevicesManage, Grant::PluginsView]
        .into_iter()
        .collect();
    for over in [Some("owner"), Some("user"), None] {
        let err = fx.svc.create_ticket(req(over), Some(&caller)).unwrap_err();
        assert!(
            matches!(err, VynkorError::PermissionDenied(_)),
            "{over:?} must exceed {caller:?}"
        );
    }
    // guest (no grants) is always a subset
    assert!(fx
        .svc
        .create_ticket(req(Some("guest")), Some(&caller))
        .is_ok());
    // an owner device may mint anything
    let owner = Preset::Owner.grants();
    assert!(fx
        .svc
        .create_ticket(req(Some("owner")), Some(&owner))
        .is_ok());
}

async fn http_pair(fx: &Fx, token: &str, body: serde_json::Value) -> StatusCode {
    fx.app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/devices/pair")
                .header("Authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn http_pair_enforces_caller_ceiling() {
    let fx = fx();
    let mgr_grants: Grants = [Grant::DevicesManage].into_iter().collect();
    fx.store.issue("mgr", "m", 3600, Some(&mgr_grants)).unwrap();
    let t = create_test_token("mgr", vec![], SECRET.as_bytes(), 3600);
    let host = "10.0.0.5";
    assert_eq!(
        http_pair(
            &fx,
            &t,
            serde_json::json!({"host": host, "preset": "owner"})
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        http_pair(&fx, &t, serde_json::json!({"host": host})).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        http_pair(
            &fx,
            &t,
            serde_json::json!({"host": host, "preset": "guest"})
        )
        .await,
        StatusCode::OK
    );

    // a non-device admin (local CLI) may mint any preset
    let admin = create_test_token(
        "local-cli",
        vec!["PERMISSION_KERNEL_ADMIN".into()],
        SECRET.as_bytes(),
        3600,
    );
    assert_eq!(
        http_pair(
            &fx,
            &admin,
            serde_json::json!({"host": host, "preset": "owner"})
        )
        .await,
        StatusCode::OK
    );
}
