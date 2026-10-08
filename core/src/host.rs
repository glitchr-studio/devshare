//! The host side of a sharing session.
//!
//! The host agent never routes: for each stream a guest opens it dials one
//! service of the session, or refuses. Anything that is not in
//! [`Selection::routes`] is unreachable, whatever the guest asks for.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{anyhow, Result};
use devshare_protocol::{
    code, normalize_host, Confirm, Device, EndReason, Hello, HelloReply, HostEvent, Manifest, Open,
    OpenReply, RejectReason, SessionInfo, Tls, MANIFEST_VERSION, MAX_FRAME, PROTOCOL_VERSION,
};
use iroh::{
    endpoint::{Connection, Incoming, RecvStream, SendStream},
    Endpoint, EndpointAddr, EndpointId,
};
use tokio::{net::TcpStream, sync::mpsc, time::Instant};

use crate::{
    control,
    environment::Selection,
    frame,
    invite::{self, Pake, GUEST_PROOF, HOST_PROOF},
    link::{self, Route},
    probe::{self, Probe},
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// Time left to guests to read the end of a session before the link closes.
const FAREWELL: Duration = Duration::from_millis(500);

pub struct ShareOptions {
    pub selection: Selection,
    pub lifetime: Duration,
    pub max_guests: u32,
    /// URL of the control plane.
    pub server: String,
    /// The public address of the invitation page, when there is one: the
    /// invitation link points there rather than at the control plane.
    pub join: Option<String>,
}

/// What happens during a session, for whoever displays it.
#[derive(Debug, Clone)]
pub enum Activity {
    GuestJoined {
        id: u32,
        device: Device,
    },
    GuestLeft {
        id: u32,
    },
    Refused {
        reason: RejectReason,
    },
    /// Too many wrong codes: the invitation was withdrawn. A new one opens
    /// the session again.
    Locked {
        attempts: u32,
    },
    Denied {
        guest: u32,
        host: String,
        port: u16,
    },
    Unreachable {
        guest: u32,
        host: String,
        port: u16,
    },
    Ended {
        reason: EndReason,
    },
}

/// What the host found when it looked at a service it is about to share.
#[derive(Debug, Clone)]
pub struct ServiceCheck {
    pub host: String,
    pub port: u16,
    pub probe: Probe,
    /// The addresses meaning "this machine" its first page points at: a
    /// guest cannot follow them. See [`probe::local_references`].
    pub local: Vec<String>,
}

/// Connects to every service of the selection, all at once, and writes the
/// certificates it saw into what guests will receive.
async fn check_services(selection: &mut Selection) -> Vec<ServiceCheck> {
    let probes: Vec<_> = selection
        .routes
        .iter()
        .map(|((host, port), target)| {
            let (host, port, target) = (host.clone(), *port, target.clone());
            tokio::spawn(async move {
                let probe = probe::probe(&target, &host).await;
                let local = match &probe {
                    Probe::Down => Vec::new(),
                    probe => {
                        let tls = matches!(probe, Probe::Tls { .. });
                        probe::local_references(&target, &format!("{host}:{port}"), tls).await
                    }
                };
                ServiceCheck {
                    host,
                    port,
                    probe,
                    local,
                }
            })
        })
        .collect();

    let mut checks = Vec::new();
    for probe in probes {
        checks.extend(probe.await.ok());
    }
    checks.sort_by(|a, b| (&a.host, a.port).cmp(&(&b.host, b.port)));

    for service in selection
        .environments
        .values_mut()
        .flat_map(|environment| &mut environment.services)
    {
        let seen = checks
            .iter()
            .find(|check| check.host == service.host && check.port == service.port);
        if let Some(ServiceCheck {
            probe: Probe::Tls { sha256, .. },
            ..
        }) = seen
        {
            service.tls = Some(Tls {
                sha256: sha256.clone(),
            });
        }
    }
    checks
}

/// A connected guest, as the host can describe it at any moment.
#[derive(Debug, Clone)]
pub struct GuestStatus {
    pub id: u32,
    pub device: Device,
    /// How its packets travel and how long a round trip takes.
    pub route: Option<(Route, Duration)>,
    /// How long it has been connected.
    pub connected: Duration,
}

struct Guest {
    device: Device,
    /// Its key toward this host: what a revocation bans.
    key: EndpointId,
    connection: Connection,
    joined: Instant,
    /// Tells the guest the session is over for it.
    end: mpsc::Sender<EndReason>,
}

/// Wrong codes a session takes before it stops listening to any: enough for
/// a few typos, not for guessing. A new invitation starts the count over.
const MAX_WRONG_CODES: u32 = 20;

struct State {
    guests: HashMap<u32, Guest>,
    next_id: u32,
    /// Wrong codes presented since the invitation was issued.
    wrong_codes: u32,
    /// The invitation in force: its code, known to this process only, the
    /// key the control plane knows it by, and what lets the host withdraw it.
    code: String,
    lookup: String,
    owner_token: String,
    /// False once too many wrong codes were tried: nobody else can join
    /// until the host issues another invitation.
    invitation_open: bool,
    /// The keys of the devices the host disconnected: they may not come back.
    banned: HashSet<EndpointId>,
    ended: Option<EndReason>,
}

struct Shared {
    session_id: String,
    /// This host's identity: both sides bind the code exchange to it.
    host_id: EndpointId,
    selection: Selection,
    lifetime: Duration,
    deadline: Instant,
    max_guests: u32,
    state: Mutex<State>,
    activity: mpsc::UnboundedSender<Activity>,
}

/// A running session.
pub struct Share {
    shared: Arc<Shared>,
    endpoint: Endpoint,
    server: String,
    join: Option<String>,
    checks: Vec<ServiceCheck>,
    activity: mpsc::UnboundedReceiver<Activity>,
}

impl Share {
    pub async fn start(mut options: ShareOptions) -> Result<Share> {
        let (checks, endpoint) =
            tokio::join!(check_services(&mut options.selection), link::endpoint(true));
        let endpoint = endpoint?;
        let invitation = announce(&options.server, &endpoint.addr(), options.lifetime).await?;
        // A session lasts as long as its invitation on the control plane,
        // unless it has no time limit: its link then keeps working, and its
        // short code for as long as the control plane keeps it.
        let lifetime = if crate::protocol::unlimited(options.lifetime.as_secs()) {
            options.lifetime
        } else {
            options.lifetime.min(Duration::from_secs(invitation.ttl))
        };

        let (activity_tx, activity_rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            session_id: hex(&rand::random::<[u8; 8]>()),
            host_id: endpoint.id(),
            selection: options.selection,
            lifetime,
            deadline: Instant::now() + lifetime,
            max_guests: options.max_guests,
            state: Mutex::new(State {
                guests: HashMap::new(),
                next_id: 1,
                wrong_codes: 0,
                code: invitation.code,
                lookup: invitation.lookup,
                owner_token: invitation.owner_token,
                invitation_open: true,
                banned: HashSet::new(),
                ended: None,
            }),
            activity: activity_tx,
        });

        tokio::spawn(accept_guests(endpoint.clone(), shared.clone()));
        tokio::spawn({
            let shared = shared.clone();
            async move {
                tokio::time::sleep_until(shared.deadline).await;
                shared.end(EndReason::Expired);
            }
        });

        Ok(Share {
            shared,
            endpoint,
            server: options.server,
            join: options.join,
            checks,
            activity: activity_rx,
        })
    }

    /// The invitation code in force. It changes with [`Share::invite`].
    pub fn code(&self) -> String {
        self.shared.state.lock().unwrap().code.clone()
    }

    /// Whether the control plane of this session is of this version: see
    /// [`control::is_current`].
    pub async fn control_plane_is_current(&self) -> bool {
        control::is_current(&self.server).await
    }

    /// The invitation: one link, for every guest and every client.
    ///
    /// With a public invitation page, it is that page's address with the
    /// invitation after a `#`. Otherwise it is the control plane's address
    /// and the code, and when the control plane is this very machine, which a guest elsewhere cannot reach, the
    /// link also carries this host's own address after a `#`: the same link
    /// then works from this network and from any other. A browser never
    /// sends that part anywhere.
    pub fn link(&self) -> String {
        // A public page, the same for every invitation: all of this one is
        // after the `#`, which a browser keeps to itself.
        if let Some(join) = &self.join {
            let carried = crate::direct::for_page(&self.endpoint.addr(), &self.code());
            return format!("{join}/#{carried}");
        }
        let link = crate::qr::link(&self.server, &self.code());
        if crate::qr::on_this_machine(&self.server) {
            format!("{link}#{}", crate::direct::for_link(&self.endpoint.addr()))
        } else {
            link
        }
    }

    /// Whether the invitation works from another network: it does when the
    /// control plane is somewhere every guest can reach, or when the link
    /// carries this host's address and the host is behind a relay.
    pub fn works_from_anywhere(&self) -> bool {
        let carried = self.join.is_some() || crate::qr::on_this_machine(&self.server);
        !carried || self.crosses_networks()
    }

    /// The invitation with this host's own address in it, for a guest that
    /// cannot reach the control plane. See [`crate::direct`].
    pub fn direct_invitation(&self) -> String {
        crate::direct::encode(&self.endpoint.addr(), &self.code())
    }

    /// Whether that invitation works from another network: it does when the
    /// host is reachable through a relay.
    pub fn crosses_networks(&self) -> bool {
        crate::direct::crosses_networks(&self.endpoint.addr())
    }

    /// Whether the invitation still lets someone in.
    pub fn invitation_open(&self) -> bool {
        self.shared.state.lock().unwrap().invitation_open
    }

    /// Replaces the invitation by a new one, valid until the session ends.
    /// The previous code stops working; connected guests are not affected.
    pub async fn invite(&self) -> Result<String> {
        let invitation = announce(&self.server, &self.endpoint.addr(), self.remaining()).await?;
        let code = invitation.code.clone();
        let (old_lookup, old_token) = {
            let mut state = self.shared.state.lock().unwrap();
            state.invitation_open = true;
            state.wrong_codes = 0;
            state.code = invitation.code;
            (
                std::mem::replace(&mut state.lookup, invitation.lookup),
                std::mem::replace(&mut state.owner_token, invitation.owner_token),
            )
        };
        control::delete(&self.server, &old_lookup, &old_token)
            .await
            .ok();
        Ok(code)
    }

    pub fn session_id(&self) -> &str {
        &self.shared.session_id
    }

    pub fn remaining(&self) -> Duration {
        self.shared
            .deadline
            .saturating_duration_since(Instant::now())
    }

    pub fn manifest(&self) -> Manifest {
        self.shared.manifest()
    }

    /// The state of each shared service when the session started.
    pub fn checks(&self) -> &[ServiceCheck] {
        &self.checks
    }

    /// The guests connected right now, in the order they joined.
    pub fn guests(&self) -> Vec<GuestStatus> {
        let state = self.shared.state.lock().unwrap();
        let mut guests: Vec<GuestStatus> = state
            .guests
            .iter()
            .map(|(id, guest)| GuestStatus {
                id: *id,
                device: guest.device.clone(),
                route: link::route(&guest.connection),
                connected: guest.joined.elapsed(),
            })
            .collect();
        guests.sort_by_key(|guest| guest.id);
        guests
    }

    /// The next thing that happened. After [`Activity::Ended`] the session is
    /// over and [`Share::stop`] should be called.
    pub async fn activity(&mut self) -> Option<Activity> {
        self.activity.recv().await
    }

    /// Disconnects one guest and keeps its device out for the rest of the
    /// session. The invitation stays open for everyone else.
    pub async fn revoke(&self, guest: u32) -> bool {
        let mut state = self.shared.state.lock().unwrap();
        let Some(Guest { end, key, .. }) = state.guests.get(&guest) else {
            return false;
        };
        let (end, key) = (end.clone(), *key);
        end.try_send(EndReason::Revoked).ok();
        state.banned.insert(key);
        true
    }

    /// Ends the session: guests are told, the invitation is withdrawn, the
    /// link is closed.
    pub async fn stop(self) {
        self.shared.end(EndReason::Stopped);
        let (lookup, owner_token) = {
            let state = self.shared.state.lock().unwrap();
            (state.lookup.clone(), state.owner_token.clone())
        };
        control::delete(&self.server, &lookup, &owner_token)
            .await
            .ok();
        tokio::time::sleep(FAREWELL).await;
        self.endpoint.close().await;
    }
}

