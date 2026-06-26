use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize)]
pub struct PluginClaims {
    pub sub: String,
    pub permissions: Vec<String>,
    #[serde(default)]
    pub ipc_targets: Vec<String>,
    pub exp: usize,
    pub iat: usize,
}

pub struct JwtValidator {
    decoding_key: DecodingKey,
    validation: Validation,
}

impl JwtValidator {
    pub fn new(secret: &[u8]) -> Self {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;
        Self {
            decoding_key: DecodingKey::from_secret(secret),
            validation,
        }
    }

    pub fn validate(&self, token: &str) -> Result<PluginClaims, String> {
        if token.is_empty() {
            return Err("missing JWT token".into());
        }
        decode::<PluginClaims>(token, &self.decoding_key, &self.validation)
            .map(|d| d.claims)
            .map_err(|e| e.to_string())
    }
}

#[allow(dead_code)]
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
