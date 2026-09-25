use axum::{
    extract::{rejection::JsonRejection, Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

use crate::auth::jwt::JwtValidator;
use crate::auth::pairing::{PairPayload, PairRequest, PairingService, TicketView};
use crate::plugins::loader::{validate_plugin_def, PluginLoader};
use crate::plugins::manager::PluginManager;
use crate::utils::config::PluginDef;
use crate::utils::errors::{TicketRejection, VynkorError};

pub struct AppState {
    pub manager: Arc<PluginManager>,
    pub jwt_validator: Option<Arc<JwtValidator>>,
    pub started_at: Instant,
    /// Plugins declared under `plugins:` in config.yaml — the set `POST
    /// /plugins/:id/start` is allowed to spawn (never arbitrary binaries).
    pub plugin_defs: Vec<PluginDef>,
    /// cd-01: ticket pairing; `None` without jwt_secret (nothing to sign with)
    pub pairing: Option<Arc<PairingService>>,
}

/// Uniform JSON envelope for every non-2xx REST response (UX-1). Clients parse
/// one shape — `{code, message, retryable}` — instead of matching on bare
/// status codes or empty bodies.
#[derive(Serialize)]
pub struct ApiError {
    pub code: u16,
    pub message: String,
    pub retryable: bool,
}

impl ApiError {
    fn body(
        code: StatusCode,
        message: impl Into<String>,
        retryable: bool,
    ) -> (StatusCode, Json<ApiError>) {
        (
            code,
            Json(ApiError {
                code: code.as_u16(),
                message: message.into(),
                retryable,
            }),
        )
    }

    pub fn not_found(msg: &str) -> (StatusCode, Json<ApiError>) {
        Self::body(StatusCode::NOT_FOUND, msg, false)
    }

    pub fn conflict(msg: &str) -> (StatusCode, Json<ApiError>) {
        Self::body(StatusCode::CONFLICT, msg, false)
    }

    pub fn unprocessable(msg: &str) -> (StatusCode, Json<ApiError>) {
        Self::body(StatusCode::UNPROCESSABLE_ENTITY, msg, false)
    }

    pub fn forbidden(msg: &str) -> (StatusCode, Json<ApiError>) {
        Self::body(StatusCode::FORBIDDEN, msg, false)
    }

    pub fn bad_request(msg: &str) -> (StatusCode, Json<ApiError>) {
        Self::body(StatusCode::BAD_REQUEST, msg, false)
    }

    pub fn gone(msg: &str) -> (StatusCode, Json<ApiError>) {
        Self::body(StatusCode::GONE, msg, false)
    }

    pub fn too_many_requests(msg: &str) -> (StatusCode, Json<ApiError>) {
        Self::body(StatusCode::TOO_MANY_REQUESTS, msg, true)
    }

    pub fn unavailable(msg: &str) -> (StatusCode, Json<ApiError>) {
        Self::body(StatusCode::SERVICE_UNAVAILABLE, msg, false)
    }

    pub fn internal(msg: &str) -> (StatusCode, Json<ApiError>) {
        Self::body(StatusCode::INTERNAL_SERVER_ERROR, msg, true)
    }
}

#[derive(Serialize)]
pub struct Health {
    pub status: &'static str,
    pub version: &'static str,
    pub uptime_secs: u64,
    pub plugins: usize,
}

/// Real health report: process liveness plus readiness detail (uptime, plugin
/// count, build version). Returns 200 whenever the process can answer.
pub async fn health_check(State(state): State<Arc<AppState>>) -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: state.started_at.elapsed().as_secs(),
        plugins: state.manager.list().len(),
    })
}

#[derive(Serialize)]
pub struct PluginInfo {
    pub plugin_id: String,
    pub state: String,
    pub registered_at: u64,
    pub permissions: Vec<String>,
    /// owning device (D-02); "local" for host plugins
    pub device_id: String,
    /// last ping/pong of the owning device, unix millis (D-04)
    pub last_seen: u64,
}

