use std::{
    env,
    io::ErrorKind,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use anyhow::{Context, Result};
use tokio::net::TcpListener;

/// Where the control plane listens unless `DEVSHARE_LISTEN` says otherwise.
const DEFAULT: &str = "0.0.0.0:8787";

#[tokio::main]
async fn main() -> Result<()> {
    // This is the control plane, and it takes no command: sharing and
    // joining are done with `devshare`.
    if let Some(argument) = env::args().nth(1) {
        eprintln!("devshare-server takes no argument (got \"{argument}\").");
        eprintln!("It is the control plane sessions are announced on; it only runs.");
        eprintln!("To share or join, use: devshare {argument}");
        std::process::exit(2);
    }

    tracing_subscriber::fmt()
        .with_env_filter(env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let wanted: SocketAddr = env::var("DEVSHARE_LISTEN")
        .unwrap_or_else(|_| DEFAULT.into())
        .parse()
        .context("DEVSHARE_LISTEN is not an address such as 0.0.0.0:8787")?;

    let listener = match TcpListener::bind(wanted).await {
        Ok(listener) => listener,
        Err(error) if error.kind() == ErrorKind::AddrInUse => {
            // Two control planes on one machine would each know half of the
            // sessions: a host announced on one, its guest looking on the
            // other. So when one is there already, it is the one to use.
            let there = SocketAddr::new(local(wanted.ip()), wanted.port());
            if devshare_server::answers_at(there).await {
                println!("A control plane is already running at {}.", url(wanted));
                println!("Nothing more to start.");
                return Ok(());
            }
            // Something else has the port: any free one will do.
            let listener = TcpListener::bind(SocketAddr::new(wanted.ip(), 0))
                .await
                .with_context(|| format!("listening on {}", wanted.ip()))?;
            let address = url(listener.local_addr()?);
            println!("Port {} is taken by another program.", wanted.port());
            println!("Listening on {address} instead. Point devshare at it with:");
            println!("  export DEVSHARE_SERVER={address}");
            listener
        }
        Err(error) => return Err(error).with_context(|| format!("listening on {wanted}")),
    };

    // The address really bound: with port 0 the system chose it.
    println!("Control plane listening on {}", url(listener.local_addr()?));
    devshare_server::serve(listener).await?;
    Ok(())
}

/// An address to reach a listener of this machine at.
fn local(address: IpAddr) -> IpAddr {
    match address.is_unspecified() {
        true => IpAddr::V4(Ipv4Addr::LOCALHOST),
        false => address,
    }
}

/// The URL of a listener, as someone on this machine would type it.
fn url(address: SocketAddr) -> String {
    match address.ip().is_unspecified() || address.ip().is_loopback() {
        true => format!("http://localhost:{}", address.port()),
        false => format!("http://{address}"),
    }
}
