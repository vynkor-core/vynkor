use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use std::num::NonZeroU32;
use std::sync::Arc;

use crate::api::middleware::VerifiedSub;

pub type TokenRateLimiter = DefaultKeyedRateLimiter<String>;

pub fn build_rate_limiter(rps: u32, burst: u32) -> Arc<TokenRateLimiter> {
    let rps = NonZeroU32::new(rps.max(1)).unwrap();
    let burst = NonZeroU32::new(burst.max(1)).unwrap();
    let quota = Quota::per_second(rps).allow_burst(burst);
    Arc::new(RateLimiter::keyed(quota))
}

/// Axum middleware: rate-limits authenticated routes by JWT `sub` claim.
///
/// This layer must be mounted *inside* `auth_middleware` (i.e. auth runs
/// first) so it only ever sees requests that already passed signature
/// verification. The key comes from `VerifiedSub`, inserted into request
/// extensions by `auth_middleware` after validation — never from a
/// self-decoded, unverified token field, which an attacker could rotate or
/// forge to bypass or exhaust another plugin's quota (BUG-004).
pub async fn rate_limit_middleware(
    State(limiter): State<Arc<TokenRateLimiter>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if let Some(VerifiedSub(sub)) = request.extensions().get::<VerifiedSub>().cloned() {
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
