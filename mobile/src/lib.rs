//! The DevShare core as the mobile apps reach it, through UniFFI: join a
//! session with its invitation, and browse it in the app's own browser.
//!
//! Without its packet-tunnel extension, an app has no network interface to
//! give the system: its browser goes through a proxy on the phone's
//! loopback instead (see [`devshare_core::guest::proxy`]), which carries
//! each connection into the session. HTTPS stays end to end, and the
//! browser accepts exactly the certificate the host saw when it shared the
//! service: [`GuestSession::certificate_matches`].

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use devshare_core::{
    environment::{self, Settings},
    guest::{
        proxy::{Proxy, Reach},
        AddressPlan, GuestLink, NamePolicy, Summary,
    },
    invite::DeviceSecret,
    protocol::{normalize_host, Device},
    qr,
};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

uniffi::setup_scaffolding!();

/// How often the route is read while the session lasts.
const REFRESH: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MobileError {
    /// Joining failed, for the reason given, written for the person who
    /// tried.
    #[error("{reason}")]
    Refused { reason: String },
}

impl From<anyhow::Error> for MobileError {
    fn from(error: anyhow::Error) -> Self {
        Self::Refused {
            reason: format!("{error:#}"),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SharedEnvironment {
    pub name: String,
    pub entrypoint: Option<String>,
    pub services: Vec<SharedService>,
    /// Ways to open it with another app than the browser: the app offers
    /// the kinds it has a provider for.
    pub launches: Vec<Launch>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct Launch {
    /// `expo`: Expo Go, for a React Native app's Metro server.
    pub kind: String,
    pub url: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SharedService {
    /// `shop.test:443`.
    pub address: String,
    /// What the browser opens: `https://shop.test`.
    pub url: String,
    pub tls: bool,
    /// What speaks on it when it is not the web: `metro`.
    pub kind: Option<String>,
}

/// A joined session. It lasts until the host ends it or
/// [`GuestSession::leave`] is called; the proxy stops when the app lets go
/// of it.
#[derive(uniffi::Object)]
pub struct GuestSession {
    environments: Vec<SharedEnvironment>,
    urls: HashSet<String>,
    names: HashSet<String>,
    /// `(name, port)` to the SHA-256 the host saw.
    pins: Vec<((String, u16), String)>,
    proxy: Proxy,
    deadline: Instant,
    state: Arc<Mutex<State>>,
    leave: Mutex<Option<oneshot::Sender<oneshot::Sender<()>>>>,
}

#[derive(Default)]
struct State {
    route: Option<String>,
    ended: Option<String>,
}

/// Joins a session with its invitation, in any of its forms: the link its
/// QR code holds, the short code. `device_name` is what the host's list of
/// guests shows; `data_folder` is where this device keeps its lasting
/// identity, so that a host that disconnected it keeps it out.
#[uniffi::export(async_runtime = "tokio")]
pub async fn join(
    invitation: String,
    device_name: String,
    data_folder: String,
) -> Result<Arc<GuestSession>, MobileError> {
    let settings = Settings::default();
    devshare_core::link::use_relay(None);
    let server = environment::server(qr::server_of(&invitation), None, &settings);
    let secret = DeviceSecret::load_from(std::path::Path::new(&data_folder))
        .unwrap_or_else(|_| DeviceSecret::random());
    let device = Device {
        name: devshare_core::protocol::clean(&device_name, 64),
        platform: std::env::consts::OS.to_string(),
        user: None,
    };
    let link = GuestLink::join_as(invitation.trim(), &server, device, &secret)
        .await
        .map_err(|error| MobileError::Refused {
            reason: error.to_string(),
        })?;

    let started = start(&link).await;
    let (environments, urls, names, pins, proxy) = match started {
        Ok(started) => started,
        Err(error) => {
            link.close().await;
            return Err(error.into());
        }
    };
    let deadline = Instant::now() + Duration::from_secs(link.manifest.session.expires_in);
    let state = Arc::new(Mutex::new(State::default()));
    let (leave, asked) = oneshot::channel();
    tokio::spawn(run(link, asked, state.clone()));
    Ok(Arc::new(GuestSession {
        environments,
        urls,
        names,
        pins,
        proxy,
        deadline,
        state,
        leave: Mutex::new(Some(leave)),
    }))
}

type Started = (
    Vec<SharedEnvironment>,
    HashSet<String>,
    HashSet<String>,
    Vec<((String, u16), String)>,
    Proxy,
);

async fn start(link: &GuestLink) -> anyhow::Result<Started> {
    // The same rule as a computer's: names that could be real sites are
    // not joined, their traffic would go to the host.
    let policy = NamePolicy::with(None);
    let refused = policy.refused(&link.manifest);
    if !refused.is_empty() {
        anyhow::bail!(
            "not joined: the session names {}, which could be a real site",
            refused.join(", ")
        );
    }
    let plan = AddressPlan::new(&link.manifest)?;
    let proxy = Proxy::start(link.opener(), Arc::new(plan), Reach::Internet).await?;

    let summary = Summary::of(&link.manifest, &HashSet::new());
    let urls = summary.urls();
    let mut names = HashSet::new();
    let mut pins = Vec::new();
    let environments = summary
        .environments
        .into_iter()
        .map(|environment| SharedEnvironment {
            name: environment.name,
            entrypoint: environment.entrypoint,
            services: environment
                .services
                .into_iter()
                .map(|service| {
                    names.insert(service.host.clone());
                    if let Some(sha256) = &service.sha256 {
                        pins.push(((service.host.clone(), service.port), sha256.clone()));
                    }
                    SharedService {
                        address: service.address,
                        url: service.url,
                        tls: service.sha256.is_some(),
                        kind: service.kind,
                    }
                })
                .collect(),
            launches: environment
                .launches
                .into_iter()
                .map(|launch| Launch {
                    kind: launch.kind,
                    url: launch.url,
                })
                .collect(),
        })
        .collect();
    Ok((environments, urls, names, pins, proxy))
}

async fn run(
    mut link: GuestLink,
    mut asked: oneshot::Receiver<oneshot::Sender<()>>,
    state: Arc<Mutex<State>>,
) {
    let mut done = None;
    let reason = {
        let opener = link.opener();
        let ended = link.ended();
        tokio::pin!(ended);
        let mut tick = tokio::time::interval(REFRESH);
        loop {
            tokio::select! {
                end = &mut ended => break end.to_string(),
                reply = &mut asked => {
                    done = reply.ok();
                    break "you left".to_string();
                }
                _ = tick.tick() => {
                    if let Some((route, rtt)) = opener.route() {
                        state.lock().unwrap().route = Some(format!("{route}, {} ms", rtt.as_millis()));
                    }
                }
            }
        }
    };
    link.close().await;
    state.lock().unwrap().ended = Some(reason);
    if let Some(done) = done {
        done.send(()).ok();
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl GuestSession {
    pub fn environments(&self) -> Vec<SharedEnvironment> {
        self.environments.clone()
    }

    /// The proxy the app's browser goes through, on the loopback.
    pub fn proxy_port(&self) -> u16 {
        self.proxy.port()
    }

    pub fn proxy_user(&self) -> String {
        devshare_core::guest::proxy::USER.to_string()
    }

    pub fn proxy_password(&self) -> String {
        self.proxy.password().to_string()
    }

    /// Whether `url` is one of the session's addresses: the only ones the
    /// app offers to open.
    pub fn may_open(&self, url: String) -> bool {
        self.urls.contains(&url)
    }

    /// Whether the browser may go to `host` at all: a name of the session.
    pub fn is_shared(&self, host: String) -> bool {
        self.names.contains(&normalize_host(&host))
    }

    /// Whether `certificate` (DER) is the one the host saw on `host:port`
    /// when it shared it. The browser accepts it then, whoever issued it.
    pub fn certificate_matches(&self, host: String, port: u16, certificate: Vec<u8>) -> bool {
        let wanted = (normalize_host(&host), port);
        let seen: String = Sha256::digest(&certificate)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.pins
            .iter()
            .any(|(service, sha256)| *service == wanted && sha256.eq_ignore_ascii_case(&seen))
    }

    pub fn remaining_seconds(&self) -> u64 {
        self.deadline
            .saturating_duration_since(Instant::now())
            .as_secs()
    }

    /// `direct, 12 ms` or `relayed, 80 ms`, once known.
    pub fn route(&self) -> Option<String> {
        self.state.lock().unwrap().route.clone()
    }

    /// Why the session is over for this device, once it is.
    pub fn ended(&self) -> Option<String> {
        self.state.lock().unwrap().ended.clone()
    }

    /// Leaves the session.
    pub async fn leave(&self) {
        let sender = self.leave.lock().unwrap().take();
        if let Some(sender) = sender {
            let (done, left) = oneshot::channel();
            if sender.send(done).is_ok() {
                left.await.ok();
            }
        }
    }
}
