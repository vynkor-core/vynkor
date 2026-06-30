use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use std::num::NonZeroU32;
use std::sync::Arc;

use crate::auth::jwt::PluginClaims;

pub type TokenRateLimiter = DefaultKeyedRateLimiter<String>;

pub fn build_rate_limiter(rps: u32, burst: u32) -> Arc<TokenRateLimiter> {
    let rps = NonZeroU32::new(rps.max(1)).unwrap();
    let burst = NonZeroU32::new(burst.max(1)).unwrap();
    let quota = Quota::per_second(rps).allow_burst(burst);
    Arc::new(RateLimiter::keyed(quota))
}

/// Axum middleware: rate-limits authenticated routes by JWT `sub` claim.
///
/// Key is extracted from the bearer token without signature verification — auth
/// middleware (which runs after this layer) handles full validation. Requests
/// without a parseable sub bypass rate limiting and proceed to auth middleware
/// which will reject them.
pub async fn rate_limit_middleware(
    State(limiter): State<Arc<TokenRateLimiter>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if let Some(sub) = extract_sub(&request) {
        if limiter.check_key(&sub).is_err() {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(axum::http::header::RETRY_AFTER, "1")],
                "Too Many Requests",
            )
                .into_response();
        }
    }
    next.run(request).await
}

fn extract_sub(req: &Request<Body>) -> Option<String> {
    let token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))?;

    // Decode without signature check — only need `sub` for rate-limit keying.
    let mut validation = Validation::new(Algorithm::HS256);
    validation.insecure_disable_signature_validation();
    validation.validate_exp = false;

    jsonwebtoken::decode::<PluginClaims>(token, &DecodingKey::from_secret(&[]), &validation)
        .ok()
        .map(|d| d.claims.sub)
}
