//! D-07: TLS material resolution. The network path (HTTP/WS gateway) is TLS
//! by default; a `role: host` kernel must never silently fall back to
//! plaintext because the operator forgot a cert pair.

use crate::utils::config::{default_tls_dir, Config};
use std::path::PathBuf;

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
    if cert_path.exists() && key_path.exists() {
        return Ok(()); // reuse across restarts — clients pin this cert
    }
    let hostname = std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".to_string());
    let rcgen::CertifiedKey { cert, key_pair } = rcgen::generate_simple_self_signed(vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        hostname,
    ])
    .map_err(|e| anyhow::anyhow!("self-signed cert generation failed: {e}"))?;
    std::fs::create_dir_all(dir)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(cert_path, cert.pem())?;
    std::fs::write(key_path, key_pair.serialize_pem())?;
    tracing::warn!(
        "no tls_cert_path/tls_key_path configured — generated a self-signed \
         cert at {} (clients must trust it explicitly)",
        cert_path.display()
    );
    Ok(())
}
