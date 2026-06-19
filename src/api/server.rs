use axum::{routing::get, routing::post, Router};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::info;

use crate::api::routes::{get_plugin, list_plugins, stop_plugin};
use crate::plugins::registry::PluginRegistry;

pub fn create_router(registry: Arc<PluginRegistry>) -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/plugins", get(list_plugins))
        .route("/plugins/:id", get(get_plugin))
        .route("/plugins/:id/stop", post(stop_plugin))
        .with_state(registry)
}

pub struct ApiServer {
    port: u16,
    registry: Arc<PluginRegistry>,
}

impl ApiServer {
    pub fn new(port: u16, registry: Arc<PluginRegistry>) -> Self {
        Self { port, registry }
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let app = create_router(Arc::clone(&self.registry));
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