pub async fn list_plugins(State(state): State<Arc<AppState>>) -> Json<Vec<PluginInfo>> {
    let plugins = state
        .manager
        .list()
        .into_iter()
        .map(|e| {
            let last_seen = state
                .manager
                .registry()
                .get_device(&e.device_id)
                .map(|d| d.last_seen)
                .unwrap_or(0);
            PluginInfo {
                plugin_id: e.plugin_id.clone(),
                state: crate::plugins::registry::plugin_state_str(&e.state).to_string(),
                registered_at: e.registered_at,
                permissions: e.manifest.permissions.clone(),
                device_id: e.device_id.clone(),
                last_seen,
            }
        })
        .collect();
    Json(plugins)
}

pub async fn get_plugin(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<PluginInfo>, StatusCode> {
    state
        .manager
        .get(&id)
        .map(|e| {
            let last_seen = state
                .manager
                .registry()
                .get_device(&e.device_id)
                .map(|d| d.last_seen)
                .unwrap_or(0);
            Json(PluginInfo {
                plugin_id: e.plugin_id.clone(),
                state: crate::plugins::registry::plugin_state_str(&e.state).to_string(),
                registered_at: e.registered_at,
                permissions: e.manifest.permissions.clone(),
                device_id: e.device_id.clone(),
                last_seen,
            })
        })
        .ok_or(StatusCode::NOT_FOUND)
}

#[derive(Serialize)]
pub struct DeviceInfoView {
    pub device_id: String,
    pub os: String,
    pub arch: String,
    pub os_version: String,
    pub capabilities: Vec<String>,
    /// unix millis of the last ping/pong from any plugin on the device
    pub last_seen: u64,
    pub state: String,
    /// E-01 (v1.7): pairing lifecycle, unix millis; 0 = unpaired/unknown
    pub created: u64,
    pub expires: u64,
}

// D-04: discovery surface — the registry's device map as a serializable view
pub async fn list_devices(State(state): State<Arc<AppState>>) -> Json<Vec<DeviceInfoView>> {
    let devices = state.manager.registry().list_devices();
    Json(
        devices
            .into_iter()
            .map(|d| DeviceInfoView {
                device_id: d.device_id,
                os: crate::api::display::device_os_str(d.os).to_string(),
                arch: d.arch,
                os_version: d.os_version,
                capabilities: d.capabilities,
                last_seen: d.last_seen,
                state: crate::api::display::device_state_str(d.state).to_string(),
                created: d.created,
                expires: d.expires,
            })
            .collect(),
    )
}

/// Spawn a plugin declared under `plugins:` in config.yaml. 404 when `id`
/// isn't declared there (this never runs an arbitrary binary path);
/// 409 when it's already supervised. Errors carry the JSON envelope (UX-1).
pub async fn start_plugin(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    if state.manager.is_supervised(&id) {
        return Err(ApiError::conflict(&format!(
            "plugin '{id}' is already running"
        )));
    }
    let def = match state.plugin_defs.iter().find(|d| d.id == id) {
        Some(d) => d,
        None => {
            return Err(ApiError::not_found(&format!(
                "plugin '{id}' is not declared in config"
            )))
        }
    };
    match validate_plugin_def(def) {
        Ok(_) => {}
        Err(VynkorError::PermissionDenied(_)) => {
            return Err(ApiError::forbidden(
                "plugin requests a permission not granted by config",
            ))
        }
        Err(_) => {
            return Err(ApiError::unprocessable(
                "plugin definition failed validation",
            ))
        }
    }
    let config = PluginLoader::config_from_def(def);
    match state.manager.start(config).await {
        Ok(_) => Ok(StatusCode::OK),
        Err(_) => Err(ApiError::unprocessable(&format!(
            "failed to start plugin '{id}'"
        ))),
    }
}

/// Stop a supervised plugin and remove its registration (manager unregisters
/// regardless of the stop outcome). 404 when `id` has no supervised process —
/// including one that exited between the registry check and the stop.
pub async fn stop_plugin(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    if state.manager.get(&id).is_none() {
        return Err(ApiError::not_found(&format!(
            "plugin '{id}' is not registered"
        )));
    }
    match state.manager.stop(&id).await {
        Ok(()) => Ok(StatusCode::OK),
        // ux-1: race with plugin exit is a miss, not a success
        Err(VynkorError::PluginNotFound(_)) => Err(ApiError::not_found(&format!(
            "plugin '{id}' exited before stop completed"
        ))),
        Err(_) => Err(ApiError::unprocessable(&format!(
            "failed to stop plugin '{id}'"
        ))),
    }
}

pub async fn restart_plugin(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    if state.manager.get(&id).is_none() {
        return Err(ApiError::not_found(&format!(
            "plugin '{id}' is not registered"
        )));
    }
    match state.manager.restart(&id).await {
        Ok(()) => Ok(StatusCode::ACCEPTED),
        Err(_) => Err(ApiError::unprocessable(&format!(
            "failed to restart plugin '{id}'"
        ))),
    }
}

/// Upper bound on `?lines=` regardless of the plugin's ring-buffer capacity —
/// keeps the query param from being read as an implicit trust of the caller
/// rather than an explicit contract (T-10).
const MAX_LOG_LINES: usize = 10_000;

#[derive(Deserialize)]
pub struct LogsQuery {
    pub lines: Option<usize>,
}

pub async fn get_plugin_logs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<LogsQuery>,
) -> Result<Json<Vec<String>>, (StatusCode, Json<ApiError>)> {
    if state.manager.get(&id).is_none() && !state.manager.is_supervised(&id) {
        return Err(ApiError::not_found(&format!(
            "plugin '{id}' is not registered"
        )));
    }
    let n = q.lines.unwrap_or(100).min(MAX_LOG_LINES);
    Ok(Json(state.manager.logs(&id, n).await))
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ApiError>)>;

