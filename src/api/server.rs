use axum::{routing::get, routing::post, Router};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

use crate::api::routes::{get_plugin, list_plugins, restart_plugin, stop_plugin, AppState};
use crate::plugins::registry::PluginRegistry;
use crate::plugins::supervisor::PluginSupervisor;

pub fn create_router(registry: Arc<PluginRegistry>, supervisor: Arc<PluginSupervisor>) -> Router {
    let state = Arc::new(AppState {
        registry,
        supervisor,
    });
    Router::new()
        .route("/health", get(health_check))
        .route("/plugins", get(list_plugins))
        .route("/plugins/:id", get(get_plugin))
        .route("/plugins/:id/stop", post(stop_plugin))
        .route("/plugins/:id/restart", post(restart_plugin))
        .with_state(state)
}

pub struct ApiServer {
    port: u16,
    registry: Arc<PluginRegistry>,
    supervisor: Arc<PluginSupervisor>,
}

impl ApiServer {
    pub fn new(
        port: u16,
        registry: Arc<PluginRegistry>,
        supervisor: Arc<PluginSupervisor>,
    ) -> Self {
        Self {
            port,
            registry,
            supervisor,
        }
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let app = create_router(Arc::clone(&self.registry), Arc::clone(&self.supervisor));
        let addr = SocketAddr::from(([0, 0, 0, 0], self.port));
        info!("HTTP API: http://localhost:{}", self.port);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;
        Ok(())
    }
}

async fn health_check() -> &'static str {
    "{\"status\":\"ok\"}"
}
