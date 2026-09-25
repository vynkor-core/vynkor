//! CD-01: single-use pairing tickets — `POST /devices/pair` (admin) mints a
//! ticket, `POST /devices/consume` (public, rate-limited) trades it for a
//! per-device credential.

use crate::jwt_helper::create_test_token;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use std::time::Instant;
use tower::ServiceExt;
use vynkor::api::server::{create_router_full, RouterConfig};
use vynkor::auth::device_store::DeviceStore;
use vynkor::auth::jwt::JwtValidator;
use vynkor::auth::pairing::{PairRequest, PairingConfig, PairingService};
use vynkor::auth::ticket_store::{TicketStore, TICKET_LEN};
use vynkor::plugins::manager::PluginManager;
use vynkor::plugins::registry::PluginRegistry;
use vynkor::plugins::supervisor::PluginSupervisor;
use vynkor::utils::errors::{TicketRejection, VynkorError};

// mint_device_token refuses secrets shorter than 32 bytes (MA-18)
const SECRET: &str = "pairing-test-secret-pairing-test-secret!";

fn service(dir: &std::path::Path) -> Arc<PairingService> {
    let store = Arc::new(DeviceStore::new(dir, SECRET));
    Arc::new(PairingService::new(
        dir,
        store,
        PairingConfig {
            jwt_secret: SECRET.to_string(),
            audience: "vynkor".to_string(),
            port: 8443,
            tls: false,
            cert_path: None,
            device_ttl_secs: 86_400,
        },
    ))
}

fn router(pairing: Arc<PairingService>) -> axum::Router {
    let manager = Arc::new(PluginManager::new(
        Arc::new(PluginSupervisor::new("/tmp/vynkor_pairing_test.sock")),
        Arc::new(PluginRegistry::new()),
    ));
    create_router_full(RouterConfig {
        manager,
        jwt_validator: Some(Arc::new(JwtValidator::new(SECRET.as_bytes()))),
        device_store: None,
        pairing: Some(pairing),
        ws_router_tx: None,
        ws_disconnect_tx: None,
        started_at: Instant::now(),
        rate_limit_rps: None,
        rate_limit_burst: None,
        plugin_defs: vec![],
        ws_handshake_timeout_secs: 5,
        max_ws_connections: 1024,
        ws_register_timeout_secs: 10,
    })
    .app
}

fn admin_token() -> String {
    create_test_token(
        "admin",
        vec!["PERMISSION_KERNEL_ADMIN".to_string()],
        SECRET.as_bytes(),
        3600,
    )
}

