use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::utils::errors::VynkorError;

/// T-12/MA-18: minimum `jwt_secret` length (bytes) accepted anywhere a JWT
/// secret is used — kernel boot (orchestrator) and every mint site. HS256
/// secrets shorter than this are brute-forceable.
pub const MIN_JWT_SECRET_BYTES: usize = 32;

#[derive(Debug, Serialize, Deserialize)]
pub struct PluginClaims {
    pub sub: String,
    pub permissions: Vec<String>,
    #[serde(default)]
    pub ipc_targets: Vec<String>,
    pub exp: usize,
    pub iat: usize,
    /// D-07 audience claim. Optional so pre-D-07 tokens keep validating;
    /// when `JwtValidator` is built with an audience, every accepted token
    /// must carry it. `mint_device_token` always sets it.
    #[serde(default)]
    pub aud: Option<String>,
    /// D-07 per-mint nonce. Audience-scoped tokens must carry one — the
    /// validator rejects `aud` without `jti` (distinctness + replay future).
    #[serde(default)]
    pub jti: Option<String>,
}

pub struct JwtValidator {
    decoding_key: DecodingKey,
    validation: Validation,
}

impl JwtValidator {
    pub fn new(secret: &[u8]) -> Self {
        Self::with_audience(secret, None)
    }

    /// `audience` (config `jwt_audience`): when set, every accepted token
    /// must carry exactly that `aud` claim. Device tokens minted by
    /// `vyn token mint` default their audience to this value.
    pub fn with_audience(secret: &[u8], audience: Option<String>) -> Self {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;
        if let Some(aud) = audience {
            validation.set_audience(&[aud]);
            // without this, jsonwebtoken only checks aud when the claim is
            // *present* — a token with no aud would pass. The operator set
            // jwt_audience to require it, so make the claim mandatory.
            validation.required_spec_claims.insert("aud".to_string());
        } else {
            // no required audience: a token that carries its own `aud` must
            // still validate (device tokens minted with a default audience)
            validation.validate_aud = false;
        }
        Self {
            decoding_key: DecodingKey::from_secret(secret),
            validation,
        }
    }

    pub fn validate(&self, token: &str) -> Result<PluginClaims, VynkorError> {
        if token.is_empty() {
            return Err(VynkorError::Auth("missing JWT token".into()));
        }
        let claims = decode::<PluginClaims>(token, &self.decoding_key, &self.validation)
            .map(|d| d.claims)
            .map_err(|e| VynkorError::Auth(e.to_string()))?;
        // D-07: audience-scoped tokens are per-device mints and must carry a
        // jti nonce. Pre-D-07 tokens carry neither claim and stay accepted.
        if claims.aud.is_some() && claims.jti.as_deref().unwrap_or("").is_empty() {
            return Err(VynkorError::Auth(
                "token carries aud but no jti nonce".into(),
            ));
        }
        Ok(claims)
    }
}

/// Mint a per-device JWT (D-07): `sub = device_id`, restricted permission
/// claims, an `aud` audience, a random `jti` nonce, and a short `exp`.
pub fn mint_device_token(
    secret: &[u8],
    device_id: &str,
    permissions: Vec<String>,
    ipc_targets: Vec<String>,
    ttl_secs: u64,
    audience: &str,
) -> Result<String, VynkorError> {
    // MA-18: same threshold as boot-time validation — `vyn token mint` reads
    // the secret straight from config with no orchestrator gate in between
    if secret.len() < MIN_JWT_SECRET_BYTES {
        return Err(VynkorError::Auth(format!(
            "jwt_secret is {} bytes, must be at least {MIN_JWT_SECRET_BYTES} bytes \
             (HS256 secrets shorter than this are brute-forceable)",
            secret.len()
        )));
    }
    let device_id = device_id.trim();
    if device_id.is_empty() {
        return Err(VynkorError::Auth("device_id must not be empty".into()));
    }
    if ttl_secs == 0 {
        return Err(VynkorError::Auth("ttl_secs must be > 0".into()));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as usize;
    let mut nonce = [0u8; 16];
    OsRng.fill_bytes(&mut nonce);
    let claims = PluginClaims {
        sub: device_id.to_string(),
        permissions,
        ipc_targets,
        exp: now + ttl_secs as usize,
        iat: now,
        aud: Some(audience.to_string()),
        jti: Some(nonce.iter().map(|b| format!("{b:02x}")).collect()),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret),
    )
    .map_err(|e| VynkorError::Auth(e.to_string()))
}
