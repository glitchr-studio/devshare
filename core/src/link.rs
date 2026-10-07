//! The encrypted peer-to-peer link between a host and its guests.

use std::{env, sync::OnceLock, time::Duration};

use anyhow::{anyhow, Context, Result};
use devshare_protocol::ALPN;
use iroh::{
    endpoint::{presets, Connection, QuicTransportConfig, RecvStream, SendStream},
    Endpoint, RelayMap, RelayMode,
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

/// The relay named by the general settings, for this process.
static CONFIGURED: OnceLock<Option<String>> = OnceLock::new();

/// Says which relay the general settings name. To call once, before any
/// session starts; `DEVSHARE_RELAY` still wins over it.
pub fn use_relay(relay: Option<String>) {
    CONFIGURED.set(relay).ok();
}

/// The relays to use: what `DEVSHARE_RELAY` says, else what the general
/// settings say, else the default ones. Either can be the address of a relay
/// of one's own, or `disabled` for direct connections only.
fn relay_mode() -> Result<RelayMode> {
    let asked = env::var("DEVSHARE_RELAY").ok();
    let configured = CONFIGURED.get().cloned().flatten();
    relay_mode_of(asked.as_deref(), configured.as_deref())
}

fn relay_mode_of(asked: Option<&str>, configured: Option<&str>) -> Result<RelayMode> {
    let chosen = asked
        .filter(|relay| !relay.is_empty())
        .or(configured.filter(|relay| !relay.is_empty()));
    match chosen {
        None => Ok(RelayMode::Default),
        Some("disabled") => Ok(RelayMode::Disabled),
        Some(url) => {
            let map = RelayMap::try_from_iter([url])
                .map_err(|error| anyhow!("\"{url}\" is not the address of a relay: {error}"))?;
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
    endpoint_as(accept, None).await
}

/// An endpoint with the given key, or a fresh one.
pub async fn endpoint_as(accept: bool, key: Option<iroh::SecretKey>) -> Result<Endpoint> {
    let relay_mode = relay_mode()?;
    let wait_for_relay = !matches!(relay_mode, RelayMode::Disabled);

    let transport = QuicTransportConfig::builder()
        .max_idle_timeout(Some(IDLE_TIMEOUT.try_into()?))
        .build();
    let mut builder = Endpoint::builder(presets::Minimal)
        .relay_mode(relay_mode)
        .transport_config(transport);
    if let Some(key) = key {
        builder = builder.secret_key(key);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_relay_is_what_was_asked_for_then_the_setting_then_the_default() {
        let kind = |asked, configured| match relay_mode_of(asked, configured).unwrap() {
            RelayMode::Default => "default".to_string(),
            RelayMode::Disabled => "disabled".to_string(),
            RelayMode::Custom(map) => map.urls::<Vec<_>>()[0].to_string(),
            _ => "other".to_string(),
        };
        assert_eq!(kind(None, None), "default");
        assert_eq!(kind(Some(""), None), "default");
        assert_eq!(
            kind(None, Some("https://relay.glitchr.dev")),
            "https://relay.glitchr.dev/"
        );
        assert_eq!(kind(None, Some("disabled")), "disabled");
        assert_eq!(
            kind(Some("disabled"), Some("https://relay.glitchr.dev")),
            "disabled"
        );
        assert_eq!(
            kind(Some("https://other.example"), Some("disabled")),
            "https://other.example/"
        );
        assert!(relay_mode_of(None, Some("not a relay")).is_err());
    }
}
