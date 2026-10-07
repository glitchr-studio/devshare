//! TLS terminated on the guest's own device, for the programs of the device
//! that trust its certificate authority (see [`crate::ca`]).
//!
//! A program opening `https://shop.test` is answered with a certificate the
//! device mints for `shop.test`; the guest then reaches the host's service
//! over the session, as for any connection, and speaks TLS to it with a
//! client that accepts exactly the certificate the host saw at share time,
//! whoever issued it. The host stays a forwarder of bytes and the protocol
//! does not change. A service whose certificate changed since is refused.
//!
//! The device's client offers its application protocols (ALPN) first; they
//! are offered to the service, and the one it picks is the one the client
//! gets, so HTTP/2 stays HTTP/2.

use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, SystemTime},
};

use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{
        ring::default_provider, verify_tls12_signature, verify_tls13_signature, CryptoProvider,
    },
    pki_types::{CertificateDer, ServerName, UnixTime},
    server::{Acceptor, ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
    ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::{LazyConfigAcceptor, TlsConnector};

use devshare_protocol::{Manifest, Service};

use crate::{
    ca::{DeviceCa, Minter},
    probe::fingerprint,
};

/// What a tunnel needs to terminate TLS: the session's certificates.
pub struct Termination {
    minter: Minter,
    provider: Arc<CryptoProvider>,
}

impl Termination {
    pub fn new(minter: Minter) -> Self {
        Self {
            minter,
            provider: Arc::new(default_provider()),
        }
    }

    /// Whether connections to `name` are terminated here.
    pub fn covers(&self, name: &str) -> bool {
        self.minter.mints(name)
    }

    /// The termination of a session just joined, when this device has its
    /// own certificate authority and the computer trusts it. `domain` is
    /// the guest's own, besides the development domains. Without one,
    /// connections pass through untouched.
    pub fn for_session(manifest: &Manifest, domain: &str) -> Option<Self> {
        let ca = match DeviceCa::load(&[domain.to_string()]) {
            Ok(Some(ca)) => ca,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!("{error:#}");
                return None;
            }
        };
        if !ca.trusted() {
            return None;
        }
        let until = SystemTime::now() + Duration::from_secs(manifest.session.expires_in);
        match ca.minter(&manifest.hostnames(), until) {
            Ok(minter) => Some(Self::new(minter)),
            Err(error) => {
                tracing::warn!("no certificates for this session: {error:#}");
                None
            }
        }
    }

    /// The services of `manifest` this termination certifies.
    pub fn certified<'a>(&self, manifest: &'a Manifest) -> Vec<&'a Service> {
        manifest
            .environments
            .values()
            .flat_map(|environment| &environment.services)
            .filter(|service| service.tls.is_some() && self.covers(&service.host))
            .collect()
    }
}

#[derive(Debug)]
pub(crate) enum Bridged<E> {
    /// The service could not be reached.
    Unreachable(E),
    /// The service presented another certificate than the one the host saw.
    Changed,
    Failed(anyhow::Error),
}

impl<E> From<std::io::Error> for Bridged<E> {
    fn from(error: std::io::Error) -> Self {
        Self::Failed(error.into())
    }
}

/// Terminates the TLS connection `device` opened to `name`, and carries what
/// it says to the service `connect` reaches, which must present the
/// certificate whose SHA-256 is `pin`.
pub(crate) async fn bridge<D, O, E>(
    device: D,
    name: &str,
    pin: &str,
    termination: &Termination,
    connect: impl Future<Output = Result<O, E>>,
) -> Result<(), Bridged<E>>
where
    D: AsyncRead + AsyncWrite + Unpin,
    O: AsyncRead + AsyncWrite + Unpin,
{
    let start = LazyConfigAcceptor::new(Acceptor::default(), device).await?;
    let offered: Vec<Vec<u8>> = start
        .client_hello()
        .alpn()
        .map(|protocols| protocols.map(<[u8]>::to_vec).collect())
        .unwrap_or_default();
    let leaf = termination.minter.leaf(name).map_err(Bridged::Failed)?;

    let origin = connect.await.map_err(Bridged::Unreachable)?;
    let pinned = Arc::new(Pinned {
        sha256: pin.to_ascii_lowercase(),
        provider: termination.provider.clone(),
        mismatch: AtomicBool::new(false),
    });
    let mut client = ClientConfig::builder_with_provider(termination.provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|error| Bridged::Failed(error.into()))?
        .dangerous()
        .with_custom_certificate_verifier(pinned.clone())
        .with_no_client_auth();
    client.alpn_protocols = offered;
    let server_name =
        ServerName::try_from(name.to_string()).map_err(|error| Bridged::Failed(error.into()))?;
    let mut origin = match TlsConnector::from(Arc::new(client))
        .connect(server_name, origin)
        .await
    {
        Ok(origin) => origin,
        Err(_) if pinned.mismatch.load(Ordering::Relaxed) => return Err(Bridged::Changed),
        Err(error) => return Err(error.into()),
    };

    let chosen = origin.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
    let mut server = ServerConfig::builder_with_provider(termination.provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|error| Bridged::Failed(error.into()))?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(Single(leaf)));
    server.alpn_protocols = chosen.into_iter().collect();
    let mut device = start.into_stream(Arc::new(server)).await?;

    match tokio::io::copy_bidirectional(&mut device, &mut origin).await {
        // Browsers often close without saying so in TLS first: an ending
        // like any other once the handshakes are done.
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => Ok(()),
    }
}

