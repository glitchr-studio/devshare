//! `devshare share <folder>`: the real command, on project folders that
//! have a compose file and nothing else yet.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::TcpListener,
    process::Command,
};

/// A project folder with a compose file and no devshare.toml.
fn project(root: &Path, name: &str, compose: &str) -> PathBuf {
    let folder = root.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("docker-compose.yml"), compose).unwrap();
    folder
}

/// Runs `devshare share` with these arguments from `root` until the session
/// is announced or the command gives up; returns what it printed and
/// whether it was still sharing.
async fn share(root: &Path, server: &str, arguments: &[&str]) -> (String, bool) {
    let mut host = Command::new(env!("CARGO_BIN_EXE_devshare"))
        .arg("share")
        .args(arguments)
        .args(["--duration", "1m", "--no-qr"])
        .current_dir(root)
        .env("DEVSHARE_SERVER", server)
        .env("DEVSHARE_SETTINGS", "/dev/null")
        .env("DEVSHARE_JOIN", "")
        .env("DEVSHARE_RELAY", "disabled")
        .env_remove("DEVSHARE_CONFIG")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut screen = BufReader::new(host.stdout.take().unwrap()).lines();
    let mut printed = String::new();
    let announced = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(line) = screen.next_line().await.unwrap() {
            printed.push_str(&line);
            printed.push('\n');
            if line.starts_with("Expires in") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap();
    if !announced {
        let refused = host.wait_with_output().await.unwrap();
        printed.push_str(&String::from_utf8_lossy(&refused.stderr));
    }
    (printed, announced)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_is_shared_from_its_compose_file_without_another_step() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(devshare_server::serve(listener));

    let root = std::env::temp_dir().join(format!("devshare-share-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let shop = project(
        &root,
        "shop",
        "services:\n  web:\n    image: nginx:alpine\n    ports: [\"8761:80\", \"8762:443\"]\n  cache:\n    image: redis:alpine\n    ports: [\"8763:6379\"]\n",
    );
    let blog = project(
        &root,
        "blog",
        "services:\n  web:\n    image: nginx:alpine\n    ports: [\"8764:80\"]\n",
    );

    // One folder: its devshare.toml is computed, written, and shared.
    let (printed, sharing) = share(&root, &server, &["shop/"]).await;
    assert!(sharing, "{printed}");
    assert!(
        printed.contains("shop/devshare.toml written from docker-compose.yml."),
        "{printed}"
    );
    assert!(
        printed.contains("shop.test:8761") && printed.contains("shop.test:8762"),
        "{printed}"
    );
    assert!(
        !printed.contains("8763"),
        "the cache is not shared: {printed}"
    );
    // Nothing runs behind these ports: said once, with how to start it.
    assert!(printed.contains("the project is not started"), "{printed}");
    assert!(
        printed.contains("docker compose up -d      (in shop/)"),
        "{printed}"
    );
    let written = std::fs::read_to_string(shop.join("devshare.toml")).unwrap();
    assert!(written.starts_with("# Written by `devshare discover`"));

    // What its owner then edits in it is kept: sharing again computes nothing.
    std::fs::write(
        shop.join("devshare.toml"),
        written.replace("port = 8762", "port = 9999"),
    )
    .unwrap();
    let (printed, sharing) = share(&root, &server, &["shop"]).await;
    assert!(sharing && !printed.contains("written from"), "{printed}");
    assert!(printed.contains("shop.test:9999"), "{printed}");

    // Two folders, one session; and from inside a folder, no argument at all.
    let (printed, sharing) = share(&root, &server, &["shop", "blog"]).await;
    assert!(sharing, "{printed}");
    assert!(
        printed.contains("shop.test:8761") && printed.contains("blog.test:8764"),
        "{printed}"
    );
    assert!(blog.join("devshare.toml").is_file());
    let (printed, sharing) = share(&blog, &server, &[]).await;
    assert!(sharing && printed.contains("blog.test:8764"), "{printed}");

    // Two folders, one of their environments; a folder named twice is one.
    let (printed, sharing) = share(
        &root,
        &server,
        &["shop", "blog", "./blog/", "--only", "blog"],
    )
    .await;
    assert!(sharing, "{printed}");
    assert!(
        printed.contains("blog.test:8764") && !printed.contains("shop.test"),
        "{printed}"
    );

    // What is not a folder is said, with the way to name an environment.
    let (printed, sharing) = share(&root, &server, &["shop", "web"]).await;
    assert!(!sharing);
    assert!(
        printed.contains("web is not a folder") && printed.contains("--only"),
        "{printed}"
    );

    // Nothing to compute from: said, with the folder's name.
    std::fs::create_dir_all(root.join("empty")).unwrap();
    let (printed, sharing) = share(&root, &server, &["empty"]).await;
    assert!(!sharing);
    assert!(
        printed.contains("empty has no devshare.toml") && printed.contains("no compose file"),
        "{printed}"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// What a browser gets from this machine's port `port` at `path`, if anything.
async fn get(port: u16, path: &str) -> Option<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .ok()?;
    stream
        .write_all(format!("GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").as_bytes())
        .await
        .ok()?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await.ok()?;
    Some(answer)
}

#[tokio::test(flavor = "multi_thread")]
async fn sharing_without_a_control_plane_on_this_machine_brings_its_own() {
    // A port of this machine nothing listens on.
    let free = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    assert!(get(port, "/v1").await.is_none());

    let root = std::env::temp_dir().join(format!("devshare-own-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    project(
        &root,
        "blog",
        "services:\n  web:\n    image: nginx:alpine\n    ports: [\"8765:80\"]\n",
    );

    let mut host = Command::new(env!("CARGO_BIN_EXE_devshare"))
        .args(["share", "blog", "--duration", "1m", "--no-qr"])
        .current_dir(&root)
        .env("DEVSHARE_SERVER", format!("http://localhost:{port}"))
        .env("DEVSHARE_SETTINGS", "/dev/null")
        .env("DEVSHARE_JOIN", "")
        .env("DEVSHARE_RELAY", "disabled")
        .env_remove("DEVSHARE_CONFIG")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut screen = BufReader::new(host.stdout.take().unwrap()).lines();
    let mut printed = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(line) = screen.next_line().await.unwrap() {
            let announced = line.starts_with("Expires in");
            printed.push(line);
            if announced {
                break;
            }
        }
    })
    .await
    .expect("the session was never announced");

    // It says so, shares, and its link opens a page on that very port.
    assert!(
        printed
            .iter()
            .any(|line| line.contains("this session runs its own")),
        "{printed:?}"
    );
    assert!(
        !printed.iter().any(|line| line.contains("older version")),
        "{printed:?}"
    );
    let link = &printed[printed
        .iter()
        .position(|line| line == "Invitation:")
        .unwrap()
        + 1];
    let code = link.rsplit('/').next().unwrap().split('#').next().unwrap();
    assert!(link.contains(&format!(":{port}/")), "{link}");
    let page = get(port, &format!("/{code}")).await.unwrap();
    assert!(
        page.contains(" 200 ") && page.contains("You are invited"),
        "{page}"
    );

    // It ends with the session: nothing is left listening.
    unsafe { libc::kill(host.id().unwrap() as i32, libc::SIGINT) };
    assert!(host.wait().await.unwrap().success());
    assert!(get(port, "/v1").await.is_none());

    std::fs::remove_dir_all(&root).ok();
}
