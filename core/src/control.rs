//! Client of the control plane, which only maps an invitation code to the
//! host's address for a limited time.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use devshare_protocol::{CreateInvitation, InvitationCreated, InvitationLookup};
use iroh::EndpointAddr;
use reqwest::StatusCode;

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?)
}

/// Said once per process: a code sent over plain HTTP to another machine can
/// be read on the way, and whoever reads it can join.
fn warn_if_in_clear(server: &str) {
    static SAID: std::sync::Once = std::sync::Once::new();
    let Some(rest) = server.strip_prefix("http://") else {
        return;
    };
    let host = rest.split(['/', ':']).next().unwrap_or_default();
    let host = host.trim_matches(['[', ']']);
    if matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return;
    }
    SAID.call_once(|| {
        tracing::warn!(
            "{server} is reached over plain HTTP: anyone on the way can read the invitation \
             code and join. Use https for a control plane on another machine."
        );
    });
}

fn url(server: &str, path: &str) -> String {
    warn_if_in_clear(server);
    format!("{}{path}", server.trim_end_matches('/'))
}

/// Whether what answers at `server` is a control plane of this version.
/// One started before invitation links became web addresses has no page to
/// serve for them: sessions work, but a phone opening the link gets nothing.
pub async fn is_current(server: &str) -> bool {
    let Ok(client) = client() else { return false };
    match client.get(url(server, "/v1")).send().await {
        Ok(answer) if answer.status().is_success() => answer
            .text()
            .await
            .is_ok_and(|text| text == devshare_protocol::CONTROL_PLANE),
        _ => false,
    }
}

pub async fn create(server: &str, host: &EndpointAddr, ttl: Duration) -> Result<InvitationCreated> {
    let request = CreateInvitation {
        host: serde_json::to_value(host)?,
        ttl: ttl.as_secs(),
    };
    let response = client()?
        .post(url(server, "/v1/invitations"))
        .json(&request)
        .send()
        .await
        .with_context(|| format!("reaching the control plane at {server}"))?;
    if !response.status().is_success() {
        bail!(
            "the control plane refused the invitation ({})",
            response.status()
        );
    }
    Ok(response.json().await?)
}

/// `None` when the code is unknown or expired.
pub async fn lookup(server: &str, code: &str) -> Result<Option<EndpointAddr>> {
    let response = client()?
        .get(url(server, &format!("/v1/invitations/{code}")))
        .send()
        .await
        .with_context(|| format!("reaching the control plane at {server}"))?;
    match response.status() {
        StatusCode::NOT_FOUND => Ok(None),
        status if status.is_success() => {
            let lookup: InvitationLookup = response.json().await?;
            Ok(Some(
                serde_json::from_value(lookup.host).context("decoding the host address")?,
            ))
        }
        status => bail!("the control plane answered {status}"),
    }
}

pub async fn delete(server: &str, code: &str, owner_token: &str) -> Result<()> {
    client()?
        .delete(url(server, &format!("/v1/invitations/{code}")))
        .bearer_auth(owner_token)
        .send()
        .await?;
    Ok(())
}
