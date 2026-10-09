//! What the owner's window is shown and what its three buttons do, with a
//! real session underneath and no window.

use std::{collections::BTreeMap, time::Duration};

use devshare_app::session::{Session, Snapshot, Update};
use devshare_core::{
    environment::{Config, EnvironmentDef, ServiceDef},
    guest::{End, GuestLink, JoinError},
    host::ShareOptions,
    invite::DeviceSecret,
    protocol::{Device, EndReason, RejectReason},
};
use tokio::{net::TcpListener, sync::mpsc};

struct Window {
    updates: mpsc::UnboundedReceiver<Update>,
}

impl Window {
    /// The next picture of the session for which `wanted` holds.
    async fn until(&mut self, wanted: impl Fn(&Snapshot) -> bool) -> Snapshot {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                match self.updates.recv().await.expect("the session went silent") {
                    Update::Session(snapshot) if wanted(&snapshot) => return *snapshot,
                    Update::Session(_) => {}
                    Update::Ended(reason) => panic!("the session ended: {reason}"),
                }
            }
        })
        .await
        .expect("the window never showed what was expected")
    }

    async fn ended(&mut self) -> String {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Update::Ended(reason) = self.updates.recv().await.unwrap() {
                    return reason;
                }
            }
        })
        .await
        .unwrap()
    }
}

fn guest(user: &str, computer: &str) -> Device {
    Device {
        name: computer.into(),
        platform: "macos".into(),
        user: Some(user.into()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_owner_sees_logins_and_disconnects_one_guest() {
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
                services: vec![
                    ServiceDef {
                        host: "shop.test".into(),
                        port: 80,
                        target: Some(service.local_addr().unwrap().to_string()),
                        kind: None,
                    },
                    // Nothing listens there.
                    ServiceDef {
                        host: "shop.test".into(),
                        port: 5173,
                        target: Some("127.0.0.1:9".into()),
                        kind: None,
                    },
                ],
                launch: Vec::new(),
            },
        )]),
    };

    let (sender, updates) = mpsc::unbounded_channel();
    let mut window = Window { updates };
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

    // Before anyone joins: an invitation, a QR code, what is shared.
    let empty = window.until(|_| true).await;
    assert!(empty.guests.is_empty() && empty.invitation_open);
    assert_eq!(empty.code.len(), 9);
    assert!(empty.link.starts_with("http://") && empty.link.contains(&format!("/{}#", empty.code)));
    assert!(empty.qr.starts_with("<?xml") || empty.qr.starts_with("<svg"));
    // The tests run without a relay: the invitation cannot leave this network.
    assert!(!empty.anywhere);
    let services = &empty.environments[0].services;
    assert_eq!(services[0].address, "shop.test:80");
    assert!(services[0].warning.is_none());
    assert!(services[1]
        .warning
        .as_ref()
        .unwrap()
        .contains("did not answer"));

    // Two people join: the owner sees their logins and their computers.
    let alices_mac = DeviceSecret::random();
    let mut alice = GuestLink::join_as(
        &empty.code,
        &server,
        guest("alice", "Alices-MacBook"),
        &alices_mac,
    )
    .await
    .unwrap();
    let bob = GuestLink::join(&empty.link, &server, guest("bob", "bob-thinkpad"))
        .await
        .unwrap();
    let both = window.until(|snapshot| snapshot.guests.len() == 2).await;
    let seen: Vec<(Option<&str>, &str)> = both
        .guests
        .iter()
        .map(|guest| (guest.user.as_deref(), guest.computer.as_str()))
        .collect();
    assert_eq!(
        seen,
        [
            (Some("alice"), "Alices-MacBook"),
            (Some("bob"), "bob-thinkpad")
        ]
    );
    assert!(both
        .notices
        .iter()
        .any(|notice| notice.text == "alice on Alices-MacBook joined."));

    // The owner disconnects Alice. Bob stays, the invitation stays open, and
    // Alice's device cannot come back.
    assert!(session.disconnect(alice.guest_id).await);
    assert!(!session.disconnect(99).await);
    let end = tokio::time::timeout(Duration::from_secs(5), alice.ended())
        .await
        .unwrap();
    assert_eq!(end, End::Host(EndReason::Revoked));
    let after = window.until(|snapshot| snapshot.guests.len() == 1).await;
    assert!(after.invitation_open);
    assert_eq!(after.guests[0].user.as_deref(), Some("bob"));
    assert!(after
        .notices
        .iter()
        .any(|notice| notice.text == "You disconnected alice on Alices-MacBook."));
    assert!(matches!(
        GuestLink::join_as(
            &empty.code,
            &server,
            guest("alice", "Alices-MacBook"),
            &alices_mac
        )
        .await,
        Err(JoinError::Rejected(RejectReason::Revoked))
    ));

    // A new invitation: another code, the first one retired.
    session.invite().await.unwrap();
    let invited = window
        .until(|snapshot| snapshot.invitation_open && snapshot.code != empty.code)
        .await;
    assert_ne!(invited.qr, empty.qr);
    let carol = GuestLink::join(&invited.code, &server, guest("carol", "carol-pc"))
        .await
        .unwrap();
    window.until(|snapshot| snapshot.guests.len() == 2).await;

    // Stopping tells the window, and the guests.
    let mut bob = bob;
    session.stop().await;
    assert_eq!(window.ended().await, "you stopped sharing");
    let end = tokio::time::timeout(Duration::from_secs(5), bob.ended())
        .await
        .unwrap();
    assert_eq!(end, End::Host(EndReason::Stopped));
    carol.close().await;
}
