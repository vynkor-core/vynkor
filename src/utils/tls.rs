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

/// SHA-256 over the DER of the first CERTIFICATE block, as colon-hex
/// uppercase — byte-for-byte what `openssl x509 -fingerprint -sha256`
/// prints, so an operator can compare it against the phone's pin by eye.
pub fn cert_sha256_fingerprint(pem: &str) -> anyhow::Result<String> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use sha2::{Digest, Sha256};

    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let start = pem
        .find(BEGIN)
        .ok_or_else(|| anyhow::anyhow!("no CERTIFICATE block in pem"))?
        + BEGIN.len();
    let len = pem[start..]
        .find(END)
        .ok_or_else(|| anyhow::anyhow!("unterminated CERTIFICATE block in pem"))?;
    let b64: String = pem[start..start + len]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let der = STANDARD
        .decode(b64)
        .map_err(|e| anyhow::anyhow!("bad base64 in CERTIFICATE block: {e}"))?;
    Ok(Sha256::digest(&der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_matches_sha256_of_der_in_openssl_format() {
        use sha2::{Digest, Sha256};
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .unwrap()
            .cert;
        let fp = cert_sha256_fingerprint(&cert.pem()).unwrap();
        let expected: Vec<String> = Sha256::digest(cert.der())
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        assert_eq!(fp, expected.join(":"));
        // 32 bytes → 32 pairs + 31 colons
        assert_eq!(fp.len(), 32 * 2 + 31);
    }

    #[test]
    fn fingerprint_uses_first_cert_of_a_chain() {
        let a = rcgen::generate_simple_self_signed(vec!["a".to_string()])
            .unwrap()
            .cert;
        let b = rcgen::generate_simple_self_signed(vec!["b".to_string()])
            .unwrap()
            .cert;
        let chain = format!("{}{}", a.pem(), b.pem());
        assert_eq!(
            cert_sha256_fingerprint(&chain).unwrap(),
            cert_sha256_fingerprint(&a.pem()).unwrap()
        );
    }

    #[test]
    fn fingerprint_rejects_non_cert_pem() {
        assert!(cert_sha256_fingerprint("-----BEGIN PRIVATE KEY-----\nAAAA\n").is_err());
        assert!(cert_sha256_fingerprint("-----BEGIN CERTIFICATE-----\nAAAA").is_err());
    }
}
