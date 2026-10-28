//! pinned-cert TLS for the bridge's `wss://` (role: client). port of the
//! phone app's verifier: the host's served cert is the trust anchor.

use super::BridgeError;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, SignatureScheme};
use std::path::Path;
use std::sync::Arc;

/// trust = byte-equality of the end-entity cert with a pinned one, under any
/// name: the host is usually reached by a DHCP/tailnet ip that is not in the
/// cert's SANs. validity dates are ignored too (same as the android app), so
/// rotation = replace the pem. handshake signatures are still verified against
/// the cert key.
#[derive(Debug)]
struct PinnedCertVerifier {
    pinned: Vec<CertificateDer<'static>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for PinnedCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if self
            .pinned
            .iter()
            .any(|c| c.as_ref() == end_entity.as_ref())
        {
            Ok(ServerCertVerified::assertion())
        } else {
            // surfaced in the connect error so a rotated host cert is obvious
            Err(rustls::Error::InvalidCertificate(CertificateError::Other(
                rustls::OtherError(Arc::new(PinMismatch)),
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

struct PinMismatch;

// rustls renders `Other` errors via Debug, so Debug carries the message
impl std::fmt::Debug for PinMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::fmt::Display for PinMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "host certificate does not match the pinned ca_cert_path")
    }
}

impl std::error::Error for PinMismatch {}

pub(crate) fn pinned_client_config(pem: &str) -> Result<rustls::ClientConfig, BridgeError> {
    let mut pinned = Vec::new();
    for cert in CertificateDer::pem_slice_iter(pem.as_bytes()) {
        pinned.push(cert.map_err(|e| BridgeError::Connect(format!("bad cert pem: {e}")))?);
    }
    if pinned.is_empty() {
        return Err(BridgeError::Connect(
            "ca_cert_path contained no certificates".into(),
        ));
    }
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
    let verifier = PinnedCertVerifier {
        pinned,
        provider: provider.clone(),
    };
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| BridgeError::Connect(format!("tls config: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth())
}

pub(crate) fn load_pinned_config(path: &Path) -> Result<rustls::ClientConfig, BridgeError> {
    let pem = std::fs::read_to_string(path).map_err(|e| {
        BridgeError::Connect(format!("bridge ca_cert_path {}: {e}", path.display()))
    })?;
    pinned_client_config(&pem)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn self_signed(name: &str) -> rcgen::CertifiedKey<rcgen::KeyPair> {
        rcgen::generate_simple_self_signed(vec![name.to_string()]).unwrap()
    }

    #[test]
    fn pinned_config_rejects_pem_without_certs() {
        assert!(pinned_client_config("not a pem").is_err());
    }

    #[test]
    fn missing_ca_file_names_path() {
        let err = load_pinned_config(Path::new("/nonexistent/host-cert.pem")).unwrap_err();
        assert!(
            err.to_string().contains("/nonexistent/host-cert.pem"),
            "{err}"
        );
    }

    #[test]
    fn pinned_verifier_accepts_only_the_pinned_cert_under_any_name() {
        let a = self_signed("vynkor-host");
        let b = self_signed("vynkor-host");
        let verifier = PinnedCertVerifier {
            pinned: vec![a.cert.der().clone()],
            provider: Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
        };
        let name = ServerName::try_from("anything.example").unwrap();
        let now = UnixTime::now();
        assert!(verifier
            .verify_server_cert(a.cert.der(), &[], &name, &[], now)
            .is_ok());
        let err = verifier
            .verify_server_cert(b.cert.der(), &[], &name, &[], now)
            .unwrap_err();
        assert!(err.to_string().contains("pinned"), "{err}");
    }

    #[derive(Debug)]
    struct FixedCert(Arc<rustls::sign::CertifiedKey>);

    impl rustls::server::ResolvesServerCert for FixedCert {
        fn resolve(
            &self,
            _: rustls::server::ClientHello<'_>,
        ) -> Option<Arc<rustls::sign::CertifiedKey>> {
            Some(self.0.clone())
        }
    }

    /// handshake where the server serves `cert` but signs with `signing`;
    /// client pins `cert`. returns whether the client accepted.
    async fn handshake(
        cert: &rcgen::CertifiedKey<rcgen::KeyPair>,
        signing: &rcgen::CertifiedKey<rcgen::KeyPair>,
        version: &'static rustls::SupportedProtocolVersion,
    ) -> Result<(), String> {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let key = provider
            .key_provider
            .load_private_key(
                rustls::pki_types::PrivateKeyDer::try_from(signing.signing_key.serialize_der())
                    .unwrap(),
            )
            .unwrap();
        let certified = rustls::sign::CertifiedKey::new(vec![cert.cert.der().clone()], key);
        let server = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[version])
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(FixedCert(Arc::new(certified))));
        let verifier = PinnedCertVerifier {
            pinned: vec![cert.cert.der().clone()],
            provider: provider.clone(),
        };
        let client = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[version])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        let (c, s) = tokio::io::duplex(16 * 1024);
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
        tokio::spawn(async move {
            let _ = acceptor.accept(s).await;
        });
        tokio_rustls::TlsConnector::from(Arc::new(client))
            .connect(ServerName::try_from("anything.example").unwrap(), c)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    #[tokio::test]
    async fn handshake_signatures_are_verified() {
        let a = self_signed("vynkor-host");
        let b = self_signed("vynkor-host");
        for v in [&rustls::version::TLS12, &rustls::version::TLS13] {
            // cert signed by its own key: accepted
            handshake(&a, &a, v).await.expect("genuine handshake");
            // pinned cert presented, but the signature is from another key
            assert!(
                handshake(&a, &b, v).await.is_err(),
                "forged signature accepted"
            );
        }
    }
}