impl Shared {
    fn manifest(&self) -> Manifest {
        Manifest {
            protocol: PROTOCOL_VERSION,
            manifest_version: MANIFEST_VERSION,
            session: SessionInfo {
                id: self.session_id.clone(),
                lifetime: self.lifetime.as_secs(),
                expires_in: self
                    .deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs(),
                max_guests: self.max_guests,
            },
            environments: self.selection.environments.clone(),
        }
    }

    fn end(&self, reason: EndReason) {
        let mut state = self.state.lock().unwrap();
        if state.ended.is_some() {
            return;
        }
        state.ended = Some(reason);
        for guest in state.guests.values() {
            guest.end.try_send(reason).ok();
        }
        self.activity.send(Activity::Ended { reason }).ok();
    }

    /// Whether this guest may try a code at all, before any work is done on
    /// one. Returns the code to try it against.
    fn may_try(&self, hello: &Hello, key: EndpointId) -> Result<String, RejectReason> {
        if hello.protocol != PROTOCOL_VERSION {
            return Err(RejectReason::UnsupportedProtocol);
        }
        let state = self.state.lock().unwrap();
        if state.ended.is_some() {
            return Err(RejectReason::SessionEnded);
        }
        if state.banned.contains(&key) {
            return Err(RejectReason::Revoked);
        }
        if !state.invitation_open {
            return Err(RejectReason::InvalidInvitation);
        }
        // The same device coming back takes its own place: it is not a new
        // guest, and must not find the session full because of itself.
        let returning = state.guests.values().any(|guest| guest.key == key);
        if !returning && state.guests.len() >= self.max_guests as usize {
            return Err(RejectReason::SessionFull);
        }
        Ok(state.code.clone())
    }

