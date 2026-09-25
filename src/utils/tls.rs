//! D-07: TLS material resolution. The network path (HTTP/WS gateway) is TLS
//! by default; a `role: host` kernel must never silently fall back to
//! plaintext because the operator forgot a cert pair.

use crate::utils::config::{default_tls_dir, Config};
use std::path::PathBuf;

/// Pin rustls' process-wide crypto provider to aws-lc-rs. The dep tree
/// enables both `ring` and `aws-lc-rs`, so rustls 0.23 can't pick one and
/// the first TLS handshake/config build panics. Idempotent — a provider
/// already installed (by `main`, a test, or an embedder) is kept.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// Resolve the cert/key the gateway serves with. `tls: false` → no TLS.
/// Both configured → used as-is. Neither → a self-signed pair is generated
/// into `<private dir>/vynkor-tls/` on first start and reused after.
/// Only one configured → boot error (half-configured TLS is a silent
/// downgrade risk, so it must not be guessed).
pub fn resolve_tls_paths(config: &Config) -> anyhow::Result<(Option<PathBuf>, Option<PathBuf>)> {
    if !config.tls {
        return Ok((None, None));
    }
    match (&config.tls_cert_path, &config.tls_key_path) {
        (Some(cert), Some(key)) => Ok((Some(cert.clone()), Some(key.clone()))),
        (None, None) => {
            let dir = default_tls_dir().ok_or_else(|| {
                anyhow::anyhow!("cannot resolve a private dir for auto-generated TLS material")
            })?;
            let cert_path = dir.join("cert.pem");
            let key_path = dir.join("key.pem");
            ensure_self_signed(&dir, &cert_path, &key_path)?;
            Ok((Some(cert_path), Some(key_path)))
        }
        _ => anyhow::bail!(
            "tls is on but only one of tls_cert_path/tls_key_path is set — provide both or neither"
        ),
    }
}

fn ensure_self_signed(
    dir: &std::path::Path,
    cert_path: &std::path::Path,
    key_path: &std::path::Path,
) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;

    // every default-config kernel on this user (daemon, `cargo test` runs in
    // parallel) shares this dir — unserialized writers splice one run's cert
    // with another's key and the api then dies on KeyMismatch forever
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".gen.lock"))?;
    let _lock = nix::fcntl::Flock::lock(lock_file, nix::fcntl::FlockArg::LockExclusive)
        .map_err(|(_, e)| anyhow::anyhow!("locking {}: {e}", dir.display()))?;

    if cert_path.exists() && key_path.exists() {
        if pair_is_consistent(cert_path, key_path) {
            return Ok(()); // reuse across restarts — clients pin this cert
        }
        // only ever reached for the auto-generated pair, never operator certs
        tracing::warn!(
            "auto-generated tls pair in {} is unusable (cert/key mismatch or \
             unreadable) — regenerating; devices that pinned it must re-pair",
            dir.display()
        );
    }

    let hostname = std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".to_string());
    let rcgen::CertifiedKey { cert, key_pair } = rcgen::generate_simple_self_signed(vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        hostname,
    ])
    .map_err(|e| anyhow::anyhow!("self-signed cert generation failed: {e}"))?;
    // key first: a lock-free reader mid-regeneration then sees new key + old
    // cert (fails, retried on next start) rather than a torn file
    write_atomic(key_path, key_pair.serialize_pem().as_bytes(), 0o600)?;
    write_atomic(cert_path, cert.pem().as_bytes(), 0o644)?;
    tracing::warn!(
        "no tls_cert_path/tls_key_path configured — generated a self-signed \
         cert at {} (clients must trust it explicitly)",
        cert_path.display()
    );
    Ok(())
}

/// True when the pem pair loads and the key belongs to the leaf cert — the
/// same check rustls runs at serve time, done up front with an explicit
/// provider so it doesn't depend on the process default being installed.
fn pair_is_consistent(cert_path: &std::path::Path, key_path: &std::path::Path) -> bool {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let Ok(certs) =
        CertificateDer::pem_file_iter(cert_path).and_then(|it| it.collect::<Result<Vec<_>, _>>())
    else {
        return false;
    };
    let Ok(key) = PrivateKeyDer::from_pem_file(key_path) else {
        return false;
    };
    let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map(|b| b.with_no_client_auth().with_single_cert(certs, key).is_ok())
        .unwrap_or(false)
}

fn write_atomic(path: &std::path::Path, bytes: &[u8], mode: u32) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("pem.tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    // mode() only applies on create — a stale tmp keeps its old bits
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &std::path::Path) -> (PathBuf, PathBuf) {
        (dir.join("cert.pem"), dir.join("key.pem"))
    }

    #[test]
    fn concurrent_generation_yields_one_consistent_pair() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vyn-tls");
        let (cert, key) = paths(&dir);
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (dir, cert, key) = (dir.clone(), cert.clone(), key.clone());
                std::thread::spawn(move || ensure_self_signed(&dir, &cert, &key).unwrap())
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert!(pair_is_consistent(&cert, &key));
    }

    #[test]
    fn mismatched_auto_pair_is_regenerated() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vyn-tls");
        let (cert, key) = paths(&dir);
        ensure_self_signed(&dir, &cert, &key).unwrap();
        // splice in a key from a different pair — what a racing writer left
        let other = rcgen::generate_simple_self_signed(vec!["x".to_string()]).unwrap();
        std::fs::write(&key, other.key_pair.serialize_pem()).unwrap();
        assert!(!pair_is_consistent(&cert, &key));

        ensure_self_signed(&dir, &cert, &key).unwrap();
        assert!(pair_is_consistent(&cert, &key));
    }

    #[test]
    fn consistent_pair_is_reused_not_rotated() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vyn-tls");
        let (cert, key) = paths(&dir);
        ensure_self_signed(&dir, &cert, &key).unwrap();
        let before = std::fs::read(&cert).unwrap();
        ensure_self_signed(&dir, &cert, &key).unwrap();
        // clients pin this cert — a restart must not rotate it
        assert_eq!(std::fs::read(&cert).unwrap(), before);
    }

    #[test]
    fn private_key_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vyn-tls");
        let (cert, key) = paths(&dir);
        ensure_self_signed(&dir, &cert, &key).unwrap();
        let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
