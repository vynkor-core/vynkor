use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

use crate::api::routes::AppState;
use crate::auth::device_store::{record_grants, DeviceStatus};
use crate::auth::grants::{Grant, Grants};
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

/// K3: who is calling, resolved per request from the verified `sub`.
/// A `sub` that is a known device is judged ONLY by the grants in its
/// `DeviceRecord` (re-read every request, so a revoke bites immediately); its
/// JWT permissions confer no host authority. Anything else (plugins, the
/// local CLI's short-lived admin token) keeps the JWT permission rules.
#[derive(Clone, Debug)]
pub enum Caller {
    Device { id: String, grants: Grants },
    Other,
}

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

    let caller = match &state.devices {
        Some(store) => match store.get(&claims.sub) {
            // fail closed: an unreadable store must not downgrade a device to
            // the permissive non-device path
            Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
            Ok(Some((row, _))) => {
                let grants = if row.status(now_secs()) == DeviceStatus::Active {
                    record_grants(&row)
                } else {
                    Grants::new()
                };
                Caller::Device {
                    id: claims.sub.clone(),
                    grants,
                }
            }
            Ok(None) => Caller::Other,
        },
        None => Caller::Other,
    };
    request.extensions_mut().insert(caller);
    request
        .extensions_mut()
        .insert(VerifiedPermissions(claims.permissions.clone()));
    request.extensions_mut().insert(VerifiedSub(claims.sub));

    Ok(next.run(request).await)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// K3 route guard. A device caller needs `grant` in its record. A non-device
/// caller keeps today's rule: `PERMISSION_KERNEL_ADMIN` when `other_needs_admin`
/// (lifecycle + pairing routes), plain authentication otherwise (read routes).
/// No `Caller` extension = JWT auth disabled for this deployment: no-op.
pub async fn require_grant(
    grant: Grant,
    other_needs_admin: bool,
    request: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let allowed = match request.extensions().get::<Caller>() {
        None => true,
        Some(Caller::Device { grants, .. }) => grants.contains(&grant),
        Some(Caller::Other) if other_needs_admin => {
            let required = PermissionType::PermissionKernelAdmin.as_str_name();
            request
                .extensions()
                .get::<VerifiedPermissions>()
                .is_some_and(|VerifiedPermissions(p)| p.iter().any(|x| x == required))
        }
        Some(Caller::Other) => true,
    };
    if !allowed {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(next.run(request).await)
}
