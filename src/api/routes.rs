use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::plugins::registry::PluginRegistry;
use crate::plugins::supervisor::PluginSupervisor;

pub struct AppState {
    pub registry: Arc<PluginRegistry>,
    pub supervisor: Arc<PluginSupervisor>,
}

#[derive(Serialize)]
pub struct PluginInfo {
    pub plugin_id: String,
    pub state: String,
    pub registered_at: u64,
    pub permissions: Vec<String>,
}

pub async fn list_plugins(State(state): State<Arc<AppState>>) -> Json<Vec<PluginInfo>> {
    let plugins = state
        .registry
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
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<PluginInfo>, StatusCode> {
    state
        .registry
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

pub async fn stop_plugin(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> StatusCode {
    if state.registry.get(&id).is_none() {
        return StatusCode::NOT_FOUND;
    }
    state.registry.unregister(&id);
    StatusCode::OK
}

pub async fn restart_plugin(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> StatusCode {
    if state.registry.get(&id).is_none() {
        return StatusCode::NOT_FOUND;
    }
    match state.supervisor.restart_plugin(&id).await {
        Ok(()) => StatusCode::ACCEPTED,
        // Plugin not in supervisor entries (connected on its own, not spawned)
        Err(_) => StatusCode::UNPROCESSABLE_ENTITY,
    }
}

#[derive(Deserialize)]
pub struct LogsQuery {
    pub lines: Option<usize>,
}

pub async fn get_plugin_logs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<LogsQuery>,
) -> Result<Json<Vec<String>>, StatusCode> {
    if state.registry.get(&id).is_none() && !state.supervisor.is_running(&id) {
        return Err(StatusCode::NOT_FOUND);
    }
    let n = q.lines.unwrap_or(100);
    Ok(Json(state.supervisor.get_logs(&id, n).await))
}