    /// A guest that did not prove the code. Enough of them close the
    /// invitation: guessing it one try at a time leads nowhere.
    fn wrong_code(&self) {
        let mut state = self.state.lock().unwrap();
        state.wrong_codes += 1;
        if state.wrong_codes == MAX_WRONG_CODES {
            state.invitation_open = false;
            self.activity
                .send(Activity::Locked {
                    attempts: state.wrong_codes,
                })
                .ok();
        }
    }

    /// Admits a guest that proved the code, unless the session changed in
    /// the meantime.
    fn admit(
        &self,
        hello: &Hello,
        key: EndpointId,
        connection: &Connection,
    ) -> Result<(u32, mpsc::Receiver<EndReason>), RejectReason> {
        let mut state = self.state.lock().unwrap();
        if state.ended.is_some() {
            return Err(RejectReason::SessionEnded);
        }
        if state.banned.contains(&key) {
            return Err(RejectReason::Revoked);
        }
        // A device that joins again, typically after its process crashed,
        // replaces its earlier connection rather than waiting for that one to
        // time out. If the earlier process is still alive, it is told.
        let earlier: Vec<u32> = state
            .guests
            .iter()
            .filter(|(_, guest)| guest.key == key)
            .map(|(id, _)| *id)
            .collect();
        for id in &earlier {
            if let Some(guest) = state.guests.remove(id) {
                guest.end.try_send(EndReason::Replaced).ok();
            }
        }
        if state.guests.len() >= self.max_guests as usize {
            return Err(RejectReason::SessionFull);
        }

        let id = state.next_id;
        state.next_id += 1;
        let (end_tx, end_rx) = mpsc::channel(1);
        state.guests.insert(
            id,
            Guest {
                device: hello.device.clone(),
                key,
                connection: connection.clone(),
                joined: Instant::now(),
                end: end_tx,
            },
        );
        Ok((id, end_rx))
    }
}

