//! The QR code of the app's window: drawn as the window draws it, read as a
//! camera reads it, and used by a guest to join.

use std::{collections::BTreeMap, time::Duration};

use devshare_app::session::{Session, Snapshot, Update};
use devshare_core::{
    environment::{Config, EnvironmentDef, ServiceDef},
    guest::GuestLink,
    host::ShareOptions,
    protocol::Device,
};
use tokio::{net::TcpListener, sync::mpsc};

/// The picture the window shows, as pixels, and what a camera reads in it.
fn scan(svg: &str) -> String {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).unwrap();
    let size = tree.size().to_int_size();
    let mut pixels = resvg::tiny_skia::Pixmap::new(size.width(), size.height()).unwrap();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::default(),
        &mut pixels.as_mut(),
    );

    let width = size.width() as usize;
    let grey: Vec<u8> = pixels
        .pixels()
        .iter()
        // Nothing drawn is the page's white.
        .map(|pixel| if pixel.alpha() == 0 { 255 } else { pixel.red() })
        .collect();
    let mut image =
        rqrr::PreparedImage::prepare_from_greyscale(width, grey.len() / width, |x, y| {
            grey[y * width + x]
        });
    let grids = image.detect_grids();
    assert_eq!(grids.len(), 1, "one QR code is expected in the picture");
    grids[0].decode().unwrap().1
}

/// The next picture of the session the window is sent.
async fn shown(updates: &mut mpsc::UnboundedReceiver<Update>) -> Snapshot {
    match tokio::time::timeout(Duration::from_secs(8), updates.recv()).await {
        Ok(Some(Update::Session(snapshot))) => *snapshot,
        Ok(Some(Update::Ended(reason))) => panic!("the session ended: {reason}"),
        _ => panic!("the window was shown nothing"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_windows_qr_code_lets_a_guest_in_and_changes_with_the_invitation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(devshare_server::serve(listener));
    let service = TcpListener::bind("127.0.0.1:0").await.unwrap();

    let config = Config {
        server: None,
        environments: BTreeMap::from([(
            "shop".to_string(),
            EnvironmentDef {
                entrypoint: None,
                services: vec![ServiceDef {
                    host: "shop.test".into(),
                    port: 80,
                    target: Some(service.local_addr().unwrap().to_string()),
                    kind: None,
                }],
                launch: Vec::new(),
            },
        )]),
    };
    let (sender, mut updates) = mpsc::unbounded_channel();
    let session = Session::start(
        ShareOptions {
            selection: config.select(&[]).unwrap(),
            lifetime: Duration::from_secs(120),
            max_guests: 3,
            server: server.clone(),
            join: None,
        },
        move |update| {
            sender.send(update).ok();
        },
    )
    .await
    .unwrap();
    // The picture says what the window says in words.
    let first = shown(&mut updates).await;
    let scanned = scan(&first.qr);
    assert_eq!(scanned, first.link);
    assert!(scanned.starts_with("http://") && scanned.contains(&format!("/{}#", first.code)));

    // A guest joins with the scanned text and nothing else.
    let phone = Device {
        name: "iPhone".into(),
        platform: "ios".into(),
        user: None,
    };
    let guest = GuestLink::join(&scanned, &server, phone.clone())
        .await
        .unwrap();
    loop {
        let snapshot = shown(&mut updates).await;
        if let Some(seen) = snapshot.guests.first() {
            assert_eq!(
                (seen.computer.as_str(), seen.platform.as_str()),
                ("iPhone", "ios")
            );
            break;
        }
    }

    // A new invitation is a new picture; the old one lets nobody in.
    session.invite().await.unwrap();
    let second = loop {
        let snapshot = shown(&mut updates).await;
        if snapshot.code != first.code {
            break snapshot;
        }
    };
    let rescanned = scan(&second.qr);
    assert_eq!(rescanned, second.link);
    assert_ne!(rescanned, scanned);
    assert!(GuestLink::join(&scanned, &server, phone.clone())
        .await
        .is_err());
    let other = GuestLink::join(&rescanned, &server, phone).await.unwrap();

    other.close().await;
    guest.close().await;
    session.stop().await;
}
