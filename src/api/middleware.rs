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

/// Verified JWT subject, inserted into request extensions by `auth_middleware`
/// after signature validation. Downstream layers (e.g. rate limiting) must key
/// on this, never on an unverified token field (BUG-004).
#[derive(Clone)]
pub struct VerifiedSub(pub String);

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

    request.extensions_mut().insert(VerifiedSub(claims.sub));

    Ok(next.run(request).await)
}
