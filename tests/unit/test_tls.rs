use std::path::PathBuf;
use vynkor::utils::config::Config;
use vynkor::utils::tls::resolve_tls_paths;

#[test]
fn tls_off_returns_no_paths() {
    let cfg = Config {
        tls: false,
        ..Default::default()
    };
    let (cert, key) = resolve_tls_paths(&cfg).unwrap();
    assert!(cert.is_none() && key.is_none());
}

#[test]
fn configured_cert_and_key_are_returned_as_is() {
    let cfg = Config {
        tls: true,
        tls_cert_path: Some(PathBuf::from("/etc/vyn/cert.pem")),
        tls_key_path: Some(PathBuf::from("/etc/vyn/key.pem")),
        ..Default::default()
    };
    let (cert, key) = resolve_tls_paths(&cfg).unwrap();
    assert_eq!(cert, Some(PathBuf::from("/etc/vyn/cert.pem")));
    assert_eq!(key, Some(PathBuf::from("/etc/vyn/key.pem")));
}

#[test]
fn half_configured_tls_is_a_boot_error() {
    // tls on with only a cert is a silent-downgrade hazard — fail loudly
    let cfg = Config {
        tls: true,
        tls_cert_path: Some(PathBuf::from("/etc/vyn/cert.pem")),
        ..Default::default()
    };
    let err = resolve_tls_paths(&cfg).unwrap_err().to_string();
    assert!(err.contains("tls_cert_path"), "got: {err}");
}

#[test]
fn auto_generates_self_signed_pair_into_private_dir() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = dir.path().join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let cfg = Config {
        tls: true,
        ..Default::default()
    };
    temp_env::with_var("XDG_RUNTIME_DIR", Some(&runtime), || {
        let (cert, key) = resolve_tls_paths(&cfg).unwrap();
        let (cert, key) = (cert.unwrap(), key.unwrap());
        assert!(
            cert.starts_with(&runtime),
            "cert must land in the private dir"
        );
        let cert_pem = std::fs::read_to_string(&cert).unwrap();
        assert!(cert_pem.contains("BEGIN CERTIFICATE"));
        let key_pem = std::fs::read_to_string(&key).unwrap();
        assert!(key_pem.contains("BEGIN PRIVATE KEY"));

        // second call reuses the same pair rather than regenerating
        let (cert2, key2) = resolve_tls_paths(&cfg).unwrap();
        assert_eq!(cert2, Some(cert));
        assert_eq!(key2, Some(key));
    });
}
