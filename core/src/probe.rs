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
/// How much of a first page is read, and how many addresses are reported.
const LARGEST_PAGE: u64 = 256 * 1024;
const MOST_REFERENCES: usize = 5;

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

/// The addresses meaning "this machine" that the first page of a service
/// points at: `localhost:5173`, `127.0.0.1:8025`. For a guest they are the
/// guest's own machine: a redirect there goes nowhere, a script or a style
/// from there does not load.
///
/// `authority` is the service as a guest names it, `shop.test:8443`.
pub async fn local_references(target: &str, authority: &str, tls: bool) -> Vec<String> {
    let page = tokio::time::timeout(TIMEOUT, first_page(target, authority, tls))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let page = String::from_utf8_lossy(&page);

    let mut found = Vec::new();
    for name in ["localhost", "127.0.0.1"] {
        for (at, _) in page.match_indices(&format!("//{name}")) {
            let rest = &page[at + 2 + name.len()..];
            // `//localhost.example` is somewhere else entirely.
            let port: String = match rest.strip_prefix(':') {
                Some(rest) => rest.chars().take_while(char::is_ascii_digit).collect(),
                None if rest
                    .starts_with(|c: char| c.is_ascii_alphanumeric() || c == '.' || c == '-') =>
                {
                    continue
                }
                None => String::new(),
            };
            let reference = match port.is_empty() {
                true => name.to_string(),
                false => format!("{name}:{port}"),
            };
            if !found.contains(&reference) {
                found.push(reference);
            }
        }
    }
    found.truncate(MOST_REFERENCES);
    found
}

/// What the service answers to a request for its first page, headers
/// included: a redirect counts as much as a link.
async fn first_page(target: &str, authority: &str, tls: bool) -> Option<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stream = TcpStream::connect(target).await.ok()?;
    let request = format!(
        "GET / HTTP/1.0\r\nHost: {authority}\r\nAccept: text/html\r\nUser-Agent: devshare\r\n\r\n"
    );
    let mut page = Vec::new();
    if tls {
        let host = authority
            .rsplit_once(':')
            .map_or(authority, |(host, _)| host);
        let server_name = ServerName::try_from(host.to_string()).ok()?;
        let seen = Arc::new(Recorder::new());
        let config = ClientConfig::builder_with_provider(seen.provider.clone())
            .with_safe_default_protocol_versions()
            .ok()?
            .dangerous()
            .with_custom_certificate_verifier(seen)
            .with_no_client_auth();
        let mut stream = TlsConnector::from(Arc::new(config))
            .connect(server_name, stream)
            .await
            .ok()?;
        stream.write_all(request.as_bytes()).await.ok()?;
        (&mut stream)
            .take(LARGEST_PAGE)
            .read_to_end(&mut page)
            .await
            .ok();
    } else {
        let mut stream = stream;
        stream.write_all(request.as_bytes()).await.ok()?;
        (&mut stream)
            .take(LARGEST_PAGE)
            .read_to_end(&mut page)
            .await
            .ok();
    }
    Some(page)
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
