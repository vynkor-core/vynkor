//! CD-08: `vyn tls status` — what cert the gateway serves and its sha-256
//! fingerprint, so an operator can confirm a phone pinned the right one
//! without reaching for openssl. Reads config + the cert file only; the
//! kernel need not be running.

use clap::Subcommand;

use crate::utils::config::{effective_tls_cert_path, load_config, Config};
use crate::utils::tls::cert_sha256_fingerprint;

#[derive(Subcommand)]
pub enum TlsCmd {
    /// Show whether TLS is on, which cert is served, and its SHA-256
    /// fingerprint (same format as `openssl x509 -fingerprint -sha256`).
    Status,
}

pub fn handle(cmd: TlsCmd, config_path: &str) -> anyhow::Result<()> {
    match cmd {
        TlsCmd::Status => {
            let cfg = load_config(config_path)?;
            print!("{}", status_report(&cfg));
            Ok(())
        }
    }
}

fn status_report(cfg: &Config) -> String {
    if !cfg.tls {
        return "tls: off — gateway serves plaintext ws://; see docs/TLS.md before \
                exposing it beyond loopback\n"
            .to_string();
    }
    let source = if cfg.tls_cert_path.is_some() {
        "configured"
    } else {
        "auto-generated self-signed"
    };
    let Some(path) = effective_tls_cert_path(cfg) else {
        return "tls: on\ncert: unresolved — no private dir for the auto-generated pair\n"
            .to_string();
    };
    let mut out = format!("tls: on\ncert: {} ({source})\n", path.display());
    match std::fs::read_to_string(&path) {
        Ok(pem) => match cert_sha256_fingerprint(&pem) {
            Ok(fp) => out.push_str(&format!("sha256: {fp}\n")),
            Err(e) => out.push_str(&format!("sha256: unreadable ({e})\n")),
        },
        // the self-signed pair is generated on first `vyn start`
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            out.push_str("sha256: - (cert not generated yet — start the kernel once)\n")
        }
        Err(e) => out.push_str(&format!("sha256: unreadable ({e})\n")),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_says_plaintext() {
        let cfg = Config {
            tls: false,
            ..Config::default()
        };
        assert!(status_report(&cfg).starts_with("tls: off"));
    }

    #[test]
    fn configured_cert_prints_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .unwrap()
            .cert;
        let path = dir.path().join("cert.pem");
        std::fs::write(&path, cert.pem()).unwrap();
        let cfg = Config {
            tls: true,
            tls_cert_path: Some(path),
            ..Config::default()
        };
        let report = status_report(&cfg);
        let fp = cert_sha256_fingerprint(&cert.pem()).unwrap();
        assert!(report.contains("(configured)"));
        assert!(report.contains(&format!("sha256: {fp}")));
    }

    #[test]
    fn missing_cert_is_reported_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            tls: true,
            tls_cert_path: Some(dir.path().join("nope.pem")),
            ..Config::default()
        };
        assert!(status_report(&cfg).contains("not generated yet"));
    }
}
