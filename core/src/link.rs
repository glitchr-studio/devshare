//! The encrypted peer-to-peer link between a host and its guests.

use std::{env, time::Duration};

use anyhow::{anyhow, Context, Result};
use devshare_protocol::ALPN;
use iroh::{
    endpoint::{presets, Connection, QuicTransportConfig, RecvStream, SendStream},
    Endpoint, RelayMap, RelayMode,
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

/// `DEVSHARE_RELAY`: unset for the default relays, a URL for a self-hosted
/// relay, `disabled` for direct connections only.
fn relay_mode() -> Result<RelayMode> {
    match env::var("DEVSHARE_RELAY").ok().as_deref() {
        None | Some("") => Ok(RelayMode::Default),
        Some("disabled") => Ok(RelayMode::Disabled),
        Some(url) => {
            let map = RelayMap::try_from_iter([url])
                .map_err(|error| anyhow!("DEVSHARE_RELAY is not a relay URL: {error}"))?;
            Ok(RelayMode::Custom(map))
        }
    }
}

/// How long a peer may stay silent before it is considered gone: a guest
/// whose device slept or lost its network frees its place after this. The
/// link sends a keep-alive every five seconds on its own.
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);

/// How the packets of a link travel right now. A link usually starts
/// relayed and becomes direct within a second or two, when the networks on
/// both sides allow it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Direct,
    Relayed,
}

impl std::fmt::Display for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Direct => "direct",
            Self::Relayed => "relayed",
        })
    }
}

/// The route in use and its round-trip time, once one is established.
pub fn route(connection: &Connection) -> Option<(Route, Duration)> {
    let paths = connection.paths();
    let path = paths.iter().find(|path| path.is_selected())?;
    let route = if path.is_relay() {
        Route::Relayed
    } else {
        Route::Direct
    };
    Some((route, path.rtt()))
}

/// Every session gets a fresh endpoint, hence a fresh key pair.
pub async fn endpoint(accept: bool) -> Result<Endpoint> {
    let relay_mode = relay_mode()?;
    let wait_for_relay = !matches!(relay_mode, RelayMode::Disabled);

    let transport = QuicTransportConfig::builder()
        .max_idle_timeout(Some(IDLE_TIMEOUT.try_into()?))
        .build();
    let mut builder = Endpoint::builder(presets::Minimal)
        .relay_mode(relay_mode)
        .transport_config(transport);
    if accept {
        builder = builder.alpns(vec![ALPN.to_vec()]);
    }
    let endpoint = builder
        .bind()
        .await
        .map_err(|error| anyhow!("{error}"))
        .context("opening the peer-to-peer endpoint")?;

    // A host must know its relay before it publishes its address. Without
    // one it stays reachable on its direct addresses.
    if accept && wait_for_relay {
        let online = tokio::time::timeout(Duration::from_secs(5), endpoint.online());
        if online.await.is_err() {
            tracing::warn!("no relay reachable, guests need a direct route to this machine");
        }
    }
    Ok(endpoint)
}

/// Splices a local stream with a QUIC stream until both directions are done.
pub async fn pipe<S>(local: S, mut send: SendStream, mut recv: RecvStream) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut local_read, mut local_write) = tokio::io::split(local);
    let inbound = async {
        tokio::io::copy(&mut recv, &mut local_write).await?;
        local_write.shutdown().await?;
        anyhow::Ok(())
    };
    let outbound = async {
        tokio::io::copy(&mut local_read, &mut send).await?;
        send.finish().ok();
        anyhow::Ok(())
    };
    tokio::try_join!(inbound, outbound)?;
    Ok(())
}
