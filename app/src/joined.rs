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
    guest::{self, GuestLink, NamePolicy, Termination, Tunnel},
    invite::DeviceSecret,
    protocol::{clean, Manifest},
    qr,
};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

/// The route and the countdown are refreshed this often.
const REFRESH: Duration = Duration::from_secs(1);

/// Everything the window shows about a joined session.
#[derive(Debug, Clone, Serialize)]
pub struct JoinedView {
    pub environments: Vec<JoinedEnvironment>,
    pub remaining: u64,
    /// `direct` or `relayed`, once known.
    pub route: Option<String>,
    pub latency: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JoinedEnvironment {
    pub name: String,
    /// Where to start, as the host declared it.
    pub entrypoint: Option<String>,
    pub services: Vec<JoinedService>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JoinedService {
    /// `shop.test:443`.
    pub address: String,
    /// What a browser opens: `https://shop.test`.
    pub url: String,
    pub tls: bool,
    /// Certified by this device's own authority: opens without a warning.
    pub certified: bool,
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

        let view = view_of(&link.manifest, &certified);
        let urls = std::sync::Arc::new(urls_of(&view));
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

/// The session as the window shows it. Everything in it comes from the
/// host: cleaned, and addresses rebuilt from the names and ports rather than
/// taken as given.
fn view_of(manifest: &Manifest, certified: &HashSet<(String, u16)>) -> JoinedView {
    let environments = manifest
        .environments
        .iter()
        .map(|(name, environment)| {
            let services: Vec<JoinedService> = environment
                .services
                .iter()
                .map(|service| {
                    let host = service.host.to_ascii_lowercase();
                    let tls = service.tls.is_some();
                    JoinedService {
                        address: format!("{host}:{}", service.port),
                        url: url(&host, service.port, tls),
                        tls,
                        certified: certified.contains(&(host, service.port)),
                    }
                })
                .collect();
            // The host's entry point, if it is one of the shared addresses.
            let entrypoint = environment.entrypoint.as_deref().and_then(|entrypoint| {
                let parsed = url::Url::parse(entrypoint).ok()?;
                if !matches!(parsed.scheme(), "http" | "https") {
                    return None;
                }
                let host = parsed.host_str()?.to_ascii_lowercase();
                let port = parsed.port_or_known_default()?;
                let start = url(&host, port, parsed.scheme() == "https");
                services
                    .iter()
                    .any(|service| service.url == start)
                    .then(|| {
                        // Percent-encoded by the parser: nothing but a path.
                        format!("{start}{}", parsed.path().trim_end_matches('/'))
                    })
            });
            JoinedEnvironment {
                name: clean(name, 64),
                entrypoint,
                services,
            }
        })
        .collect();
    JoinedView {
        environments,
        remaining: manifest.session.expires_in,
        route: None,
        latency: None,
    }
}

/// `https://shop.test`, `http://shop.test:5173`.
fn url(host: &str, port: u16, tls: bool) -> String {
    match (tls, port) {
        (true, 443) => format!("https://{host}"),
        (false, 80) => format!("http://{host}"),
        (true, port) => format!("https://{host}:{port}"),
        (false, port) => format!("http://{host}:{port}"),
    }
}

fn urls_of(view: &JoinedView) -> HashSet<String> {
    view.environments
        .iter()
        .flat_map(|environment| {
            environment
                .services
                .iter()
                .map(|service| service.url.clone())
                .chain(environment.entrypoint.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use devshare_core::protocol::{Environment, Service, SessionInfo, Tls, Transport};

    use super::*;

    fn manifest(entrypoint: &str) -> Manifest {
        let service = |host: &str, port, tls: bool| Service {
            host: host.into(),
            port,
            protocol: Transport::Tcp,
            tls: tls.then(|| Tls {
                sha256: "00".repeat(32),
            }),
        };
        Manifest {
            protocol: 2,
            manifest_version: 1,
            session: SessionInfo {
                id: "s".into(),
                lifetime: 300,
                expires_in: 120,
                max_guests: 3,
            },
            environments: BTreeMap::from([(
                "shop".to_string(),
                Environment {
                    entrypoint: Some(entrypoint.into()),
                    dns: vec![],
                    services: vec![
                        service("Shop.test", 443, true),
                        service("shop.test", 5173, false),
                    ],
                },
            )]),
        }
    }

    #[test]
    fn the_window_gets_addresses_built_from_names_and_ports() {
        let certified = HashSet::from([("shop.test".to_string(), 443)]);
        let view = view_of(&manifest("https://shop.test/cart"), &certified);
        let shop = &view.environments[0];
        assert_eq!(shop.entrypoint.as_deref(), Some("https://shop.test/cart"));
        assert_eq!(shop.services[0].url, "https://shop.test");
        assert!(shop.services[0].certified);
        assert_eq!(shop.services[1].url, "http://shop.test:5173");
        assert!(!shop.services[1].certified);
        assert_eq!(view.remaining, 120);

        let urls = urls_of(&view);
        assert!(urls.contains("https://shop.test"));
        assert!(urls.contains("https://shop.test/cart"));
        assert!(!urls.contains("https://evil.example"));
    }

    #[test]
    fn an_entrypoint_that_is_not_a_shared_address_is_dropped() {
        for entrypoint in [
            "https://evil.example/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "http://shop.test:9999/",
        ] {
            let view = view_of(&manifest(entrypoint), &HashSet::new());
            assert_eq!(view.environments[0].entrypoint, None, "{entrypoint}");
        }
        let view = view_of(&manifest("https://shop.test/"), &HashSet::new());
        assert_eq!(
            view.environments[0].entrypoint.as_deref(),
            Some("https://shop.test")
        );
    }
}