async fn post(
    app: &axum::Router,
    path: &str,
    token: Option<&str>,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn pair(app: &axum::Router) -> serde_json::Value {
    let (status, json) = post(
        app,
        "/devices/pair",
        Some(&admin_token()),
        serde_json::json!({"host": "10.0.0.5", "name": "friend phone"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    json
}

#[tokio::test]
async fn test_ticket_single_use_and_ttl() {
    let dir = tempfile::tempdir().unwrap();
    let svc = service(dir.path());
    let app = router(Arc::clone(&svc));

    let view = pair(&app).await;
    let ticket = view["ticket"].as_str().unwrap().to_string();
    assert_eq!(ticket.len(), TICKET_LEN);
    assert_eq!(view["v"], 2);
    assert_eq!(view["ttl_secs"], 300);
    assert_eq!(view["ws"], "ws://10.0.0.5:8443/ws");

    // first consume → full v2 pair payload, no admin token needed
    let (status, payload) = post(
        &app,
        "/devices/consume",
        None,
        serde_json::json!({ "ticket": ticket }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{payload}");
    assert_eq!(payload["v"], 2);
    assert_eq!(payload["host_url"], "ws://10.0.0.5:8443/ws");
    assert_eq!(payload["name"], "friend phone");
    let device_id = payload["device_id"].as_str().unwrap();
    assert_eq!(payload["device_secret"].as_str().unwrap().len(), 64);
    let claims = JwtValidator::new(SECRET.as_bytes())
        .validate(payload["jwt_token"].as_str().unwrap())
        .unwrap();
    assert_eq!(claims.sub, device_id);
    assert!(
        !payload.to_string().contains(SECRET),
        "master secret leaked"
    );
    assert!(payload.get("jwt_secret").is_none());

    // the device row exists and is active
    let store = DeviceStore::new(dir.path(), SECRET);
    assert!(store.active_secret(device_id).unwrap().is_some());

    // second consume → 409
    let (status, err) = post(
        &app,
        "/devices/consume",
        None,
        serde_json::json!({ "ticket": ticket }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(err["code"], 409);

    // expired → 410 (issued in the past, so no wall-clock wait)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let stale = svc
        .tickets()
        .issue_at(now - 1_000, 300, None, "ws://10.0.0.5:8443/ws".into())
        .unwrap();
    let (status, err) = post(
        &app,
        "/devices/consume",
        None,
        serde_json::json!({ "ticket": stale.ticket }),
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(err["code"], 410);
}

#[tokio::test]
async fn unknown_and_forged_tickets_look_the_same() {
    let dir = tempfile::tempdir().unwrap();
    let app = router(service(dir.path()));

    for bogus in ["nope", &"A".repeat(TICKET_LEN), ""] {
        let (status, err) = post(
            &app,
            "/devices/consume",
            None,
            serde_json::json!({ "ticket": bogus }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{bogus}");
        assert_eq!(err["code"], 404);
    }
}

#[tokio::test]
async fn pair_requires_kernel_admin() {
    let dir = tempfile::tempdir().unwrap();
    let app = router(service(dir.path()));
    let body = serde_json::json!({"host": "10.0.0.5"});

    let (status, _) = post(&app, "/devices/pair", None, body.clone()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let plain = create_test_token("p", vec![], SECRET.as_bytes(), 3600);
    let (status, _) = post(&app, "/devices/pair", Some(&plain), body).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn pair_clamps_ttl() {
    let dir = tempfile::tempdir().unwrap();
    let app = router(service(dir.path()));
    let (status, view) = post(
        &app,
        "/devices/pair",
        Some(&admin_token()),
        serde_json::json!({"host": "10.0.0.5", "ttl_secs": 999_999}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["ttl_secs"], 3600);
}

#[tokio::test]
async fn consume_is_rate_limited() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DeviceStore::new(dir.path(), SECRET));
    let svc = PairingService::new(
        dir.path(),
        store,
        PairingConfig {
            jwt_secret: SECRET.to_string(),
            audience: "vynkor".to_string(),
            port: 8443,
            tls: false,
            cert_path: None,
            device_ttl_secs: 86_400,
        },
    )
    .with_consume_quota(1, 1);
    let app = router(Arc::new(svc));

    let body = serde_json::json!({"ticket": "x"});
    let (first, _) = post(&app, "/devices/consume", None, body.clone()).await;
    assert_eq!(first, StatusCode::NOT_FOUND);
    let (second, err) = post(&app, "/devices/consume", None, body).await;
    assert_eq!(second, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(err["retryable"], true);
}

#[test]
fn concurrent_consume_issues_exactly_one_device() {
    let dir = tempfile::tempdir().unwrap();
    let svc = service(dir.path());
    let view = svc
        .create_ticket(PairRequest {
            host: Some("10.0.0.5".into()),
            ..Default::default()
        })
        .unwrap();

    let handles: Vec<_> = (0..16)
        .map(|_| {
            let svc = Arc::clone(&svc);
            let ticket = view.ticket.clone();
            std::thread::spawn(move || svc.consume(&ticket))
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let ok = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok, 1, "exactly one consumer may win");
    for r in results.iter().filter(|r| r.is_err()) {
        assert!(matches!(
            r,
            Err(VynkorError::Ticket(TicketRejection::AlreadyUsed))
        ));
    }
    let store = DeviceStore::new(dir.path(), SECRET);
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn concurrent_consumes_of_distinct_tickets_keep_every_device() {
    // devices.json is read-modify-write — in-process issuers must serialize
    let dir = tempfile::tempdir().unwrap();
    let svc = service(dir.path());
    let tickets: Vec<String> = (0..8)
        .map(|_| {
            svc.create_ticket(PairRequest {
                host: Some("10.0.0.5".into()),
                ..Default::default()
            })
            .unwrap()
            .ticket
        })
        .collect();
    let handles: Vec<_> = tickets
        .into_iter()
        .map(|t| {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.consume(&t).unwrap())
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let store = DeviceStore::new(dir.path(), SECRET);
    assert_eq!(store.list().unwrap().len(), 8);
}

#[test]
fn tickets_file_holds_no_raw_ticket_and_is_private() {
    let dir = tempfile::tempdir().unwrap();
    let store = TicketStore::new(dir.path());
    let issued = store.issue(300, None, "ws://h:1/ws".into()).unwrap();

    let raw = std::fs::read_to_string(store.path()).unwrap();
    assert!(
        !raw.contains(&issued.ticket),
        "raw ticket must not hit disk"
    );
    assert!(raw.contains(&issued.record.ticket_sha256));

    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(store.path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(store.path().file_name().unwrap(), "tickets.json");
}

#[test]
fn ticket_store_survives_reopen_and_sweeps_stale_rows() {
    let dir = tempfile::tempdir().unwrap();
    let ticket = {
        let store = TicketStore::new(dir.path());
        store.issue(300, None, "ws://h:1/ws".into()).unwrap().ticket
    };
    // a fresh handle (kernel restart) still honours the ticket
    let store = TicketStore::new(dir.path());
    let rec = store.consume(&ticket).unwrap();
    assert!(rec.used_at.is_some());

    // far in the future every row is stale and gets swept
    let removed = store.sweep_expired(u64::MAX / 2).unwrap();
    assert_eq!(removed, 1);
    assert!(matches!(
        store.consume(&ticket),
        Err(VynkorError::Ticket(TicketRejection::Unknown))
    ));
}

#[test]
fn sweep_keeps_recently_expired_rows_so_they_still_report_gone() {
    let dir = tempfile::tempdir().unwrap();
    let store = TicketStore::new(dir.path());
    let issued = store
        .issue_at(1_000, 60, None, "ws://h:1/ws".into())
        .unwrap();
    // just past expiry: swept rows would degrade 410 into 404
    assert_eq!(store.sweep_expired(1_100).unwrap(), 0);
    assert!(matches!(
        store.consume_at(&issued.ticket, 1_100),
        Err(VynkorError::Ticket(TicketRejection::Expired))
    ));
}
