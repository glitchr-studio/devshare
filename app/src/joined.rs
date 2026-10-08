//! A session this computer joined, as the window sees it: what is shared and
//! where to open it, how the link goes, how long is left, and leaving.
//!
//! Joining goes through the same core as `devshare join`: the same
//! invitation, the same names, the same privileged helper for the network
//! interface, the same device certificate authority when there is one.

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use anyhow::Result;
use devshare_core::{
    environment::{self, Settings},
    guest::{self, GuestLink, NamePolicy, Summary, SummaryEnvironment, Termination, Tunnel},
    invite::DeviceSecret,
    qr,
};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

/// The route and the countdown are refreshed this often.
const REFRESH: Duration = Duration::from_secs(1);

/// Everything the window shows about a joined session.
#[derive(Debug, Clone, Serialize)]
pub struct JoinedView {
    pub environments: Vec<SummaryEnvironment>,
    pub remaining: u64,
    /// `direct` or `relayed`, once known.
    pub route: Option<String>,
    pub latency: Option<u64>,
}

/// What the window is told.
#[derive(Debug, Clone)]
pub enum JoinUpdate {
    Joined(Box<JoinedView>),
    /// The session is over for this computer, and why.
    Left(String),
}

/// A joined session. Cheap to clone; it lasts until the host ends it or
/// [`Joined::leave`] is called.
#[derive(Clone)]
pub struct Joined {
    leave: mpsc::Sender<oneshot::Sender<()>>,
    /// The addresses of the session: the only ones the window may open.
    urls: std::sync::Arc<HashSet<String>>,
}

impl Joined {
    /// Joins with `invitation`, in any of its forms. `server` is the control
    /// plane to ask when the invitation does not say.
    pub async fn start(
        invitation: &str,
        server: Option<String>,
        on_update: impl Fn(JoinUpdate) + Send + 'static,
    ) -> Result<Self> {
        let settings = Settings::load().unwrap_or_default();
        devshare_core::link::use_relay(settings.relay.clone());
        let server = server.or_else(|| qr::server_of(invitation));
        let server = environment::server(server, None, &settings);
        // Before the host sees this computer: a guest that cannot make the
        // interface never takes one of its places.
        Tunnel::check_rights()?;
        let secret = DeviceSecret::load().unwrap_or_else(|error| {
            tracing::warn!("no device secret ({error:#}): joining without a lasting identity");
            DeviceSecret::random()
        });
        let link = GuestLink::join_as(invitation, &server, guest::this_device(), &secret).await?;

        // The app never trusts names outside the development domains: that
        // is for `devshare join --trust-names`, typed by someone who knows.
        let names = NamePolicy::with(Some(settings.domain()));
        let termination = Termination::for_session(&link.manifest, &settings.domain());
        let certified: HashSet<(String, u16)> = termination
            .as_ref()
            .map(|tls| {
                tls.certified(&link.manifest)
                    .into_iter()
                    .map(|service| (service.host.to_ascii_lowercase(), service.port))
                    .collect()
            })
            .unwrap_or_default();
        let tunnel = match Tunnel::start(link.opener(), &link.manifest, &names, termination).await {
            Ok(tunnel) => tunnel,
            Err(error) => {
                link.close().await;
                return Err(error);
            }
        };

        let summary = Summary::of(&link.manifest, &certified);
        let urls = std::sync::Arc::new(summary.urls());
        let view = JoinedView {
            environments: summary.environments,
            remaining: link.manifest.session.expires_in,
            route: None,
            latency: None,
        };
        let deadline = Instant::now() + Duration::from_secs(link.manifest.session.expires_in);
        let (leave, inbox) = mpsc::channel(1);
        tokio::spawn(run(link, tunnel, view, deadline, inbox, on_update));
        Ok(Self { leave, urls })
    }

    /// Leaves the session: the interface and the names go at once.
    pub async fn leave(&self) {
        let (done, left) = oneshot::channel();
        if self.leave.send(done).await.is_ok() {
            left.await.ok();
        }
    }

    /// Whether `url` is one of the session's addresses.
    pub fn may_open(&self, url: &str) -> bool {
        self.urls.contains(url)
    }
}

async fn run(
    mut link: GuestLink,
    tunnel: Tunnel,
    mut view: JoinedView,
    deadline: Instant,
    mut inbox: mpsc::Receiver<oneshot::Sender<()>>,
    on_update: impl Fn(JoinUpdate),
) {
    let mut asked = None;
    let reason = {
        let opener = link.opener();
        let ended = link.ended();
        tokio::pin!(ended);
        let mut tick = tokio::time::interval(REFRESH);
        loop {
            tokio::select! {
                end = &mut ended => break end.to_string(),
                done = inbox.recv() => {
                    asked = done;
                    break "you left".to_string();
                }
                _ = tick.tick() => {
                    view.remaining = deadline.saturating_duration_since(Instant::now()).as_secs();
                    if let Some((route, rtt)) = opener.route() {
                        view.route = Some(route.to_string());
                        view.latency = Some(rtt.as_millis() as u64);
                    }
                    on_update(JoinUpdate::Joined(Box::new(view.clone())));
                }
            }
        }
    };
    drop(tunnel);
    link.close().await;
    on_update(JoinUpdate::Left(reason));
    if let Some(done) = asked {
        done.send(()).ok();
    }
}
