//! What the host agent learns about a service before sharing it: whether it
//! answers, and the certificate it presents when it speaks TLS.
//!
//! The certificate's fingerprint travels in the manifest. A guest that does
//! not trust the host's development CA can still check that it reached the
//! very service the host looked at.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use rustls::{
    client::{
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        verify_server_name,
    },
    crypto::{
        ring::default_provider, verify_tls12_signature, verify_tls13_signature, CryptoProvider,
    },
    pki_types::{CertificateDer, ServerName, UnixTime},
    server::ParsedCertificate,
    ClientConfig, DigitallySignedStruct, SignatureScheme,
};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// Long enough for a local service, short enough not to delay sharing.
const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// Nothing accepts connections there.
    Down,
    /// Answers, and not with TLS.
    Plain,
    Tls {
        /// SHA-256 of the certificate, in hexadecimal.
        sha256: String,
        /// Whether the certificate is issued for the shared hostname.
        covers_host: bool,
    },
}

/// Connects to `target` the way a guest asking for `host` would.
pub async fn probe(target: &str, host: &str) -> Probe {
    let Ok(Ok(stream)) = tokio::time::timeout(TIMEOUT, TcpStream::connect(target)).await else {
        return Probe::Down;
    };
    let Ok(server_name) = ServerName::try_from(host.to_string()) else {
        return Probe::Plain;
    };

    let seen = Arc::new(Recorder::new());
    let config = ClientConfig::builder_with_provider(seen.provider.clone())
        .with_safe_default_protocol_versions()
        .expect("the default protocol versions are supported")
        .dangerous()
        .with_custom_certificate_verifier(seen.clone())
        .with_no_client_auth();

    let handshake = TlsConnector::from(Arc::new(config)).connect(server_name.clone(), stream);
    if !matches!(tokio::time::timeout(TIMEOUT, handshake).await, Ok(Ok(_))) {
        return Probe::Plain;
    }
    let Some(certificate) = seen.certificate.lock().unwrap().take() else {
        return Probe::Plain;
    };

    let covers_host = ParsedCertificate::try_from(&certificate)
        .and_then(|parsed| verify_server_name(&parsed, &server_name))
        .is_ok();
    Probe::Tls {
        sha256: fingerprint(&certificate),
        covers_host,
    }
}

pub fn fingerprint(certificate: &[u8]) -> String {
    Sha256::digest(certificate)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Accepts whatever certificate is presented and keeps it. This judges
/// nothing: the probe only reports what the service shows.
#[derive(Debug)]
struct Recorder {
    provider: Arc<CryptoProvider>,
    certificate: Mutex<Option<CertificateDer<'static>>>,
}

impl Recorder {
    fn new() -> Self {
        Self {
            provider: Arc::new(default_provider()),
            certificate: Mutex::new(None),
        }
    }
}

impl ServerCertVerifier for Recorder {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        *self.certificate.lock().unwrap() = Some(end_entity.clone().into_owned());
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let algorithms = &self.provider.signature_verification_algorithms;
        verify_tls12_signature(message, cert, dss, algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let algorithms = &self.provider.signature_verification_algorithms;
        verify_tls13_signature(message, cert, dss, algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