/// A fresh invitation on the control plane: a code drawn here, never sent,
/// and the key the control plane will know it by.
struct Announced {
    code: String,
    lookup: String,
    owner_token: String,
    ttl: u64,
}

async fn announce(server: &str, host: &EndpointAddr, ttl: Duration) -> Result<Announced> {
    // Another invitation with the same lookup key is a one-in-a-trillion
    // event; drawing again settles it.
    for _ in 0..3 {
        let code = code::from_random(rand::random());
        let lookup = invite::lookup_key(&code).await?;
        if let Some(created) = control::create(server, &lookup, host, ttl).await? {
            return Ok(Announced {
                code,
                lookup,
                owner_token: created.owner_token,
                ttl: created.ttl,
            });
        }
    }
    Err(anyhow!("the control plane refused every new invitation"))
}

async fn accept_guests(endpoint: Endpoint, shared: Arc<Shared>) {
    while let Some(incoming) = endpoint.accept().await {
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_guest(incoming, shared).await {
                tracing::debug!("guest connection ended: {error:#}");
            }
        });
    }
}

async fn serve_guest(incoming: Incoming, shared: Arc<Shared>) -> Result<()> {
    let connection = incoming.await.map_err(|error| anyhow!("{error}"))?;

    let key = connection.remote_id();
    let (mut control, mut inbound, hello) = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let (send, mut recv) = connection.accept_bi().await?;
        let hello: Hello = frame::read(&mut recv, MAX_FRAME).await?;
        anyhow::Ok((send, recv, hello))
    })
    .await
    .map_err(|_| anyhow!("no hello from the guest"))??;
    // What the guest says of itself is shown to the host: made safe to show.
    let hello = Hello {
        device: hello.device.cleaned(),
        ..hello
    };

    // Who may try at all, then the code exchange: the guest proves it holds
    // the code without sending it, and only then does the host prove it too.
    let admitted = async {
        let code = shared.may_try(&hello, key)?;
        let (pake, challenge) =
            Pake::host(&code, &shared.host_id).ok_or(RejectReason::InvalidInvitation)?;
        frame::write(&mut control, &HelloReply::Challenge { pake: challenge })
            .await
            .map_err(|_| RejectReason::InvalidInvitation)?;
        let confirm: Confirm =
            tokio::time::timeout(HANDSHAKE_TIMEOUT, frame::read(&mut inbound, MAX_FRAME))
                .await
                .map_err(|_| RejectReason::InvalidInvitation)?
                .map_err(|_| RejectReason::InvalidInvitation)?;
        let proven = pake
            .finish(&hello.pake)
            .filter(|shared_key| shared_key.verify(GUEST_PROOF, &confirm.mac));
        let Some(shared_key) = proven else {
            shared.wrong_code();
            return Err(RejectReason::InvalidInvitation);
        };
        let (id, end) = shared.admit(&hello, key, &connection)?;
        Ok((id, end, shared_key))
    }
    .await;

    let (id, mut end, shared_key) = match admitted {
        Ok(admitted) => admitted,
        Err(reason) => {
            shared.activity.send(Activity::Refused { reason }).ok();
            frame::write(&mut control, &HelloReply::Reject { reason })
                .await
                .ok();
            control.finish().ok();
            tokio::time::timeout(FAREWELL, connection.closed())
                .await
                .ok();
            connection.close(1u32.into(), b"rejected");
            return Ok(());
        }
    };

    let served = async {
        let welcome = HelloReply::Welcome {
            guest_id: id,
            manifest: shared.manifest(),
            mac: shared_key.prove(HOST_PROOF),
        };
        frame::write(&mut control, &welcome).await?;
        shared
            .activity
            .send(Activity::GuestJoined {
                id,
                device: hello.device.clone(),
            })
            .ok();

        loop {
            tokio::select! {
                reason = end.recv() => {
                    let reason = reason.unwrap_or(EndReason::Stopped);
                    frame::write(&mut control, &HostEvent::Ended { reason }).await.ok();
                    control.finish().ok();
                    tokio::time::timeout(FAREWELL, connection.closed()).await.ok();
                    connection.close(0u32.into(), b"ended");
                    break;
                }
                stream = connection.accept_bi() => {
                    let (send, recv) = stream?;
                    tokio::spawn(serve_stream(shared.clone(), id, send, recv));
                }
            }
        }
        anyhow::Ok(())
    }
    .await;

    shared.state.lock().unwrap().guests.remove(&id);
    shared.activity.send(Activity::GuestLeft { id }).ok();
    served
}

