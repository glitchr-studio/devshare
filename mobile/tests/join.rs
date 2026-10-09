//! The mobile bridge as an app uses it: join with the invitation, then go
//! through the proxy the app's browser is given, over the real link.

use std::{collections::BTreeMap, net::SocketAddr, time::Duration};

use devshare_core::{
    environment::{Config, EnvironmentDef, ServiceDef},
    host::{Share, ShareOptions},
};
use devshare_mobile::join;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// A web server that answers every request with the name it was given.
async fn site(name: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = [0u8; 2048];
                let read = stream.read(&mut request).await.unwrap_or(0);
                let first = String::from_utf8_lossy(&request[..read])
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let body = format!("{name} answered {first}");
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(answer.as_bytes()).await.ok();
            });
        }
    });
    address
}

#[tokio::test(flavor = "multi_thread")]
async fn a_phone_joins_and_its_browser_reaches_the_service_through_the_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(devshare_server::serve(listener));
    let shop = site("shop").await;
    let config = Config {
        server: None,
        environments: BTreeMap::from([(
            "shop".to_string(),
            EnvironmentDef {
                entrypoint: Some("http://shop.test/cart".into()),
                services: vec![ServiceDef {
                    host: "shop.test".into(),
                    port: 80,
                    target: Some(shop.to_string()),
                    kind: None,
                }],
                launch: Vec::new(),
            },
        )]),
    };
    let share = Share::start(ShareOptions {
        selection: config.select(&[]).unwrap(),
        lifetime: Duration::from_secs(120),
        max_guests: 2,
        server,
        join: None,
    })
    .await
    .unwrap();

    let data = std::env::temp_dir().join(format!("devshare-mobile-{}", std::process::id()));
    let session = join(
        share.link(),
        "Marco's iPhone".into(),
        data.display().to_string(),
    )
    .await
    .unwrap();
    assert!(
        data.join("device-secret").is_file(),
        "a lasting identity, in the app's folder"
    );

    let environments = session.environments();
    assert_eq!(
        environments[0].entrypoint.as_deref(),
        Some("http://shop.test/cart")
    );
    assert_eq!(environments[0].services[0].url, "http://shop.test");
    assert!(session.may_open("http://shop.test/cart".into()));
    assert!(!session.may_open("https://glitchr.dev".into()));
    assert!(session.is_shared("Shop.Test".into()) && !session.is_shared("glitchr.dev".into()));
    assert!(!session.certificate_matches("shop.test".into(), 80, vec![1, 2, 3]));

    // The browser, through the proxy, with its credentials.
    let mut browser = TcpStream::connect(("127.0.0.1", session.proxy_port()))
        .await
        .unwrap();
    let credentials = base64_basic(&session.proxy_user(), &session.proxy_password());
    let request = format!("GET http://shop.test/cart HTTP/1.1\r\nHost: shop.test\r\nProxy-Authorization: Basic {credentials}\r\n\r\n");
    browser.write_all(request.as_bytes()).await.unwrap();
    let mut answer = String::new();
    browser.read_to_string(&mut answer).await.unwrap();
    assert!(
        answer.ends_with("shop answered GET /cart HTTP/1.1"),
        "{answer}"
    );

    assert!(session.remaining_seconds() > 100);
    assert_eq!(session.ended(), None);
    session.leave().await;
    assert_eq!(session.ended().as_deref(), Some("you left"));
    share.stop().await;
    std::fs::remove_dir_all(&data).ok();
}

fn base64_basic(user: &str, password: &str) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD.encode(format!("{user}:{password}"))
}
