//! One sharing session as the owner's window sees it: a picture of the
//! session sent whenever something changes, and the three things the owner
//! can do about it — disconnect someone, invite again, stop.

use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};

use anyhow::Result;
use devshare_core::{
    host::{Activity, Share, ShareOptions},
    probe::Probe,
    protocol::code,
    qr,
};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

/// How many lines of activity the window keeps.
const NOTICES: usize = 30;
/// The countdown and the routes are refreshed this often when nothing happens.
const REFRESH: Duration = Duration::from_secs(1);

/// Everything the window shows about a session.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    /// `7GX2-KLM9`.
    pub code: String,
    pub link: String,
    /// The link as a QR code, an SVG image.
    pub qr: String,
    /// Whether the link and its QR code work from another network than
    /// this one.
    pub anywhere: bool,
    /// False after someone was disconnected, until a new invitation.
    pub invitation_open: bool,
    pub remaining: u64,
    pub max_guests: u32,
    pub environments: Vec<EnvironmentView>,
    pub guests: Vec<GuestView>,
    /// Most recent first.
    pub notices: Vec<Notice>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvironmentView {
    pub name: String,
    pub services: Vec<ServiceView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceView {
    pub address: String,
    pub tls: bool,
    /// Why guests may not get what they expect from this service.
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GuestView {
    pub id: u32,
    /// The login on the guest's own computer, as that computer reports it.
    pub user: Option<String>,
    pub computer: String,
    pub platform: String,
    /// `direct` or `relayed`, once the link is established.
    pub route: Option<String>,
    pub latency: Option<u64>,
    /// Seconds since it joined.
    pub connected: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Notice {
    pub id: u64,
    pub text: String,
    /// Whether it deserves the owner's attention.
    pub warning: bool,
}

/// What the window is told.
#[derive(Debug, Clone)]
pub enum Update {
    Session(Box<Snapshot>),
    /// The session is over, and why.
    Ended(String),
}

enum Command {
    Disconnect(u32, oneshot::Sender<bool>),
    Invite(oneshot::Sender<Result<(), String>>),
    Stop(oneshot::Sender<()>),
}

/// A running session. Cheap to clone; the session lives until it expires or
/// [`Session::stop`] is called.
#[derive(Clone)]
pub struct Session {
    commands: mpsc::Sender<Command>,
}

impl Session {
    /// Starts sharing. `on_update` is called from then on with the state of
    /// the session, and one last time when it ends.
    pub async fn start(
        options: ShareOptions,
        on_update: impl Fn(Update) + Send + 'static,
    ) -> Result<Self> {
        let share = Share::start(options).await?;
        let (commands, inbox) = mpsc::channel(8);
        tokio::spawn(run(share, inbox, on_update));
        Ok(Self { commands })
    }

    /// Disconnects a guest. Its invitation is withdrawn with it, so it cannot
    /// come back; [`Session::invite`] opens the session to others again.
    pub async fn disconnect(&self, guest: u32) -> bool {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Disconnect(guest, reply))
            .await
            .ok();
        answer.await.unwrap_or(false)
    }

    /// Replaces the invitation by a new one.
    pub async fn invite(&self) -> Result<(), String> {
        let (reply, answer) = oneshot::channel();
        self.commands.send(Command::Invite(reply)).await.ok();
        answer
            .await
            .unwrap_or_else(|_| Err("the session is over".into()))
    }

    /// Ends the session and waits until the guests were told.
    pub async fn stop(&self) {
        let (reply, answer) = oneshot::channel();
        if self.commands.send(Command::Stop(reply)).await.is_ok() {
            answer.await.ok();
        }
    }
}

struct Journal {
    notices: VecDeque<Notice>,
    next: u64,
    /// The names of the guests seen so far: a guest that left is still
    /// named in the line that says so.
    names: HashMap<u32, String>,
}

impl Journal {
    fn add(&mut self, warning: bool, text: String) {
        self.next += 1;
        self.notices.push_front(Notice {
            id: self.next,
            text,
            warning,
        });
        self.notices.truncate(NOTICES);
    }

    fn name(&self, guest: u32) -> String {
        self.names
            .get(&guest)
            .cloned()
            .unwrap_or_else(|| format!("guest {guest}"))
    }
}

async fn run(
    mut share: Share,
    mut inbox: mpsc::Receiver<Command>,
    on_update: impl Fn(Update) + Send + 'static,
) {
    let mut journal = Journal {
        notices: VecDeque::new(),
        next: 0,
        names: HashMap::new(),
    };
    let mut refresh = tokio::time::interval(REFRESH);

    loop {
        tokio::select! {
            _ = refresh.tick() => {}
            activity = share.activity() => match activity {
                Some(Activity::GuestJoined { id, device }) => {
                    journal.add(false, format!("{} joined.", device.label()));
                    journal.names.insert(id, device.label());
                }
                Some(Activity::GuestLeft { id }) => {
                    journal.add(false, format!("{} left.", journal.name(id)));
                }
                Some(Activity::Refused { reason }) => {
                    journal.add(true, format!("A device was refused: {reason}."));
                }
                Some(Activity::Denied { guest, host, port }) => journal.add(
                    true,
                    format!("{} asked for {host}:{port}, which is not shared.", journal.name(guest)),
                ),
                Some(Activity::Unreachable { guest, host, port }) => journal.add(
                    true,
                    format!(
                        "{} asked for {host}:{port}, which does not answer on this machine.",
                        journal.name(guest)
                    ),
                ),
                Some(Activity::Ended { reason }) => {
                    share.stop().await;
                    on_update(Update::Ended(reason.to_string()));
                    return;
                }
                None => return,
            },
            command = inbox.recv() => match command {
                Some(Command::Disconnect(guest, reply)) => {
                    let disconnected = share.revoke(guest).await;
                    if disconnected {
                        journal.add(false, format!("You disconnected {}.", journal.name(guest)));
                    }
                    reply.send(disconnected).ok();
                }
                Some(Command::Invite(reply)) => {
                    let invited = share.invite().await;
                    if invited.is_ok() {
                        journal.add(false, "New invitation. The previous code no longer works.".into());
                    }
                    reply.send(invited.map(|_| ()).map_err(|error| format!("{error:#}"))).ok();
                }
                Some(Command::Stop(reply)) => {
                    share.stop().await;
                    on_update(Update::Ended("you stopped sharing".into()));
                    reply.send(()).ok();
                    return;
                }
                // The window is gone: nobody is left to watch over the session.
                None => {
                    share.stop().await;
                    return;
                }
            },
        }
        on_update(Update::Session(Box::new(snapshot(&share, &journal))));
    }
}

fn snapshot(share: &Share, journal: &Journal) -> Snapshot {
    let manifest = share.manifest();
    let code = code::display(&share.code());
    let link = share.link();

    let environments = manifest
        .environments
        .iter()
        .map(|(name, environment)| EnvironmentView {
            name: name.clone(),
            services: environment
                .services
                .iter()
                .map(|service| {
                    let check = share
                        .checks()
                        .iter()
                        .find(|check| check.host == service.host && check.port == service.port);
                    let warning = match check.map(|check| &check.probe) {
                        Some(Probe::Down) => {
                            Some("did not answer on this machine when sharing started".to_string())
                        }
                        Some(Probe::Tls {
                            covers_host: false, ..
                        }) => Some(format!("its certificate is not for {}", service.host)),
                        _ => None,
                    };
                    ServiceView {
                        address: format!("{}:{}", service.host, service.port),
                        tls: service.tls.is_some(),
                        warning,
                    }
                })
                .collect(),
        })
        .collect();

    let guests = share
        .guests()
        .into_iter()
        .map(|guest| GuestView {
            id: guest.id,
            user: guest.device.user,
            computer: guest.device.name,
            platform: guest.device.platform,
            route: guest.route.map(|(route, _)| route.to_string()),
            latency: guest.route.map(|(_, latency)| latency.as_millis() as u64),
            connected: guest.connected.as_secs(),
        })
        .collect();

    Snapshot {
        qr: qr::svg(&link),
        code,
        link,
        anywhere: share.works_from_anywhere(),
        invitation_open: share.invitation_open(),
        remaining: share.remaining().as_secs(),
        max_guests: manifest.session.max_guests,
        environments,
        guests,
        notices: journal.notices.iter().cloned().collect(),
    }
}
