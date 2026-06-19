use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Serialize;
use std::sync::Arc;

use crate::plugins::registry::PluginRegistry;

#[derive(Serialize)]
pub struct PluginInfo {
    pub plugin_id: String,
    pub state: String,
    pub registered_at: u64,
    pub permissions: Vec<String>,
}

pub async fn list_plugins(State(registry): State<Arc<PluginRegistry>>) -> Json<Vec<PluginInfo>> {
    let plugins = registry
        .list()
        .into_iter()
        .map(|e| PluginInfo {
            plugin_id: e.plugin_id,
            state: format!("{:?}", e.state),
            registered_at: e.registered_at,
            permissions: e.manifest.permissions,
        })
        .collect();
    Json(plugins)
}

pub async fn get_plugin(
    State(registry): State<Arc<PluginRegistry>>,
    Path(id): Path<String>,
) -> Result<Json<PluginInfo>, StatusCode> {
    registry
        .get(&id)
        .map(|e| {
            Json(PluginInfo {
                plugin_id: e.plugin_id,
                state: format!("{:?}", e.state),
                registered_at: e.registered_at,
                permissions: e.manifest.permissions,
            })
        })
        .ok_or(StatusCode::NOT_FOUND)
}

pub async fn stop_plugin(
    State(registry): State<Arc<PluginRegistry>>,
    Path(id): Path<String>,
) -> StatusCode {
    if registry.get(&id).is_none() {
        return StatusCode::NOT_FOUND;
    }
    registry.unregister(&id);
    StatusCode::OK
}