/// One stream is one TCP connection to one service of the session.
async fn serve_stream(shared: Arc<Shared>, guest: u32, mut send: SendStream, mut recv: RecvStream) {
    let Ok(Ok(open)) =
        tokio::time::timeout(HANDSHAKE_TIMEOUT, frame::read::<_, Open>(&mut recv, 4096)).await
    else {
        return;
    };
    let host = normalize_host(&open.host);

    let Some(target) = shared.selection.routes.get(&(host.clone(), open.port)) else {
        shared
            .activity
            .send(Activity::Denied {
                guest,
                host,
                port: open.port,
            })
            .ok();
        frame::write(&mut send, &OpenReply::Denied).await.ok();
        send.finish().ok();
        return;
    };

    let service = match tokio::time::timeout(DIAL_TIMEOUT, TcpStream::connect(target)).await {
        Ok(Ok(service)) => service,
        _ => {
            shared
                .activity
                .send(Activity::Unreachable {
                    guest,
                    host,
                    port: open.port,
                })
                .ok();
            frame::write(&mut send, &OpenReply::Unreachable).await.ok();
            send.finish().ok();
            return;
        }
    };
    service.set_nodelay(true).ok();

    if frame::write(&mut send, &OpenReply::Ok).await.is_ok() {
        link::pipe(service, send, recv).await.ok();
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
