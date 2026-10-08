//! A whole session between a host and its guests, over the real link and a
//! real control plane, without the guest's network interface.

use std::{collections::BTreeMap, net::SocketAddr, time::Duration};

use devshare_core::{
    environment::{Config, EnvironmentDef, ServiceDef},
    guest::{End, GuestLink, JoinError, OpenError, Opener},
    host::{Activity, Share, ShareOptions},
    invite::DeviceSecret,
    protocol::{Device, EndReason, RejectReason},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn control_plane() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(devshare_server::serve(listener));
    format!("http://{address}")
}

/// A service that says its name, then repeats what it is sent.
async fn service(name: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                stream.write_all(format!("{name}|").as_bytes()).await.ok();
                let (mut read, mut write) = stream.split();
                tokio::io::copy(&mut read, &mut write).await.ok();
            });
        }
    });
    address
}

struct Session {
    server: String,
    share: Share,
    /// Listens on the host but belongs to no environment of the session.
    private: SocketAddr,
}

async fn session(lifetime: Duration, max_guests: u32) -> Session {
    let server = control_plane().await;
    let (shop, api, private) = (
        service("shop").await,
        service("api").await,
        service("private").await,
    );

    let def = |host: &str, port, target: SocketAddr| ServiceDef {
        host: host.into(),
        port,
        target: Some(target.to_string()),
    };
    let config = Config {
        server: None,
        environments: BTreeMap::from([
            (
                "shop".to_string(),
                EnvironmentDef {
                    entrypoint: Some("https://shop.test".into()),
                    services: vec![def("shop.test", 443, shop), def("api.shop.test", 8080, api)],
                },
            ),
            (
                "infrastructure".to_string(),
                EnvironmentDef {
                    entrypoint: None,
                    services: vec![def("db.test", 5432, private)],
                },
            ),
        ]),
    };

    let share = Share::start(ShareOptions {
        selection: config.select(&["shop".to_string()]).unwrap(),
        lifetime,
        max_guests,
        server: server.clone(),
        join: None,
    })
    .await
    .unwrap();
    Session {
        server,
        share,
        private,
    }
}

fn device(name: &str) -> Device {
    Device {
        name: format!("{name}-laptop"),
        platform: "test".into(),
        user: Some(name.into()),
    }
}

async fn call(opener: &Opener, host: &str, port: u16, message: &str) -> Result<String, OpenError> {
    let (mut send, mut recv) = opener.open(host, port).await?;
    send.write_all(message.as_bytes()).await.unwrap();
    send.finish().unwrap();
    let mut answer = String::new();
    recv.read_to_string(&mut answer).await.unwrap();
    Ok(answer)
}

