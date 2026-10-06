//! The QR code, end to end: a control plane, the real `devshare share`
//! showing its invitation, and a guest that has nothing but what a camera
//! reads on the host's screen.

use std::{process::Stdio, time::Duration};

use devshare_core::{
    guest::GuestLink,
    protocol::Device,
    qr::reading::{is_code_line, picture},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines},
    net::TcpListener,
    process::{ChildStdout, Command},
};

/// The lines of the host's screen up to one that says `wanted`.
async fn screen_until(screen: &mut Lines<BufReader<ChildStdout>>, wanted: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let read = async {
        while let Some(line) = screen.next_line().await.unwrap() {
            let found = line.contains(wanted);
            lines.push(line);
            if found {
                return;
            }
        }
        panic!("the host stopped before saying \"{wanted}\"");
    };
    if tokio::time::timeout(Duration::from_secs(20), read)
        .await
        .is_err()
    {
        panic!(
            "the host never said \"{wanted}\"; it said:\n{}",
            lines.join("\n")
        );
    }
    lines
}

/// What a browser gets from the control plane at `path`.
async fn get(server: &str, path: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(server.trim_start_matches("http://"))
        .await
        .unwrap();
    stream
        .write_all(format!("GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await.unwrap();
    answer
}

/// What a camera reads in the QR code among the lines of a screen.
fn scan(screen: &[String]) -> String {
    let code: Vec<&str> = screen
        .iter()
        .map(String::as_str)
        .filter(|line| is_code_line(line))
        .collect();
    assert!(code.len() > 10, "no QR code on the screen");
    // Not a terminal: the code is drawn for a dark background.
    let (pixels, width) = picture(&code.join("\n"), false);
    let mut image =
        rqrr::PreparedImage::prepare_from_greyscale(width, pixels.len() / width, |x, y| {
            pixels[y * width + x]
        });
    let grids = image.detect_grids();
    assert_eq!(grids.len(), 1, "one QR code is expected on the screen");
    grids[0].decode().unwrap().1
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guest_joins_with_what_a_camera_reads_on_the_hosts_screen() {
    // The server: a control plane on a port of the system's choosing.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(devshare_server::serve(listener));

    // A local service, and the project that declares it.
    let service = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = service.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = service.accept().await {
            tokio::spawn(async move {
                stream.write_all(b"shop|").await.ok();
                let (mut read, mut write) = stream.split();
                tokio::io::copy(&mut read, &mut write).await.ok();
            });
        }
    });
    let folder = std::env::temp_dir().join(format!("devshare-qr-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("devshare.toml"),
        format!("[environments.shop]\nservices = [{{ host = \"shop.test\", port = 80, target = \"{target}\" }}]\n"),
    )
    .unwrap();

    // The host: the real command, in the project's folder.
    let mut host = Command::new(env!("CARGO_BIN_EXE_devshare"))
        .args(["share", "--duration", "1m", "--guests", "1"])
        .current_dir(&folder)
        .env("DEVSHARE_SERVER", &server)
        .env("DEVSHARE_SETTINGS", "/dev/null")
        // This test is about the control plane's own link and page.
        .env("DEVSHARE_JOIN", "")
        .env("DEVSHARE_RELAY", "disabled")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut screen = BufReader::new(host.stdout.take().unwrap()).lines();
    let shown = screen_until(&mut screen, "Expires in").await;

    // The camera: the QR code says what the screen says in words.
    let scanned = scan(&shown);
    let link = &shown[shown.iter().position(|line| line == "Invitation:").unwrap() + 1];
    let code = &shown[shown.iter().position(|line| line == "Code:").unwrap() + 1];
    assert_eq!(&scanned, link);
    // A web address a phone can open: the control plane's, then the code;
    // and, the control plane being the host's own machine, the host's
    // address after the `#`.
    assert!(
        scanned.starts_with("http://") && scanned.contains(&format!("/{code}#")),
        "{scanned}"
    );
    let page = get(&server, &format!("/{code}")).await;
    assert!(page.contains(" 200 "), "{page}");
    assert!(page.contains("You are invited to a DevShare session") && page.contains(code.as_str()));

    // The client: it joins with the scanned text and nothing else.
    let device = Device {
        name: "phone".into(),
        platform: "test".into(),
        user: Some("alice".into()),
    };
    let guest = GuestLink::join(&scanned, &server, device).await.unwrap();
    assert!(guest.manifest.environments.contains_key("shop"));
    screen_until(&mut screen, "guest 1 joined: alice on phone").await;

    // And reaches the shared service under its hostname.
    let (mut send, mut recv) = guest.opener().open("shop.test", 80).await.unwrap();
    send.write_all(b"scanned").await.unwrap();
    send.finish().unwrap();
    let mut answer = String::new();
    recv.read_to_string(&mut answer).await.unwrap();
    assert_eq!(answer, "shop|scanned");

    // The tests run without a relay: the screen says the invitation stays
    // on this network.
    assert!(
        shown
            .iter()
            .any(|line| line.contains("this invitation only works on this network")),
        "{shown:?}"
    );

    // One invitation for everyone: the very text that was scanned also
    // lets in a guest that cannot reach the control plane at all.
    guest.close().await;
    screen_until(&mut screen, "guest 1 left").await;
    let device = Device {
        name: "laptop".into(),
        platform: "test".into(),
        user: Some("bob".into()),
    };
    let guest = GuestLink::join(&scanned, "http://127.0.0.1:1", device)
        .await
        .unwrap();
    screen_until(&mut screen, "guest 2 joined: bob on laptop").await;

    // A second scan of the same code finds the session full: the code is
    // an invitation, not an open door.
    let late = Device {
        name: "tablet".into(),
        platform: "test".into(),
        user: None,
    };
    assert!(GuestLink::join(&scanned, &server, late).await.is_err());
    screen_until(&mut screen, "a device was refused").await;

    // The host stops: the command ends well, and the code is dead.
    guest.close().await;
    unsafe { libc::kill(host.id().unwrap() as i32, libc::SIGINT) };
    screen_until(&mut screen, "Sharing stopped.").await;
    assert!(host.wait().await.unwrap().success());
    let after = Device {
        name: "phone".into(),
        platform: "test".into(),
        user: None,
    };
    assert!(GuestLink::join(&scanned, &server, after).await.is_err());

    let page = get(&server, &format!("/{code}")).await;
    assert!(
        page.contains(" 404 ") && page.contains("no longer valid"),
        "{page}"
    );

    std::fs::remove_dir_all(&folder).ok();
}
