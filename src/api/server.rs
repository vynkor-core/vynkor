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

use crate::api::middleware::{auth_middleware, require_kernel_admin};
use crate::api::rate_limit::{build_rate_limiter, rate_limit_middleware, TokenRateLimiter};
use crate::api::routes::{
    get_plugin, get_plugin_logs, health_check, list_devices, list_plugins, restart_plugin,
    start_plugin, stop_plugin, AppState,
};
use crate::api::websocket::{ws_handler, WsGateway};
use crate::auth::jwt::JwtValidator;
use crate::ipc::messages::IncomingMessage;
use crate::plugins::manager::PluginManager;
use crate::utils::config::PluginDef;

/// All router wiring in one place (MA-06): replaces the former 11-argument
/// positional signature that needed a clippy suppression.
pub struct RouterConfig {
    pub manager: Arc<PluginManager>,
    pub jwt_validator: Option<Arc<JwtValidator>>,
    pub ws_router_tx: Option<mpsc::Sender<IncomingMessage>>,
    pub ws_disconnect_tx: Option<mpsc::Sender<u64>>,
    pub started_at: Instant,
    pub rate_limit_rps: Option<u32>,
    pub rate_limit_burst: Option<u32>,
    pub plugin_defs: Vec<PluginDef>,
    pub ws_handshake_timeout_secs: u64,
    pub max_ws_connections: usize,
    pub ws_register_timeout_secs: u64,
}

/// The built router plus its keyed rate limiter (`Some` once JWT auth is on).
/// The limiter's idle-key eviction task belongs to the long-lived server
/// runner via `spawn_rate_limiter_prune` — spawning it inside the router
/// builder leaks a handle-less task in every test build.
pub struct BuiltRouter {
    pub app: Router,
    pub rate_limiter: Option<Arc<TokenRateLimiter>>,
}

/// Convenience constructor for tests (no WebSocket support).
pub fn create_router(
    manager: Arc<PluginManager>,
    jwt_validator: Option<Arc<JwtValidator>>,
) -> Router {
    create_router_full(RouterConfig {
        manager,
        jwt_validator,
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

pub fn create_router_full(config: RouterConfig) -> BuiltRouter {
    let state = Arc::new(AppState {
        manager: config.manager,
        jwt_validator: config.jwt_validator.clone(),
        started_at: config.started_at,
        plugin_defs: config.plugin_defs,
    });

    let public = Router::new().route("/health", get(health_check));

    // All non-health endpoints require auth when jwt_secret is configured.
    // auth_middleware short-circuits to next.run when jwt_validator is None,
    // so allow_no_auth deployments see no change in behaviour.
    // Lifecycle routes additionally require PERMISSION_KERNEL_ADMIN (T-01):
    // auth_middleware only proves the caller holds *a* valid JWT, not that
    // it's authorized to start/stop/restart plugins.
    let admin = Router::new()
        .route("/plugins/{id}/start", post(start_plugin))
        .route("/plugins/{id}/stop", post(stop_plugin))
        .route("/plugins/{id}/restart", post(restart_plugin))
        .layer(middleware::from_fn(require_kernel_admin));

    let mut protected = Router::new()
        .route("/metrics", get(get_metrics))
        .route("/plugins", get(list_plugins))
        .route("/plugins/{id}", get(get_plugin))
        .route("/plugins/{id}/logs", get(get_plugin_logs))
        .route("/devices", get(list_devices))
        .merge(admin);

    // Per-token rate limiting: only active when JWT auth is configured. Layered
    // *before* auth_middleware is added below, so auth ends up outermost and
    // runs first — rate_limit_middleware only ever sees verified claims
    // (BUG-004: keying on a self-decoded, unverified `sub` let an attacker
    // rotate it to dodge the limit or forge a victim's id to burn their quota).
    let mut rate_limiter = None;
    if config.jwt_validator.is_some() {
        let limiter = build_rate_limiter(
            config.rate_limit_rps.unwrap_or(100),
            config.rate_limit_burst.unwrap_or(20),
        );
        // The keyed limiter's state grows forever otherwise: `sub` is an
        // attacker-controlled JWT claim, so a client can mint a fresh one per
        // request to bloat memory (AUDIT M-01). Eviction is spawned by the
        // server runner, not here.
        rate_limiter = Some(Arc::clone(&limiter));
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

    if let (Some(router_tx), Some(disconnect_tx)) = (config.ws_router_tx, config.ws_disconnect_tx) {
        let gateway = Arc::new(WsGateway {
            router_tx,
            disconnect_tx,
            conn_counter: Arc::new(AtomicU64::new(0)),
            jwt_validator: config.jwt_validator,
            open_conns: Arc::new(AtomicU64::new(0)),
            max_connections: config.max_ws_connections,
            register_timeout_secs: config.ws_register_timeout_secs,
        });
        let ws_sub = Router::new()
            .route("/ws", get(ws_handler))
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                Duration::from_secs(config.ws_handshake_timeout_secs),
            ))
            .with_state(gateway);
        app = app.merge(ws_sub);
    }

    BuiltRouter { app, rate_limiter }
}

/// 60s eviction of idle rate-limit keys (AUDIT M-01 bound). Spawned by the
/// long-lived server runner, not by `create_router_full` (MA-06).
pub fn spawn_rate_limiter_prune(limiter: Arc<TokenRateLimiter>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            limiter.retain_recent();
        }
    })
}

