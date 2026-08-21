use crate::jwt_helper::{create_device_token, create_test_token};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use std::time::{SystemTime, UNIX_EPOCH};
use veyron::auth::jwt::{mint_device_token, JwtValidator, PluginClaims};

// 32 bytes — must satisfy MIN_JWT_SECRET_BYTES, which mint_device_token now
// enforces (MA-18)
const SECRET: &[u8] = b"test-secret-key-for-unit-tests-0";

fn unix_now() -> usize {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as usize
}

#[test]
fn valid_token_accepted_and_claims_extracted() {
    let validator = JwtValidator::new(SECRET);
    let token = create_test_token("plugin-a", vec!["PERMISSION_NETWORK".into()], SECRET, 3600);
    let claims = validator.validate(&token).expect("valid token must parse");
    assert_eq!(claims.sub, "plugin-a");
    assert!(claims
        .permissions
        .contains(&"PERMISSION_NETWORK".to_string()));
}

#[test]
fn expired_token_rejected() {
    let validator = JwtValidator::new(SECRET);
    let token = create_test_token("plugin-a", vec![], SECRET, -100);
    let err = validator.validate(&token);
    assert!(err.is_err(), "expired token must be rejected");
}

#[test]
fn wrong_secret_rejected() {
    let validator = JwtValidator::new(b"different-secret");
    let token = create_test_token("plugin-a", vec![], SECRET, 3600);
    let err = validator.validate(&token);
    assert!(
        err.is_err(),
        "token signed with wrong secret must be rejected"
    );
}

#[test]
fn empty_token_rejected() {
    let validator = JwtValidator::new(SECRET);
    let err = validator.validate("");
    assert!(err.is_err(), "empty token must be rejected");
}

#[test]
fn permissions_round_trip_through_claims() {
    let validator = JwtValidator::new(SECRET);
    let perms = vec![
        "PERMISSION_NETWORK".to_string(),
        "PERMISSION_FILES_READ".to_string(),
        "PERMISSION_AI".to_string(),
    ];
    let token = create_test_token("agent", perms.clone(), SECRET, 3600);
    let claims = validator.validate(&token).unwrap();
    assert_eq!(claims.permissions, perms);
}

#[test]
fn plugin_id_extractable_from_sub_claim() {
    let validator = JwtValidator::new(SECRET);
    let token = create_test_token("real-plugin", vec![], SECRET, 3600);
    let claims = validator.validate(&token).unwrap();
    // kernel checks claims.sub != plugin_id to prevent impersonation
    assert_ne!(claims.sub, "impersonator");
    assert_eq!(claims.sub, "real-plugin");
}

// ---- D-07: per-device mint + aud/exp/nonce validation ----

#[test]
fn mint_device_token_round_trips_claims() {
    let token = mint_device_token(
        SECRET,
        "phone-1",
        vec!["PERMISSION_IPC_SEND".into()],
        vec!["kernel".into()],
        3600,
        "veyron",
    )
    .unwrap();
    let validator = JwtValidator::new(SECRET);
    let claims = validator.validate(&token).unwrap();
    assert_eq!(claims.sub, "phone-1");
    assert_eq!(claims.aud.as_deref(), Some("veyron"));
    let nonce = claims
        .jti
        .as_deref()
        .expect("device token must carry a jti nonce");
    assert!(!nonce.is_empty());
    assert_eq!(claims.permissions, ["PERMISSION_IPC_SEND"]);
    assert_eq!(claims.ipc_targets, ["kernel"]);
    let now = unix_now();
    assert!(
        claims.exp > now && claims.exp <= now + 3600,
        "exp must be a short window from now, got exp={} now={}",
        claims.exp,
        now
    );
}

#[test]
fn minted_device_tokens_are_distinct() {
    let a = create_device_token("phone-1", vec![], SECRET, 3600);
    let b = create_device_token("phone-1", vec![], SECRET, 3600);
    assert_ne!(a, b, "each mint must carry a fresh jti nonce");
}

#[test]
fn mint_device_token_rejects_empty_device_and_zero_ttl() {
    assert!(mint_device_token(SECRET, " ", vec![], vec![], 3600, "veyron").is_err());
    assert!(mint_device_token(SECRET, "phone-1", vec![], vec![], 0, "veyron").is_err());
}

#[test]
fn mint_device_token_rejects_short_secret() {
    // MA-18: same MIN_JWT_SECRET_BYTES threshold as kernel boot
    assert!(mint_device_token(b"short", "phone-1", vec![], vec![], 3600, "veyron").is_err());
    assert!(mint_device_token(&[b'x'; 31], "phone-1", vec![], vec![], 3600, "veyron").is_err());
    assert!(mint_device_token(&[b'x'; 32], "phone-1", vec![], vec![], 3600, "veyron").is_ok());
}

#[test]
fn audience_enforced_when_configured() {
    let token = create_device_token("phone-1", vec![], SECRET, 3600);
    let matching = JwtValidator::with_audience(SECRET, Some("veyron".into()));
    assert!(matching.validate(&token).is_ok());

    let mismatched = JwtValidator::with_audience(SECRET, Some("other-hub".into()));
    assert!(mismatched.validate(&token).is_err());

    // pre-D-07 tokens carry no aud — rejected once an audience is configured
    let legacy = create_test_token("plugin-a", vec![], SECRET, 3600);
    assert!(mismatched.validate(&legacy).is_err());
}

#[test]
fn audience_scoped_token_without_nonce_rejected() {
    // hand-mint a token with aud but no jti — the validator must reject the
    // pair (a minted device token always carries both, D-07)
    let claims = PluginClaims {
        sub: "phone-1".into(),
        permissions: vec![],
        ipc_targets: vec![],
        exp: unix_now() + 3600,
        iat: unix_now(),
        aud: Some("veyron".into()),
        jti: None,
    };
    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(SECRET),
    )
    .unwrap();
    let validator = JwtValidator::new(SECRET);
    let err = validator.validate(&token).unwrap_err();
    assert!(err.contains("jti"), "got: {err}");
}

#[test]
fn legacy_token_without_aud_still_validates() {
    // pre-D-07 tokens (no aud/jti) must keep working against a plain validator
    let token = create_test_token("plugin-a", vec![], SECRET, 3600);
    assert!(JwtValidator::new(SECRET).validate(&token).is_ok());
}