fn pairing_of(state: &AppState) -> Result<&PairingService, (StatusCode, Json<ApiError>)> {
    state
        .pairing
        .as_deref()
        .ok_or_else(|| ApiError::unavailable("pairing requires jwt_secret in config"))
}

// store/io details stay in the log, never in the response
fn pairing_error(e: VynkorError) -> (StatusCode, Json<ApiError>) {
    match e {
        // unknown and forged are indistinguishable on purpose
        VynkorError::Ticket(TicketRejection::Unknown) => ApiError::not_found("unknown ticket"),
        VynkorError::Ticket(TicketRejection::AlreadyUsed) => {
            ApiError::conflict("ticket already used")
        }
        VynkorError::Ticket(TicketRejection::Expired) => ApiError::gone("ticket expired"),
        VynkorError::InvalidInput(msg) | VynkorError::NetworkError(msg) => {
            ApiError::unprocessable(&msg)
        }
        other => {
            tracing::error!("pairing failed: {other}");
            ApiError::internal("pairing failed")
        }
    }
}

/// CD-01: mint a single-use pairing ticket (admin only). Body is optional.
pub async fn pair_device(
    State(state): State<Arc<AppState>>,
    body: Option<Json<PairRequest>>,
) -> ApiResult<TicketView> {
    let pairing = pairing_of(&state)?;
    let req = body.map(|Json(b)| b).unwrap_or_default();
    pairing.create_ticket(req).map(Json).map_err(pairing_error)
}

#[derive(Deserialize)]
pub struct ConsumeRequest {
    pub ticket: String,
}

/// CD-01: trade a ticket for a per-device credential. Unauthenticated — the
/// ticket is the credential — but globally rate-limited.
pub async fn consume_ticket(
    State(state): State<Arc<AppState>>,
    body: Result<Json<ConsumeRequest>, JsonRejection>,
) -> ApiResult<PairPayload> {
    let pairing = pairing_of(&state)?;
    if !pairing.allow_consume() {
        return Err(ApiError::too_many_requests("too many pairing attempts"));
    }
    let Json(req) =
        body.map_err(|_| ApiError::bad_request("body must be {\"ticket\": \"...\"}"))?;
    pairing
        .consume(&req.ticket)
        .map(Json)
        .map_err(pairing_error)
}
