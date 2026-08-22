use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

use crate::api::routes::AppState;
use crate::auth::jwt::PluginClaims;
use crate::proto::vynkor::PermissionType;

/// Verified JWT subject, inserted into request extensions by `auth_middleware`
/// after signature validation. Downstream layers (e.g. rate limiting) must key
/// on this, never on an unverified token field (BUG-004).
#[derive(Clone)]
pub struct VerifiedSub(pub String);

/// Verified JWT permission claims, inserted alongside `VerifiedSub`. Routes
/// that need authorization (not just authentication) key on this — never on
/// an unverified/unchecked claim (T-01).
#[derive(Clone)]
pub struct VerifiedPermissions(pub Vec<String>);

pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let validator = match &state.jwt_validator {
        Some(v) => v,
        None => return Ok(next.run(request).await),
    };

    let token = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let claims: PluginClaims = validator
        .validate(token)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    request
        .extensions_mut()
        .insert(VerifiedPermissions(claims.permissions.clone()));
    request.extensions_mut().insert(VerifiedSub(claims.sub));

    Ok(next.run(request).await)
}

/// Gate plugin lifecycle routes (start/stop/restart) on `PERMISSION_KERNEL_ADMIN`,
/// mirroring the IPC `KernelCommand` check (`src/ipc/protocol.rs:536`). Without
/// this, `auth_middleware` proved the caller holds *a* valid JWT but never
/// checked what it's authorized for — any plugin's token could stop/start/
/// restart any other plugin (T-01).
///
/// No `VerifiedPermissions` extension present means JWT auth is disabled for
/// this deployment (`jwt_validator: None`); `auth_middleware` already let the
/// request through unauthenticated, so this layer is a no-op in that mode.
pub async fn require_kernel_admin(
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    if let Some(VerifiedPermissions(perms)) = request.extensions().get::<VerifiedPermissions>() {
        let required = PermissionType::PermissionKernelAdmin.as_str_name();
        if !perms.iter().any(|p| p == required) {
            return Err(StatusCode::FORBIDDEN);
        }
    }

    Ok(next.run(request).await)
}
