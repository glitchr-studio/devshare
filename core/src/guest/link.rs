use std::time::Duration;

use anyhow::{anyhow, Result};
use devshare_protocol::{
    code, Device, EndReason, Hello, HelloReply, HostEvent, Manifest, Open, OpenReply, RejectReason,
    ALPN, MAX_FRAME, PROTOCOL_VERSION,
};
use iroh::{
    endpoint::{Connection, RecvStream, SendStream},
    Endpoint,
};

use crate::{
    control, direct, frame,
    link::{self, Route},
};

const JOIN_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the address carried by a link is tried before the control plane
/// the link also names is asked instead.
const DIRECT_TIMEOUT: Duration = Duration::from_secs(8);
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub enum JoinError {
    /// Not something an invitation looks like.
    Malformed,
    /// Unknown to the control plane: mistyped, expired or withdrawn.
    NotFound,
    Rejected(RejectReason),
    Failed(anyhow::Error),
}

impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed => f.write_str("this is not an invitation code or link"),
            Self::NotFound => f.write_str("no session for this invitation: it may have expired"),
            Self::Rejected(reason) => write!(f, "{reason}"),
            Self::Failed(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for JoinError {}

impl From<anyhow::Error> for JoinError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

#[derive(Debug)]
pub enum OpenError {
    /// The service is not part of the session.
    Denied,
    /// The service is shared but does not answer on the host.
    Unreachable,
    Failed(anyhow::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied => f.write_str("not shared in this session"),
            Self::Unreachable => {
                f.write_str("nothing answers on the host: the project may not be started there")
            }
            Self::Failed(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for OpenError {}

/// Why a joined session is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Host(EndReason),
    /// The link dropped without a word from the host.
    Lost,
}

impl std::fmt::Display for End {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host(reason) => write!(f, "{reason}"),
            Self::Lost => f.write_str("the connection to the host was lost"),
        }
    }
}

/// A joined session.
pub struct GuestLink {
    endpoint: Endpoint,
    connection: Connection,
    control: RecvStream,
    /// Kept open: the host reads its end as "the guest is still here".
    _control_send: SendStream,
    pub guest_id: u32,
    pub manifest: Manifest,
}

/// Opens streams to the services of a session. Cheap to clone.
#[derive(Clone)]
pub struct Opener {
    connection: Connection,
}

impl GuestLink {
    /// Redeems an invitation, whatever it is given as: the link (typed,
    /// pasted or read from its QR code), the short code, or the host's
    /// address alone. Every client joins through here, so they all accept
    /// the same thing.
    ///
    /// A link made on the host's own machine carries the host's address:
    /// that is tried first, because it works from any network. The control
    /// plane, at `server`, is asked when the invitation carries no address,
    /// or when the address in it leads nowhere from here.
    pub async fn join(invitation: &str, server: &str, device: Device) -> Result<Self, JoinError> {
        let code = code::parse(invitation);
        match direct::decode(invitation) {
            Some(Err(())) => return Err(JoinError::Malformed),
            Some(Ok((host, carried))) => {
                // With a control plane to fall back on, do not wait as long.
                let patience = if code.is_some() {
                    DIRECT_TIMEOUT
                } else {
                    JOIN_TIMEOUT
                };
                match Self::connect(host, carried, device.clone(), patience).await {
                    Err(JoinError::Failed(error)) if code.is_some() => {
                        tracing::debug!("the address in the invitation led nowhere: {error:#}");
                    }
                    outcome => return outcome,
                }
            }
            None => {}
        }

        let code = code.ok_or(JoinError::Malformed)?;
        let host = control::lookup(server, &code)
            .await?
            .ok_or(JoinError::NotFound)?;
        Self::connect(host, code, device, JOIN_TIMEOUT).await
    }

    /// Connects to the host at `host` and presents the invitation `code`.
    async fn connect(
        host: iroh::EndpointAddr,
        code: String,
        device: Device,
        patience: Duration,
    ) -> Result<Self, JoinError> {
        let endpoint = link::endpoint(false).await?;

        let joined = tokio::time::timeout(patience, async {
            let connection = endpoint
                .connect(host, ALPN)
                .await
                .map_err(|error| anyhow!("connecting to the host: {error}"))?;
            let (mut send, mut recv) = connection
                .open_bi()
                .await
                .map_err(|error| anyhow!("{error}"))?;
            let hello = Hello {
                protocol: PROTOCOL_VERSION,
                invitation: code,
                device,
                capabilities: Vec::new(),
            };
            frame::write(&mut send, &hello).await?;
            let reply: HelloReply = frame::read(&mut recv, MAX_FRAME).await?;
            anyhow::Ok((connection, send, recv, reply))
        })
        .await;

        let (connection, send, recv, reply) = match joined {
            Ok(Ok(joined)) => joined,
            Ok(Err(error)) => {
                endpoint.close().await;
                return Err(JoinError::Failed(error));
            }
            Err(_) => {
                endpoint.close().await;
                return Err(JoinError::Failed(anyhow!(
                    "the host did not answer in time"
                )));
            }
        };

        match reply {
            HelloReply::Welcome { guest_id, manifest } => Ok(Self {
                endpoint,
                connection,
                control: recv,
                _control_send: send,
                guest_id,
                manifest,
            }),
            HelloReply::Reject { reason } => {
                connection.close(0u32.into(), b"rejected");
                endpoint.close().await;
                Err(JoinError::Rejected(reason))
            }
        }
    }

    /// How the packets travel right now and how long a round trip takes.
    pub fn route(&self) -> Option<(Route, Duration)> {
        link::route(&self.connection)
    }

    pub fn opener(&self) -> Opener {
        Opener {
            connection: self.connection.clone(),
        }
    }

    /// Resolves when the session is over, whoever ended it.
    pub async fn ended(&mut self) -> End {
        let expiry = Duration::from_secs(self.manifest.session.expires_in);
        let expired = tokio::time::sleep(expiry);
        tokio::pin!(expired);
        loop {
            tokio::select! {
                // The host says the same a moment earlier or later; this is
                // the guest's own guarantee that nothing outlives the session.
                _ = &mut expired => return End::Host(EndReason::Expired),
                event = frame::read::<_, HostEvent>(&mut self.control, MAX_FRAME) => match event {
                    Ok(HostEvent::Ended { reason }) => return End::Host(reason),
                    Ok(HostEvent::Manifest { manifest }) => self.manifest = manifest,
                    Err(_) => return End::Lost,
                },
            }
        }
    }

    pub async fn close(self) {
        self.connection.close(0u32.into(), b"left");
        self.endpoint.close().await;
    }
}

impl Opener {
    /// How the packets travel right now and how long a round trip takes.
    pub fn route(&self) -> Option<(Route, Duration)> {
        link::route(&self.connection)
    }

    /// Opens a connection to `host:port` on the host's side of the session.
    pub async fn open(&self, host: &str, port: u16) -> Result<(SendStream, RecvStream), OpenError> {
        let opened = tokio::time::timeout(OPEN_TIMEOUT, async {
            let (mut send, mut recv) = self
                .connection
                .open_bi()
                .await
                .map_err(|error| anyhow!("{error}"))?;
            frame::write(
                &mut send,
                &Open {
                    host: host.to_string(),
                    port,
                },
            )
            .await?;
            let reply: OpenReply = frame::read(&mut recv, 4096).await?;
            anyhow::Ok((send, recv, reply))
        })
        .await;

        match opened {
            Ok(Ok((send, recv, OpenReply::Ok))) => Ok((send, recv)),
            Ok(Ok((_, _, OpenReply::Denied))) => Err(OpenError::Denied),
            Ok(Ok((_, _, OpenReply::Unreachable))) => Err(OpenError::Unreachable),
            Ok(Err(error)) => Err(OpenError::Failed(error)),
            Err(_) => Err(OpenError::Failed(anyhow!(
                "the host did not answer in time"
            ))),
        }
    }
}
