use axum::{middleware, routing::get, routing::post, Router};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

use crate::api::middleware::auth_middleware;
use crate::api::routes::{
    get_plugin, get_plugin_logs, list_plugins, restart_plugin, stop_plugin, AppState,
};
use crate::auth::jwt::JwtValidator;
use crate::plugins::registry::PluginRegistry;
use crate::plugins::supervisor::PluginSupervisor;

pub fn create_router(
    registry: Arc<PluginRegistry>,
    supervisor: Arc<PluginSupervisor>,
    jwt_validator: Option<Arc<JwtValidator>>,
) -> Router {
    let state = Arc::new(AppState {
        registry,
        supervisor,
        jwt_validator,
    });

    let public = Router::new()
        .route("/health", get(health_check))
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

    Router::new()
        .merge(public)
        .merge(protected)
        .with_state(state)
}

pub struct ApiServer {
    port: u16,
    registry: Arc<PluginRegistry>,
    supervisor: Arc<PluginSupervisor>,
    jwt_validator: Option<Arc<JwtValidator>>,
}

impl ApiServer {
    pub fn new(
        port: u16,
        registry: Arc<PluginRegistry>,
        supervisor: Arc<PluginSupervisor>,
        jwt_validator: Option<Arc<JwtValidator>>,
    ) -> Self {
        Self {
            port,
            registry,
            supervisor,
            jwt_validator,
        }
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let app = create_router(
            Arc::clone(&self.registry),
            Arc::clone(&self.supervisor),
            self.jwt_validator.clone(),
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
