use axum::{Router, routing::get};
use std::net::SocketAddr;
use tracing::info;

pub struct ApiServer {
    port: u16,
}

impl ApiServer {
    pub fn new(port: u16) -> Self {
        Self { port }
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let app = Router::new()
            .route("/health", get(health_check));

        let addr = SocketAddr::from(([0, 0, 0, 0], self.port));
        info!("🌐 HTTP API: http://localhost:{}", self.port);

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

async fn health_check() -> &'static str {
    "{\"status\":\"ok\"}"
}
