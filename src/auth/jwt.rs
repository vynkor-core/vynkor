use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};

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