async fn next(share: &mut Share) -> Activity {
    tokio::time::timeout(Duration::from_secs(5), share.activity())
        .await
        .expect("the host reported nothing")
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guest_reaches_the_shared_services_under_their_hostnames() {
    let mut session = session(Duration::from_secs(60), 3).await;
    let code = session.share.code().to_string();

    // The code as the host displays it, with its dash, in lowercase.
    let pasted = format!("devshare://join/{}-{}", &code[..4], &code[4..]).to_lowercase();
    let link = GuestLink::join(&pasted, &session.server, device("alice"))
        .await
        .unwrap();

    assert!(matches!(
        next(&mut session.share).await,
        Activity::GuestJoined { id: 1, device } if device.label() == "alice on alice-laptop"
    ));
    let shop = &link.manifest.environments["shop"];
    assert_eq!(shop.dns, ["shop.test", "api.shop.test"]);
    assert!(!link.manifest.environments.contains_key("infrastructure"));
    // Where the services really listen stays on the host.
    assert!(!serde_json::to_string(&link.manifest)
        .unwrap()
        .contains("127.0.0.1"));

    let opener = link.opener();
    assert_eq!(
        call(&opener, "shop.test", 443, "hello").await.unwrap(),
        "shop|hello"
    );
    assert_eq!(
        call(&opener, "API.shop.test.", 8080, "ping").await.unwrap(),
        "api|ping"
    );

    link.close().await;
    assert!(matches!(
        next(&mut session.share).await,
        Activity::GuestLeft { id: 1 }
    ));
    session.share.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_outside_the_session_is_reachable() {
    let mut session = session(Duration::from_secs(60), 3).await;
    let link = GuestLink::join(&session.share.code(), &session.server, device("bob"))
        .await
        .unwrap();
    let opener = link.opener();
    next(&mut session.share).await;

    let private_port = session.private.port();
    for (host, port) in [
        ("shop.test", 22),           // a shared hostname, another port
        ("db.test", 5432),           // declared on the host, not selected
        ("127.0.0.1", private_port), // the host's loopback
        ("localhost", private_port),
        ("shop.test.evil.example", 443),
    ] {
        let refused = call(&opener, host, port, "x").await;
        assert!(
            matches!(refused, Err(OpenError::Denied)),
            "{host}:{port} → {refused:?}"
        );
        assert!(matches!(
            next(&mut session.share).await,
            Activity::Denied { guest: 1, host: denied, port: p } if denied == host && p == port
        ));
    }

    // The refusals did not break the link.
    assert_eq!(
        call(&opener, "shop.test", 443, "still").await.unwrap(),
        "shop|still"
    );
    link.close().await;
    session.share.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invitations_are_checked_and_guests_are_counted() {
    let session = session(Duration::from_secs(60), 1).await;
    let code = &session.share.code();

    assert!(matches!(
        GuestLink::join("not a code", &session.server, device("x")).await,
        Err(JoinError::Malformed)
    ));
    // Well formed, but no session behind it.
    let other = if code == "22222222" {
        "33333333"
    } else {
        "22222222"
    };
    assert!(matches!(
        GuestLink::join(other, &session.server, device("x")).await,
        Err(JoinError::NotFound)
    ));

    let first = GuestLink::join(code, &session.server, device("first"))
        .await
        .unwrap();
    assert!(matches!(
        GuestLink::join(code, &session.server, device("second")).await,
        Err(JoinError::Rejected(RejectReason::SessionFull))
    ));

    // A place is free again once the first guest has left.
    first.close().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let third = GuestLink::join(code, &session.server, device("third"))
        .await
        .unwrap();
    third.close().await;
    session.share.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_expires_for_everyone() {
    let mut session = session(Duration::from_secs(2), 3).await;
    let code = session.share.code().to_string();
    let mut link = GuestLink::join(&code, &session.server, device("carol"))
        .await
        .unwrap();
    let opener = link.opener();
    assert!(link.manifest.session.expires_in <= 2);

    let end = tokio::time::timeout(Duration::from_secs(5), link.ended())
        .await
        .unwrap();
    assert_eq!(end, End::Host(EndReason::Expired));

    loop {
        if let Activity::Ended { reason } = next(&mut session.share).await {
            assert_eq!(reason, EndReason::Expired);
            break;
        }
    }
    session.share.stop().await;

    assert!(call(&opener, "shop.test", 443, "late").await.is_err());
    assert!(matches!(
        GuestLink::join(&code, &session.server, device("late")).await,
        Err(JoinError::NotFound)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_a_guest_spares_the_others_and_closes_the_door() {
    let mut session = session(Duration::from_secs(60), 3).await;
    let code = session.share.code().to_string();
    let daves_laptop = DeviceSecret::random();
    let mut dave = GuestLink::join_as(&code, &session.server, device("dave"), &daves_laptop)
        .await
        .unwrap();
    let mut erin = GuestLink::join(&code, &session.server, device("erin"))
        .await
        .unwrap();
    let (dave_opener, erin_opener) = (dave.opener(), erin.opener());

    assert!(session.share.revoke(dave.guest_id).await);
    assert!(!session.share.revoke(99).await);

    let end = tokio::time::timeout(Duration::from_secs(5), dave.ended())
        .await
        .unwrap();
    assert_eq!(end, End::Host(EndReason::Revoked));
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(call(&dave_opener, "shop.test", 443, "x").await.is_err());

    // Erin keeps working; Dave's device is turned away, even with the right
    // code; the invitation stays open for anyone else.
    assert_eq!(
        call(&erin_opener, "shop.test", 443, "ok").await.unwrap(),
        "shop|ok"
    );
    assert!(matches!(
        GuestLink::join_as(&code, &session.server, device("dave"), &daves_laptop).await,
        Err(JoinError::Rejected(RejectReason::Revoked))
    ));
    assert!(session.share.invitation_open());
    let fay = GuestLink::join(&code, &session.server, device("fay"))
        .await
        .unwrap();
    fay.close().await;

    // Stopping tells the guests that remain.
    while !matches!(next(&mut session.share).await, Activity::GuestLeft { .. }) {}
    let stop = tokio::spawn(session.share.stop());
    let end = tokio::time::timeout(Duration::from_secs(5), erin.ended())
        .await
        .unwrap();
    assert_eq!(end, End::Host(EndReason::Stopped));
    stop.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_owner_sees_who_is_connected_and_can_invite_again() {
    let mut session = session(Duration::from_secs(60), 3).await;
    let first_code = session.share.code();
    let mut frank = GuestLink::join(&first_code, &session.server, device("frank"))
        .await
        .unwrap();
    let grace = GuestLink::join(&first_code, &session.server, device("grace"))
        .await
        .unwrap();
    next(&mut session.share).await;
    next(&mut session.share).await;

    let guests = session.share.guests();
    let labels: Vec<String> = guests.iter().map(|guest| guest.device.label()).collect();
    assert_eq!(labels, ["frank on frank-laptop", "grace on grace-laptop"]);
    assert!(guests
        .iter()
        .all(|guest| guest.connected < Duration::from_secs(5)));

    // Frank is disconnected; the invitation stays open for the others.
    assert!(session.share.revoke(frank.guest_id).await);
    assert!(session.share.invitation_open());
    let end = tokio::time::timeout(Duration::from_secs(5), frank.ended())
        .await
        .unwrap();
    assert_eq!(end, End::Host(EndReason::Revoked));

    // A new invitation retires the first code.
    let second_code = session.share.invite().await.unwrap();
    assert_ne!(second_code, first_code);
    assert_eq!(session.share.code(), second_code);
    assert!(session.share.invitation_open());
    assert!(matches!(
        GuestLink::join(&first_code, &session.server, device("frank")).await,
        Err(JoinError::NotFound)
    ));
    let heidi = GuestLink::join(&second_code, &session.server, device("heidi"))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(700)).await;
    let labels: Vec<String> = session
        .share
        .guests()
        .iter()
        .map(|guest| guest.device.label())
        .collect();
    assert_eq!(labels, ["grace on grace-laptop", "heidi on heidi-laptop"]);

    // Grace was never disturbed.
    assert_eq!(
        call(&grace.opener(), "shop.test", 443, "still")
            .await
            .unwrap(),
        "shop|still"
    );
    heidi.close().await;
    grace.close().await;
    session.share.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guest_that_cannot_reach_the_control_plane_joins_with_the_hosts_own_address() {
    let mut session = session(Duration::from_secs(60), 3).await;
    let invitation = session.share.direct_invitation();
    assert!(invitation.starts_with("dsh1"));

    // A control plane that answers nothing: this guest is on another network.
    let nowhere = "http://127.0.0.1:1";
    assert!(matches!(
        GuestLink::join(&session.share.code(), nowhere, device("remote")).await,
        Err(JoinError::Failed(_))
    ));
    let remote_laptop = DeviceSecret::random();
    let mut link = GuestLink::join_as(&invitation, nowhere, device("remote"), &remote_laptop)
        .await
        .unwrap();
    assert!(matches!(
        next(&mut session.share).await,
        Activity::GuestJoined { device, .. } if device.label() == "remote on remote-laptop"
    ));

    // It is a guest like any other: the same services, the same refusals.
    let opener = link.opener();
    assert_eq!(
        call(&opener, "shop.test", 443, "far").await.unwrap(),
        "shop|far"
    );
    assert!(matches!(
        call(&opener, "db.test", 5432, "x").await,
        Err(OpenError::Denied)
    ));

    // Disconnected by the host, that device cannot come back with the same
    // invitation; another device still can.
    assert!(session.share.revoke(link.guest_id).await);
    let end = tokio::time::timeout(Duration::from_secs(5), link.ended())
        .await
        .unwrap();
    assert_eq!(end, End::Host(EndReason::Revoked));
    assert!(matches!(
        GuestLink::join_as(&invitation, nowhere, device("remote"), &remote_laptop).await,
        Err(JoinError::Rejected(RejectReason::Revoked))
    ));
    let other = GuestLink::join(&invitation, nowhere, device("other"))
        .await
        .unwrap();
    other.close().await;

    // A new invitation is a new one of these too.
    session.share.invite().await.unwrap();
    let renewed = session.share.direct_invitation();
    assert_ne!(renewed, invitation);
    let again = GuestLink::join(&renewed, nowhere, device("remote"))
        .await
        .unwrap();
    again.close().await;

    // Cut short or mistyped, it is said to be no invitation.
    assert!(matches!(
        GuestLink::join(&invitation[..40], nowhere, device("remote")).await,
        Err(JoinError::Malformed)
    ));
    session.share.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn one_link_serves_a_guest_on_this_network_and_one_that_is_not() {
    use devshare_core::direct;

    let session = session(Duration::from_secs(60), 3).await;
    // The control plane is this machine: the link carries the host's address.
    let link = session.share.link();
    let code = session.share.code();
    assert!(link.starts_with("http://"), "{link}");
    assert!(
        link.contains(&format!("/{}-{}#", &code[..4], &code[4..])),
        "{link}"
    );

    // A guest that reaches the control plane, and one that cannot: the same
    // link, the same call.
    let near = GuestLink::join(&link, &session.server, device("near"))
        .await
        .unwrap();
    let far = GuestLink::join(&link, "http://127.0.0.1:1", device("far"))
        .await
        .unwrap();
    assert_eq!(
        call(&near.opener(), "shop.test", 443, "a").await.unwrap(),
        "shop|a"
    );
    assert_eq!(
        call(&far.opener(), "shop.test", 443, "b").await.unwrap(),
        "shop|b"
    );
    near.close().await;
    far.close().await;

    // When the address in the link leads nowhere from where the guest is,
    // the control plane the link names is asked for a fresh one: it must
    // name the same host, the one the link says.
    let (named, _) = direct::decode(&link).unwrap().unwrap();
    let moved = iroh::EndpointAddr::new(named.id).with_ip_addr("127.0.0.1:9".parse().unwrap());
    let stale = format!("{}/{code}#{}", session.server, direct::for_link(&moved));
    let through = GuestLink::join(&stale, &session.server, device("through"))
        .await
        .unwrap();
    assert_eq!(
        call(&through.opener(), "shop.test", 443, "c")
            .await
            .unwrap(),
        "shop|c"
    );
    through.close().await;

    // A control plane that names another host than the link does is not
    // believed, even with the right code: that is how a hostile one would
    // put itself in the middle.
    let other = iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[9; 32]).public())
        .with_ip_addr("127.0.0.1:9".parse().unwrap());
    let forged = format!("{}/{code}#{}", session.server, direct::for_link(&other));
    assert!(matches!(
        GuestLink::join(&forged, &session.server, device("misled")).await,
        Err(JoinError::Failed(_))
    ));

    // Without any control plane to ask, a stale link has nowhere to go.
    assert!(matches!(
        GuestLink::join(&stale, "http://127.0.0.1:1", device("lost")).await,
        Err(JoinError::Failed(_))
    ));
    session.share.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_a_public_invitation_page_the_link_points_there_and_still_says_everything() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(devshare_server::serve(listener));
    let shop = service("shop").await;
    let config = Config {
        server: None,
        environments: BTreeMap::from([(
            "shop".to_string(),
            EnvironmentDef {
                entrypoint: None,
                services: vec![ServiceDef {
                    host: "shop.test".into(),
                    port: 443,
                    target: Some(shop.to_string()),
                }],
            },
        )]),
    };
    let share = Share::start(ShareOptions {
        selection: config.select(&[]).unwrap(),
        lifetime: Duration::from_secs(60),
        max_guests: 2,
        server: server.clone(),
        join: Some("https://join.example".into()),
    })
    .await
    .unwrap();

    // Nothing of this machine in the link: a page anyone can open, and the
    // invitation after the `#`.
    let link = share.link();
    assert!(link.starts_with("https://join.example/#"), "{link}");
    assert!(
        !link.contains("127.0.0.1") && !link.contains(&share.code()),
        "{link}"
    );

    // A guest needs nothing else, and no control plane.
    let guest = GuestLink::join(&link, "http://127.0.0.1:1", device("anywhere"))
        .await
        .unwrap();
    assert_eq!(
        call(&guest.opener(), "shop.test", 443, "x").await.unwrap(),
        "shop|x"
    );
    guest.close().await;

    // A new invitation is a new link to the same page.
    share.invite().await.unwrap();
    let renewed = share.link();
    assert!(renewed.starts_with("https://join.example/#") && renewed != link);
    assert!(GuestLink::join(&link, "http://127.0.0.1:1", device("late"))
        .await
        .is_err());
    share.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn after_too_many_wrong_codes_the_invitation_closes_until_a_new_one() {
    let mut session = session(Duration::from_secs(60), 3).await;
    let link = session.share.link();
    let code = session.share.code();
    // The same host, another code: what someone who kept an old link and
    // guesses the new code sends.
    let wrong = |n: u32| {
        let guess = format!("{n:08}").replace('0', "2").replace('1', "3");
        link.replace(
            &format!("/{}-{}", &code[..4], &code[4..]),
            &format!("/{guess}"),
        )
    };
    assert_ne!(wrong(1), link);

    for n in 1..20 {
        assert!(matches!(
            GuestLink::join(&wrong(n), &session.server, device("guesser")).await,
            Err(JoinError::Rejected(RejectReason::InvalidInvitation))
        ));
        assert!(matches!(
            next(&mut session.share).await,
            Activity::Refused { .. }
        ));
    }
    // The twentieth wrong code closes the door...
    assert!(
        GuestLink::join(&wrong(20), &session.server, device("guesser"))
            .await
            .is_err()
    );
    assert!(matches!(
        next(&mut session.share).await,
        Activity::Locked { attempts: 20 }
    ));
    assert!(matches!(
        next(&mut session.share).await,
        Activity::Refused { .. }
    ));
    assert!(!session.share.invitation_open());
    // ...to the right code too.
    assert!(matches!(
        GuestLink::join(&link, &session.server, device("late")).await,
        Err(JoinError::Rejected(RejectReason::InvalidInvitation))
    ));
    next(&mut session.share).await;

    // A new invitation opens it again, and starts the count over.
    session.share.invite().await.unwrap();
    assert!(session.share.invitation_open());
    let guest = GuestLink::join(&session.share.link(), &session.server, device("welcome"))
        .await
        .unwrap();
    guest.close().await;
    session.share.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_control_plane_that_lies_about_the_host_learns_nothing_and_admits_nobody() {
    use devshare_core::direct;
    use std::sync::{Arc, Mutex};
    use tokio::io::AsyncReadExt;

    let alice = session(Duration::from_secs(60), 3).await;
    let mut bob = session(Duration::from_secs(60), 3).await;
    let (bobs_host, _) = direct::decode(&bob.share.direct_invitation())
        .unwrap()
        .unwrap();

    // A control plane that answers every lookup with Bob's host, and keeps
    // what it is sent.
    let liar = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let liar_url = format!("http://{}", liar.local_addr().unwrap());
    let heard = Arc::new(Mutex::new(String::new()));
    let answer = serde_json::json!({ "host": bobs_host }).to_string();
    tokio::spawn({
        let heard = heard.clone();
        async move {
            while let Ok((mut stream, _)) = liar.accept().await {
                let mut request = vec![0u8; 4096];
                let read = stream.read(&mut request).await.unwrap_or(0);
                heard
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&request[..read]));
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                stream.write_all(reply.as_bytes()).await.ok();
            }
        }
    });

    // Alice's code, typed, looked up on the liar: the guest reaches Bob's
    // host, which cannot complete the exchange with a code it does not know.
    let typed = alice.share.code();
    assert!(matches!(
        GuestLink::join(&typed, &liar_url, device("misled")).await,
        Err(JoinError::Rejected(RejectReason::InvalidInvitation))
    ));
    assert!(matches!(
        next(&mut bob.share).await,
        Activity::Refused {
            reason: RejectReason::InvalidInvitation
        }
    ));
    assert!(alice.share.guests().is_empty() && bob.share.guests().is_empty());

    // All the liar ever heard is a lookup key: never the code.
    let heard = heard.lock().unwrap().clone();
    assert!(heard.contains("GET /v2/invitations/"), "{heard}");
    assert!(
        !heard.contains(&typed) && !heard.contains(&typed[..4]),
        "{heard}"
    );

    alice.share.stop().await;
    bob.share.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_device_joining_again_takes_its_own_place() {
    let mut session = session(Duration::from_secs(60), 1).await;
    let code = session.share.code();
    let laptop = DeviceSecret::random();
    let mut first = GuestLink::join_as(&code, &session.server, device("ivan"), &laptop)
        .await
        .unwrap();
    next(&mut session.share).await;

    // The session is full, but this is the same device, after a crash for
    // instance: it takes over, and the earlier process is told.
    let second = GuestLink::join_as(&code, &session.server, device("ivan"), &laptop)
        .await
        .unwrap();
    let end = tokio::time::timeout(Duration::from_secs(5), first.ended())
        .await
        .unwrap();
    assert_eq!(end, End::Host(EndReason::Replaced));
    assert_eq!(session.share.guests().len(), 1);
    assert_eq!(
        call(&second.opener(), "shop.test", 443, "back")
            .await
            .unwrap(),
        "shop|back"
    );

    // Another device still finds it full.
    assert!(matches!(
        GuestLink::join(&code, &session.server, device("judy")).await,
        Err(JoinError::Rejected(RejectReason::SessionFull))
    ));
    second.close().await;
    session.share.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_without_a_time_limit_outlives_its_invitation_on_the_control_plane() {
    let unlimited = Duration::from_secs(devshare_core::protocol::NO_LIMIT);
    let session = session(unlimited, 3).await;
    // The control plane keeps a code a day at most; the session goes on.
    assert!(session.share.remaining() > Duration::from_secs(365 * 24 * 3600));
    let link = GuestLink::join(&session.share.code(), &session.server, device("night"))
        .await
        .unwrap();
    assert!(devshare_core::protocol::unlimited(
        link.manifest.session.expires_in
    ));
    link.close().await;
    session.share.stop().await;
}