/// The session's certificate for the name the connection was made to.
#[derive(Debug)]
struct Single(Arc<CertifiedKey>);

impl ResolvesServerCert for Single {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }
}

/// Accepts the one certificate the host saw, and checks that the service
/// holds its key, as any TLS client does.
#[derive(Debug)]
struct Pinned {
    sha256: String,
    provider: Arc<CryptoProvider>,
    mismatch: AtomicBool,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if fingerprint(end_entity) == self.sha256 {
            return Ok(ServerCertVerified::assertion());
        }
        self.mismatch.store(true, Ordering::Relaxed);
        Err(rustls::Error::InvalidCertificate(
            rustls::CertificateError::ApplicationVerificationFailure,
        ))
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

#[cfg(test)]
mod tests {
    use rcgen::{CertifiedKey as Generated, KeyPair};
    use rustls::{pki_types::PrivateKeyDer, RootCertStore};
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};
    use tokio_rustls::TlsAcceptor;

    use super::*;
    use crate::ca::DeviceCa;

    /// A service of the host's: TLS with a certificate of its own, speaking
    /// HTTP/1.1 only, echoing what it reads after a greeting.
    fn origin(certificate: &Generated<KeyPair>) -> (DuplexStream, tokio::task::JoinHandle<()>) {
        let mut config = ServerConfig::builder_with_provider(Arc::new(default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.cert.der().clone()],
                PrivateKeyDer::try_from(certificate.signing_key.serialize_der()).unwrap(),
            )
            .unwrap();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let (ours, theirs) = duplex(64 * 1024);
        let served = tokio::spawn(async move {
            let Ok(mut stream) = TlsAcceptor::from(Arc::new(config)).accept(theirs).await else {
                return;
            };
            stream.write_all(b"origin:").await.unwrap();
            let mut buffer = [0u8; 64];
            let read = stream.read(&mut buffer).await.unwrap();
            stream.write_all(&buffer[..read]).await.unwrap();
            stream.shutdown().await.ok();
        });
        (ours, served)
    }

    /// A browser of the device: trusts the device's authority only.
    async fn browser(
        ca: &DeviceCa,
        stream: DuplexStream,
    ) -> std::io::Result<tokio_rustls::client::TlsStream<DuplexStream>> {
        let mut roots = RootCertStore::empty();
        roots.add(ca.certificate_der()).unwrap();
        let mut config = ClientConfig::builder_with_provider(Arc::new(default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("shop.test").unwrap(), stream)
            .await
    }

    fn termination(ca: &DeviceCa) -> Termination {
        let until = SystemTime::now() + Duration::from_secs(3600);
        Termination::new(ca.minter(&["shop.test".to_string()], until).unwrap())
    }

    #[tokio::test]
    async fn the_device_trusts_its_own_certificate_and_reaches_the_service_the_host_saw() {
        let ca = DeviceCa::generate(&[]).unwrap();
        let termination = termination(&ca);
        let service = rcgen::generate_simple_self_signed(["shop.test".to_string()]).unwrap();
        let pin = fingerprint(service.cert.der());
        let (to_origin, served) = origin(&service);

        let (device, ours) = duplex(64 * 1024);
        let bridged = tokio::spawn(async move {
            bridge(ours, "shop.test", &pin, &termination, async {
                Ok::<_, std::io::Error>(to_origin)
            })
            .await
        });

        let mut browser = browser(&ca, device)
            .await
            .expect("the device's certificate is trusted");
        assert_eq!(browser.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
        browser.write_all(b"hello").await.unwrap();
        let mut answer = String::new();
        browser.read_to_string(&mut answer).await.ok();
        assert_eq!(answer, "origin:hello");
        served.await.unwrap();
        drop(browser);
        let ended = bridged.await.unwrap();
        assert!(ended.is_ok(), "{ended:?}");
    }

    #[tokio::test]
    async fn a_service_whose_certificate_changed_since_share_time_is_refused() {
        let ca = DeviceCa::generate(&[]).unwrap();
        let termination = termination(&ca);
        let seen = rcgen::generate_simple_self_signed(["shop.test".to_string()]).unwrap();
        let now = rcgen::generate_simple_self_signed(["shop.test".to_string()]).unwrap();
        let pin = fingerprint(seen.cert.der());
        let (to_origin, _served) = origin(&now);

        let (device, ours) = duplex(64 * 1024);
        let bridged = tokio::spawn(async move {
            bridge(ours, "shop.test", &pin, &termination, async {
                Ok::<_, std::io::Error>(to_origin)
            })
            .await
        });

        assert!(browser(&ca, device).await.is_err());
        assert!(matches!(bridged.await.unwrap(), Err(Bridged::Changed)));
    }

    #[tokio::test]
    async fn a_service_out_of_reach_is_said_so() {
        let ca = DeviceCa::generate(&[]).unwrap();
        let termination = termination(&ca);
        let (device, ours) = duplex(64 * 1024);
        let bridged = tokio::spawn(async move {
            bridge(ours, "shop.test", "00", &termination, async {
                Err::<DuplexStream, _>("refused by the host")
            })
            .await
        });
        assert!(browser(&ca, device).await.is_err());
        assert!(matches!(
            bridged.await.unwrap(),
            Err(Bridged::Unreachable("refused by the host"))
        ));
    }
}