pub struct ApiServer {
    port: u16,
    bind_ip: std::net::IpAddr,
    manager: Arc<PluginManager>,
    jwt_validator: Option<Arc<JwtValidator>>,
    ws_router_tx: Option<mpsc::Sender<IncomingMessage>>,
    ws_disconnect_tx: Option<mpsc::Sender<u64>>,
    started_at: Instant,
    rate_limit_rps: Option<u32>,
    rate_limit_burst: Option<u32>,
    tls_cert_path: Option<PathBuf>,
    tls_key_path: Option<PathBuf>,
    plugin_defs: Vec<PluginDef>,
    ws_handshake_timeout_secs: u64,
    max_ws_connections: usize,
    ws_register_timeout_secs: u64,
}

impl ApiServer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        port: u16,
        bind_ip: std::net::IpAddr,
        manager: Arc<PluginManager>,
        jwt_validator: Option<Arc<JwtValidator>>,
        ws_router_tx: Option<mpsc::Sender<IncomingMessage>>,
        ws_disconnect_tx: Option<mpsc::Sender<u64>>,
        started_at: Instant,
        rate_limit_rps: Option<u32>,
        rate_limit_burst: Option<u32>,
        tls_cert_path: Option<PathBuf>,
        tls_key_path: Option<PathBuf>,
        plugin_defs: Vec<PluginDef>,
        ws_handshake_timeout_secs: u64,
        max_ws_connections: usize,
        ws_register_timeout_secs: u64,
    ) -> Self {
        Self {
            port,
            bind_ip,
            manager,
            jwt_validator,
            ws_router_tx,
            ws_disconnect_tx,
            started_at,
            rate_limit_rps,
            rate_limit_burst,
            tls_cert_path,
            tls_key_path,
            plugin_defs,
            ws_handshake_timeout_secs,
            max_ws_connections,
            ws_register_timeout_secs,
        }
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let built = create_router_full(RouterConfig {
            manager: Arc::clone(&self.manager),
            jwt_validator: self.jwt_validator.clone(),
            ws_router_tx: self.ws_router_tx.clone(),
            ws_disconnect_tx: self.ws_disconnect_tx.clone(),
            started_at: self.started_at,
            rate_limit_rps: self.rate_limit_rps,
            rate_limit_burst: self.rate_limit_burst,
            plugin_defs: self.plugin_defs.clone(),
            ws_handshake_timeout_secs: self.ws_handshake_timeout_secs,
            max_ws_connections: self.max_ws_connections,
            ws_register_timeout_secs: self.ws_register_timeout_secs,
        });
        // named binding on purpose: `let _` would drop (and detach) instantly,
        // while `_prune_task` keeps the evictor alive until run() returns
        let _prune_task = built.rate_limiter.map(spawn_rate_limiter_prune);
        let app = built.app;
        let addr = SocketAddr::from((self.bind_ip, self.port));

        if let (Some(cert), Some(key)) = (&self.tls_cert_path, &self.tls_key_path) {
            info!("HTTPS/WSS API: https://{}", addr);
            let tls_config =
                axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
            axum_server::bind_rustls(addr, tls_config)
                .serve(app.into_make_service())
                .await?;
        } else {
            info!("HTTP API: http://{}", addr);
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
