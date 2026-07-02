use crate::jwt_helper::create_test_token;
use veyron::auth::jwt::JwtValidator;

const SECRET: &[u8] = b"test-secret-key-for-unit-tests";

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
