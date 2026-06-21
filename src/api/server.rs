use axum::{
    http::{header, StatusCode},
    middleware,
    response::IntoResponse,
    routing::get,
    routing::post,
    Router,
};
use std::net::SocketAddr;
use std::sync::{atomic::AtomicU64, Arc};
use std::time::Duration;
use tokio::sync::mpsc;
use tower_http::timeout::TimeoutLayer;
use tracing::info;

use crate::api::middleware::auth_middleware;
use crate::api::routes::{
    get_plugin, get_plugin_logs, list_plugins, restart_plugin, stop_plugin, AppState,
};
use crate::api::websocket::{ws_handler, WsGateway};
use crate::auth::jwt::JwtValidator;
use crate::ipc::messages::IncomingMessage;
use crate::plugins::registry::PluginRegistry;
use crate::plugins::supervisor::PluginSupervisor;

pub fn create_router(
    registry: Arc<PluginRegistry>,
    supervisor: Arc<PluginSupervisor>,
    jwt_validator: Option<Arc<JwtValidator>>,
) -> Router {
    create_router_full(registry, supervisor, jwt_validator, None, None)
}

pub fn create_router_full(
    registry: Arc<PluginRegistry>,
    supervisor: Arc<PluginSupervisor>,
    jwt_validator: Option<Arc<JwtValidator>>,
    ws_router_tx: Option<mpsc::Sender<IncomingMessage>>,
    ws_disconnect_tx: Option<mpsc::Sender<u64>>,
) -> Router {
    let state = Arc::new(AppState {
        registry,
        supervisor,
        jwt_validator: jwt_validator.clone(),
    });

    let public = Router::new()
        .route("/health", get(health_check))
        .route("/metrics", get(get_metrics))
        .route("/plugins", get(list_plugins))
        .route("/plugins/:id", get(get_plugin))
        .route("/plugins/:id/logs", get(get_plugin_logs));

    let protected = Router::new()
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
    registry: Arc<PluginRegistry>,
    supervisor: Arc<PluginSupervisor>,
    jwt_validator: Option<Arc<JwtValidator>>,
    ws_router_tx: Option<mpsc::Sender<IncomingMessage>>,
    ws_disconnect_tx: Option<mpsc::Sender<u64>>,
}

impl ApiServer {
    pub fn new(
        port: u16,
        registry: Arc<PluginRegistry>,
        supervisor: Arc<PluginSupervisor>,
        jwt_validator: Option<Arc<JwtValidator>>,
        ws_router_tx: Option<mpsc::Sender<IncomingMessage>>,
        ws_disconnect_tx: Option<mpsc::Sender<u64>>,
    ) -> Self {
        Self {
            port,
            registry,
            supervisor,
            jwt_validator,
            ws_router_tx,
            ws_disconnect_tx,
        }
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let app = create_router_full(
            Arc::clone(&self.registry),
            Arc::clone(&self.supervisor),
            self.jwt_validator.clone(),
            self.ws_router_tx.clone(),
            self.ws_disconnect_tx.clone(),
        );
        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        info!("HTTP API: http://localhost:{}", self.port);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;
        Ok(())
    }
}

async fn health_check() -> &'static str {
    "{\"status\":\"ok\"}"
}

async fn get_metrics() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        crate::metrics::render(),
    )
}
