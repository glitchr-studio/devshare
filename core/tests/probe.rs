//! What the host agent sees of a service before sharing it, and what of it
//! reaches the guests.

use std::{collections::BTreeMap, net::SocketAddr, sync::Arc, time::Duration};

use devshare_core::{
    environment::{Config, EnvironmentDef, ServiceDef},
    guest::GuestLink,
    host::{Share, ShareOptions},
    probe::{fingerprint, probe, Probe},
    protocol::Device,
};
use rustls::{crypto::ring::default_provider, pki_types::PrivatePkcs8KeyDer, ServerConfig};
use tokio::{io::AsyncWriteExt, net::TcpListener};
use tokio_rustls::TlsAcceptor;

/// A TLS service with a self-signed certificate for `name`. Returns where it
/// listens and the fingerprint of its certificate.
async fn tls_service(name: &str) -> (SocketAddr, String) {
    let certified = rcgen::generate_simple_self_signed(vec![name.to_string()]).unwrap();
    let certificate = certified.cert.der().clone();
    let key = PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
    let config = ServerConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], key.into())
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut stream) = acceptor.accept(stream).await {
                    stream.write_all(b"secure").await.ok();
                    stream.shutdown().await.ok();
                }
            });
        }
    });
    (address, fingerprint(&certificate))
}

/// A service that answers anything with an HTTP error, as a web server does
/// when it is sent a TLS handshake on its plain port.
async fn plain_service() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                stream
                    .write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n")
                    .await
                    .ok();
            });
        }
    });
    address
}

/// An address nothing listens on.
async fn closed_port() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_probe_tells_tls_from_plain_from_down() {
    let (secure, sha256) = tls_service("shop.test").await;

    assert_eq!(
        probe(&secure.to_string(), "shop.test").await,
        Probe::Tls {
            sha256: sha256.clone(),
            covers_host: true
        }
    );
    // The same certificate, shared under a name it was not issued for.
    assert_eq!(
        probe(&secure.to_string(), "admin.test").await,
        Probe::Tls {
            sha256,
            covers_host: false
        }
    );
    assert_eq!(
        probe(&plain_service().await.to_string(), "shop.test").await,
        Probe::Plain
    );
    assert_eq!(
        probe(&closed_port().await.to_string(), "shop.test").await,
        Probe::Down
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn guests_receive_the_fingerprint_the_host_saw() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(devshare_server::serve(listener));

    let (secure, sha256) = tls_service("shop.test").await;
    let (plain, closed) = (plain_service().await, closed_port().await);
    let def = |host: &str, port, target: SocketAddr| ServiceDef {
        host: host.into(),
        port,
        target: Some(target.to_string()),
    };
    let config = Config {
        server: None,
        environments: BTreeMap::from([(
            "shop".to_string(),
            EnvironmentDef {
                entrypoint: None,
                services: vec![
                    def("shop.test", 443, secure),
                    def("shop.test", 80, plain),
                    def("shop.test", 5173, closed),
                ],
            },
        )]),
    };

    let share = Share::start(ShareOptions {
        selection: config.select(&[]).unwrap(),
        lifetime: Duration::from_secs(60),
        max_guests: 1,
        server: server.clone(),
        join: None,
    })
    .await
    .unwrap();

    let seen: Vec<(u16, &Probe)> = share.checks().iter().map(|c| (c.port, &c.probe)).collect();
    assert!(matches!(
        seen[..],
        [
            (80, Probe::Plain),
            (
                443,
                Probe::Tls {
                    covers_host: true,
                    ..
                }
            ),
            (5173, Probe::Down)
        ]
    ));

    let device = Device {
        name: "guest".into(),
        platform: "test".into(),
        user: None,
    };
    let link = GuestLink::join(&share.code(), &server, device)
        .await
        .unwrap();
    let tls = |port| {
        let services = &link.manifest.environments["shop"].services;
        services
            .iter()
            .find(|service| service.port == port)
            .unwrap()
            .tls
            .clone()
    };
    assert_eq!(tls(443).unwrap().sha256, sha256);
    assert_eq!(tls(80), None);
    // A service that was down when sharing started is shared all the same.
    assert_eq!(tls(5173), None);

    link.close().await;
    share.stop().await;
}
