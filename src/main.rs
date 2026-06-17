mod api;
mod ipc;
mod utils;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    utils::logging::init();
    tracing::info!("🚀 Veyron starting...");

    let api = api::server::ApiServer::new(8783);
    let api_task = tokio::spawn(async move {
        if let Err(e) = api.run().await {
            tracing::error!("API error: {}", e);
        }
    });

    tracing::info!("✅ Kernel ready!");

    tokio::signal::ctrl_c().await?;
    tracing::info!("🛑 Shutting down...");

    api_task.abort();

    Ok(())
}
