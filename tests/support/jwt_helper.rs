//! Test-only JWT minting helper, shared by the unit and integration test
//! crates via `#[path]` include. Lives outside `src/` so no test-support
//! code is compiled into the production binary.

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use std::time::{SystemTime, UNIX_EPOCH};
use veyron::auth::jwt::PluginClaims;

pub fn create_test_token(
    plugin_id: &str,
    permissions: Vec<String>,
    secret: &[u8],
    exp_offset_secs: i64,
) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let claims = PluginClaims {
        sub: plugin_id.to_string(),
        permissions,
        ipc_targets: vec![],
        exp: (now + exp_offset_secs).max(0) as usize,
        iat: now as usize,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret),
    )
    .expect("test token encoding must not fail")
}
