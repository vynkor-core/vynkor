use axum::{
    http::{header, StatusCode},
    middleware,
    response::IntoResponse,
    routing::get,
    routing::post,
    Router,
};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{atomic::AtomicU64, Arc};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tower_http::timeout::TimeoutLayer;
use tracing::info;

use crate::api::middleware::auth_middleware;
use crate::api::rate_limit::{build_rate_limiter, rate_limit_middleware};
use crate::api::routes::{
    get_plugin, get_plugin_logs, health_check, list_plugins, restart_plugin, stop_plugin, AppState,
};
use crate::api::websocket::{ws_handler, WsGateway};
use crate::auth::jwt::JwtValidator;
use crate::ipc::messages::IncomingMessage;
use crate::plugins::manager::PluginManager;

/// Convenience constructor for tests (no WebSocket support).
pub fn create_router(
    manager: Arc<PluginManager>,
    jwt_validator: Option<Arc<JwtValidator>>,
) -> Router {
    create_router_full(
        manager,
        jwt_validator,
        None,
        None,
        Instant::now(),
        None,
        None,
    )
}

pub fn create_router_full(
    manager: Arc<PluginManager>,
    jwt_validator: Option<Arc<JwtValidator>>,
    ws_router_tx: Option<mpsc::Sender<IncomingMessage>>,
    ws_disconnect_tx: Option<mpsc::Sender<u64>>,
    started_at: Instant,
    rate_limit_rps: Option<u32>,
    rate_limit_burst: Option<u32>,
) -> Router {
    let state = Arc::new(AppState {
        manager,
        jwt_validator: jwt_validator.clone(),
        started_at,
    });

    let public = Router::new().route("/health", get(health_check));

    // All non-health endpoints require auth when jwt_secret is configured.
    // auth_middleware short-circuits to next.run when jwt_validator is None,
    // so allow_no_auth deployments see no change in behaviour.
    let mut protected = Router::new()
        .route("/metrics", get(get_metrics))
        .route("/plugins", get(list_plugins))
        .route("/plugins/:id", get(get_plugin))
        .route("/plugins/:id/logs", get(get_plugin_logs))
        .route("/plugins/:id/stop", post(stop_plugin))
        .route("/plugins/:id/restart", post(restart_plugin));

    // Per-token rate limiting: only active when JWT auth is configured. Layered
    // *before* auth_middleware is added below, so auth ends up outermost and
    // runs first — rate_limit_middleware only ever sees verified claims
    // (BUG-004: keying on a self-decoded, unverified `sub` let an attacker
    // rotate it to dodge the limit or forge a victim's id to burn their quota).
    if jwt_validator.is_some() {
        let limiter = build_rate_limiter(
            rate_limit_rps.unwrap_or(100),
            rate_limit_burst.unwrap_or(20),
        );
        // The keyed limiter's state grows forever otherwise: `sub` is an
        // attacker-controlled JWT claim, so a client can mint a fresh one per
        // request to bloat memory (AUDIT M-01). Evict idle keys periodically.
        let prune_limiter = Arc::clone(&limiter);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                prune_limiter.retain_recent();
            }
        });
        protected = protected.layer(middleware::from_fn_with_state(
            limiter,
            rate_limit_middleware,
        ));
    }

    protected = protected.route_layer(middleware::from_fn_with_state(
        Arc::clone(&state),
        auth_middleware,
    ));

    let mut app = Router::new()
        .merge(public)
        .merge(protected)
        .with_state(state);

    if let (Some(router_tx), Some(disconnect_tx)) = (ws_router_tx, ws_disconnect_tx) {
        let gateway = Arc::new(WsGateway {
            router_tx,
            disconnect_tx,
            conn_counter: Arc::new(AtomicU64::new(0)),
            jwt_validator,
        });
        let ws_sub = Router::new()
            .route("/ws", get(ws_handler))
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                Duration::from_secs(5),
            ))
            .with_state(gateway);
        app = app.merge(ws_sub);
    }

    app
}

pub struct ApiServer {
    port: u16,
    manager: Arc<PluginManager>,
    jwt_validator: Option<Arc<JwtValidator>>,
    ws_router_tx: Option<mpsc::Sender<IncomingMessage>>,
    ws_disconnect_tx: Option<mpsc::Sender<u64>>,
    started_at: Instant,
    rate_limit_rps: Option<u32>,
    rate_limit_burst: Option<u32>,
    tls_cert_path: Option<PathBuf>,
    tls_key_path: Option<PathBuf>,
}

impl ApiServer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        port: u16,
        manager: Arc<PluginManager>,
        jwt_validator: Option<Arc<JwtValidator>>,
        ws_router_tx: Option<mpsc::Sender<IncomingMessage>>,
        ws_disconnect_tx: Option<mpsc::Sender<u64>>,
        started_at: Instant,
        rate_limit_rps: Option<u32>,
        rate_limit_burst: Option<u32>,
        tls_cert_path: Option<PathBuf>,
        tls_key_path: Option<PathBuf>,
    ) -> Self {
        Self {
            port,
            manager,
            jwt_validator,
            ws_router_tx,
            ws_disconnect_tx,
            started_at,
            rate_limit_rps,
            rate_limit_burst,
            tls_cert_path,
            tls_key_path,
        }
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let app = create_router_full(
            Arc::clone(&self.manager),
            self.jwt_validator.clone(),
            self.ws_router_tx.clone(),
            self.ws_disconnect_tx.clone(),
            self.started_at,
            self.rate_limit_rps,
            self.rate_limit_burst,
        );
        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));

        if let (Some(cert), Some(key)) = (&self.tls_cert_path, &self.tls_key_path) {
            info!("HTTPS/WSS API: https://localhost:{}", self.port);
            let tls_config =
                axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
            axum_server::bind_rustls(addr, tls_config)
                .serve(app.into_make_service())
                .await?;
        } else {
            info!("HTTP API: http://localhost:{}", self.port);
            axum_server::bind(addr)
                .serve(app.into_make_service())
                .await?;
        }
        Ok(())
    }
}

async fn get_metrics() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        crate::metrics::render(),
    )
}
