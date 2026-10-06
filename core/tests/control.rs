//! Telling a control plane of this version from an older one.

use devshare_core::control::is_current;
use tokio::{io::AsyncWriteExt, net::TcpListener};

#[tokio::test(flavor = "multi_thread")]
async fn a_control_plane_too_old_to_say_what_it_is_is_recognised() {
    // This version.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let current = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(devshare_server::serve(listener));
    assert!(is_current(&current).await);

    // An older one: it knows no such address and answers 404, with nothing.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let older = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await
                .ok();
        }
    });
    assert!(!is_current(&older).await);

    // Nothing there at all.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let nothing = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert!(!is_current(&nothing).await);
}
