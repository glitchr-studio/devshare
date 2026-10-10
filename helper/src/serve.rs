//! The helper at work: one connection per guest process, one session per
//! user, every request checked before anything touches the system.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{IoSlice, Write},
    net::Ipv4Addr,
    os::{
        fd::AsRawFd,
        unix::{fs::PermissionsExt, net::UnixListener, net::UnixStream},
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{bail, Context, Result};
use devshare_protocol::{
    helper::{
        self, Outcome, Reply, Request, Up, HELPER_PROTOCOL, MAX_MESSAGE, MAX_NAMES, MTU,
        TEST_NETWORK,
    },
    names::NamePolicy,
    system_dns::{self, Installed},
};
use nix::sys::socket::{sendmsg, ControlMessage, MsgFlags};

use crate::trust;
use tun::AbstractDevice;

pub struct Options {
    pub socket: PathBuf,
    pub users: PathBuf,
    pub trusted_domains: PathBuf,
    pub store: trust::Store,
}

/// The interface and the names of one guest's session, held until the
/// guest says `down` or goes away.
struct Session {
    device: tun::Device,
    installed: Installed,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Err(error) = system_dns::remove(&self.installed) {
            tracing::error!("could not remove a session's names: {error}");
        }
        // The interface itself goes when the last descriptor to it closes:
        // ours now, the guest's when its process ends.
        tracing::info!(
            "session down ({})",
            self.device.tun_name().unwrap_or_default()
        );
    }
}

/// The users with a session up right now.
type Active = Arc<Mutex<HashSet<u32>>>;

pub fn run(options: Options) -> Result<()> {
    crate::must_be_root("serving guests")?;
    // Whatever a previous helper or a killed guest left behind.
    if let Err(error) = system_dns::remove_leftovers().and_then(|()| system_dns::set_local(&[])) {
        tracing::warn!("could not clean up what was left behind: {error}");
    }

    if let Some(directory) = options.socket.parent() {
        fs::create_dir_all(directory)
            .with_context(|| format!("creating {}", directory.display()))?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o755))?;
    }
    fs::remove_file(&options.socket).ok();
    let listener = UnixListener::bind(&options.socket)
        .with_context(|| format!("listening on {}", options.socket.display()))?;
    // Anyone may connect; who is served is decided per connection, by uid.
    fs::set_permissions(&options.socket, fs::Permissions::from_mode(0o666))?;
    tracing::info!("listening on {}", options.socket.display());

    let options = Arc::new(options);
    let active: Active = Arc::default();
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!("a connection failed: {error}");
                continue;
            }
        };
        let (options, active) = (options.clone(), active.clone());
        thread::spawn(move || serve(stream, &options, &active));
    }
    Ok(())
}

fn serve(mut stream: UnixStream, options: &Options, active: &Active) {
    // Before reading a byte: whose process this is.
    let uid = match peer_uid(&stream) {
        Ok(uid) => uid,
        Err(error) => {
            tracing::warn!("a connection with no identity: {error}");
            return;
        }
    };
    if !allowed(uid, &options.users) {
        tracing::warn!(
            "refused a connection from uid {uid}: not in {}",
            options.users.display()
        );
        return;
    }

    let mut session: Option<Session> = None;
    // Whether this connection pointed this machine's own names at itself.
    // This connection among the others that name projects.
    let connection = NEXT_CONNECTION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut local = false;
    // Until the guest is gone, however it went.
    while let Ok(request) = helper::read::<Request>(&mut stream, MAX_MESSAGE) {
        let outcome = match request {
            Request::Hello { protocol } => {
                let reply = if protocol == HELPER_PROTOCOL {
                    Reply::Welcome {
                        version: env!("CARGO_PKG_VERSION").to_string(),
                        protocol: HELPER_PROTOCOL,
                    }
                } else {
                    Reply::Outcome(Outcome::Error(format!(
                        "this helper speaks protocol {HELPER_PROTOCOL}, the guest {protocol}"
                    )))
                };
                send(&mut stream, &reply, None)
            }
            Request::Up(up) => match bring_up(uid, up, &mut session, options, active) {
                Ok((interface, descriptor)) => send(
                    &mut stream,
                    &Reply::Outcome(Outcome::Up { interface }),
                    Some(descriptor),
                ),
                Err(error) => {
                    tracing::warn!("refused an up from uid {uid}: {error:#}");
                    send(
                        &mut stream,
                        &Reply::Outcome(Outcome::Error(format!("{error:#}"))),
                        None,
                    )
                }
            },
            Request::Down {} => {
                take_down(uid, &mut session, active);
                send(&mut stream, &Reply::Outcome(Outcome::Down {}), None)
            }
            Request::TrustCa { certificate } => {
                let domains = trusted(&options.trusted_domains);
                let outcome = match trust::trust(&options.store, uid, &certificate, domains) {
                    Ok(sha256) => Outcome::Trusted { sha256 },
                    Err(error) => {
                        tracing::warn!("refused to trust an authority for uid {uid}: {error:#}");
                        Outcome::Error(format!("{error:#}"))
                    }
                };
                send(&mut stream, &Reply::Outcome(outcome), None)
            }
            Request::Local { names } => {
                let outcome = match set_local(connection, &names, &options.trusted_domains) {
                    Ok(()) => {
                        local = !names.is_empty();
                        Outcome::Local {}
                    }
                    Err(error) => {
                        tracing::warn!("refused names for uid {uid}: {error:#}");
                        Outcome::Error(format!("{error:#}"))
                    }
                };
                send(&mut stream, &Reply::Outcome(outcome), None)
            }
            Request::UntrustCa { sha256 } => {
                let outcome = match trust::untrust(&options.store, uid, &sha256) {
                    Ok(()) => Outcome::Untrusted {},
                    Err(error) => Outcome::Error(format!("{error:#}")),
                };
                send(&mut stream, &Reply::Outcome(outcome), None)
            }
        };
        if outcome.is_err() {
            break;
        }
    }
    take_down(uid, &mut session, active);
    if local {
        if let Err(error) = write_local(connection, None) {
            tracing::error!("could not remove this machine's names: {error}");
        }
    }
}

static NEXT_CONNECTION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The names each connection asked for. What is written is all of them
/// together: one program leaving takes its own names away, not another's.
static LOCAL_NAMES: std::sync::Mutex<Vec<(u64, Vec<String>)>> = std::sync::Mutex::new(Vec::new());

/// Sets (or, with `None`, forgets) a connection's names and writes the
/// names of all connections.
fn write_local(connection: u64, names: Option<&[String]>) -> Result<()> {
    let mut all = LOCAL_NAMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    all.retain(|(known, _)| *known != connection);
    if let Some(names) = names.filter(|names| !names.is_empty()) {
        all.push((connection, names.to_vec()));
    }
    let mut together: Vec<String> = all.iter().flat_map(|(_, names)| names.clone()).collect();
    together.sort();
    together.dedup();
    system_dns::set_local(&together).context("pointing the names at this machine")?;
    tracing::info!("this machine's names: {}", together.join(", "));
    Ok(())
}

/// Points this machine's own project names at itself, after the same check
/// as a session's names: development names only, and not too many.
fn set_local(connection: u64, names: &[String], trusted_domains: &Path) -> Result<()> {
    if names.len() > MAX_NAMES {
        bail!("at most {MAX_NAMES} names, not {}", names.len());
    }
    let policy = NamePolicy {
        domains: trusted(trusted_domains),
        trust_all: false,
    };
    if let Some(name) = names
        .iter()
        .find(|name| !policy.accepts(name) || name.as_str() == "localhost")
    {
        bail!(
            "\"{}\" is not a name of this machine's projects: not a hostname, or not under a test domain",
            devshare_protocol::clean(name, 80)
        );
    }
    write_local(connection, Some(names))
}

/// Creates the interface and installs the names, after checking everything
/// about the request. Returns the interface's name and descriptor.
fn bring_up(
    uid: u32,
    up: Up,
    session: &mut Option<Session>,
    options: &Options,
    active: &Active,
) -> Result<(String, i32)> {
    if session.is_some() {
        bail!("a session is already up on this connection");
    }
    check(&up, &options.trusted_domains)?;
    if !active.lock().unwrap().insert(uid) {
        bail!("a session is already up for this user; one at a time");
    }
    let brought = (|| {
        let mut config = tun::Configuration::default();
        config
            .address(up.address)
            .netmask(Ipv4Addr::from(helper::mask(up.prefix)))
            .mtu(MTU)
            .up();
        // utun interfaces are point-to-point: the peer address is what the
        // route to the session's network goes through.
        #[cfg(target_os = "macos")]
        config.destination(up.resolver);
        #[cfg(target_os = "linux")]
        config.platform_config(|platform| {
            platform.ensure_root_privileges(true);
        });
        let device = tun::create(&config).context("creating the interface")?;
        let interface = device.tun_name().context("reading the interface's name")?;

        let names: Vec<(String, Ipv4Addr)> = up
            .names
            .iter()
            .map(|name| (name.name.clone(), name.address))
            .collect();
        let installed = system_dns::install(&interface, &names, up.resolver)
            .context("installing the session's names")?;
        let shown: Vec<&str> = names
            .iter()
            .take(3)
            .map(|(name, _)| name.as_str())
            .collect();
        tracing::info!(
            "session up for uid {uid} on {interface}: {}{}",
            shown.join(", "),
            match names.len() {
                0..=3 => String::new(),
                more => format!(" and {} more", more - 3),
            }
        );
        let descriptor = device.as_raw_fd();
        *session = Some(Session { device, installed });
        anyhow::Ok((interface, descriptor))
    })();
    if brought.is_err() {
        active.lock().unwrap().remove(&uid);
    }
    brought
}

fn take_down(uid: u32, session: &mut Option<Session>, active: &Active) {
    if session.take().is_some() {
        active.lock().unwrap().remove(&uid);
    }
}

/// What a request must satisfy, whoever sends it.
fn check(up: &Up, trusted_domains: &Path) -> Result<()> {
    if up.names.is_empty() || up.names.len() > MAX_NAMES {
        bail!(
            "a session names between 1 and {MAX_NAMES} hosts, not {}",
            up.names.len()
        );
    }
    let policy = NamePolicy {
        domains: trusted(trusted_domains),
        trust_all: false,
    };
    if let Some(name) = up.names.iter().find(|name| !policy.accepts(&name.name)) {
        bail!(
            "\"{}\" is not a name a session may use: not a hostname, or not under a test domain",
            devshare_protocol::clean(&name.name, 80)
        );
    }
    if !(16..=30).contains(&up.prefix) {
        bail!("a prefix of {} is not a session's network", up.prefix);
    }
    let (block, block_prefix) = TEST_NETWORK;
    let inside = |address: Ipv4Addr| {
        helper::contains(block, block_prefix, address)
            && helper::contains(up.address, up.prefix, address)
    };
    let addresses = [up.address, up.resolver]
        .into_iter()
        .chain(up.names.iter().map(|name| name.address));
    let mut seen = HashMap::new();
    for address in addresses {
        if !inside(address) {
            bail!("{address} is outside the session's network in {block}/{block_prefix}");
        }
        if seen.insert(address, ()).is_some() {
            bail!("{address} is used twice");
        }
    }
    Ok(())
}

/// The domains root trusts besides the test domains.
fn trusted(file: &Path) -> Vec<String> {
    fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .map(|line| line.trim().trim_matches('.').to_ascii_lowercase())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// Root, and the users recorded at installation.
fn allowed(uid: u32, users: &Path) -> bool {
    uid == 0
        || fs::read_to_string(users)
            .unwrap_or_default()
            .lines()
            .any(|line| line.trim().parse::<u32>() == Ok(uid))
}

#[cfg(target_os = "macos")]
fn peer_uid(stream: &UnixStream) -> Result<u32> {
    let (uid, _) = nix::unistd::getpeereid(stream)?;
    Ok(uid.as_raw())
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> Result<u32> {
    let credentials =
        nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)?;
    Ok(credentials.uid())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn peer_uid(_stream: &UnixStream) -> Result<u32> {
    bail!("this system is not supported")
}

/// One reply, and the descriptor that goes with it if any.
fn send(stream: &mut UnixStream, reply: &Reply, descriptor: Option<i32>) -> Result<()> {
    let frame = helper::encode(reply);
    match descriptor {
        None => stream.write_all(&frame)?,
        Some(descriptor) => {
            let descriptors = [descriptor];
            sendmsg::<()>(
                stream.as_raw_fd(),
                &[IoSlice::new(&frame)],
                &[ControlMessage::ScmRights(&descriptors)],
                MsgFlags::empty(),
                None,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use devshare_protocol::helper::Name;

    use super::*;

    fn up(names: &[(&str, [u8; 4])]) -> Up {
        Up {
            names: names
                .iter()
                .map(|(name, address)| Name {
                    name: name.to_string(),
                    address: Ipv4Addr::from(*address),
                })
                .collect(),
            address: Ipv4Addr::new(198, 18, 90, 1),
            resolver: Ipv4Addr::new(198, 18, 90, 2),
            prefix: 24,
        }
    }

    fn file(tag: &str, lines: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("ds-{tag}-{}", std::process::id()));
        fs::write(&path, lines).unwrap();
        path
    }

    #[test]
    fn only_test_names_inside_the_session_network_pass() {
        let none = file("trusted-none", "");
        assert!(check(&up(&[("shop.test", [198, 18, 90, 10])]), &none).is_ok());
        for (why, request) in [
            (
                "a real site",
                up(&[("accounts.google.com", [198, 18, 90, 10])]),
            ),
            ("not a hostname", up(&[("../etc/x", [198, 18, 90, 10])])),
            (
                "an address outside the block",
                up(&[("shop.test", [10, 0, 0, 10])]),
            ),
            (
                "outside the session's network",
                up(&[("shop.test", [198, 18, 91, 10])]),
            ),
            (
                "the resolver's address",
                up(&[("shop.test", [198, 18, 90, 2])]),
            ),
            ("no name at all", up(&[])),
        ] {
            assert!(check(&request, &none).is_err(), "{why}");
        }
        let mut too_many = up(&[]);
        too_many.names = (0..=MAX_NAMES as u8)
            .map(|n| Name {
                name: format!("h{n}.test"),
                address: Ipv4Addr::new(198, 18, 90, n.wrapping_add(10)),
            })
            .collect();
        assert!(check(&too_many, &none).is_err());
        let mut wide = up(&[("shop.test", [198, 18, 90, 10])]);
        wide.prefix = 8;
        assert!(check(&wide, &none).is_err());

        // Root may trust more domains; a comment is not one.
        let lan = file("trusted-lan", "# ours\nlan\n");
        assert!(check(&up(&[("shop.lan", [198, 18, 90, 10])]), &none).is_err());
        assert!(check(&up(&[("shop.lan", [198, 18, 90, 10])]), &lan).is_ok());
        assert!(check(&up(&[("shop.com", [198, 18, 90, 10])]), &lan).is_err());
    }

    #[test]
    fn root_and_the_recorded_users_are_served() {
        let users = file("users", "501\n 1000 \n");
        assert!(allowed(0, &users));
        assert!(allowed(501, &users));
        assert!(allowed(1000, &users));
        assert!(!allowed(502, &users));
        assert!(!allowed(501, Path::new("/nonexistent/users")));
    }

    #[test]
    fn a_session_at_a_time_for_a_user() {
        let active: Active = Arc::default();
        let options = Options {
            socket: PathBuf::new(),
            users: PathBuf::new(),
            trusted_domains: file("trusted-empty", ""),
            store: trust::Store::default(),
        };
        // Creating an interface needs root: what is checked here is the
        // bookkeeping around it, by making the second attempt fail before.
        active.lock().unwrap().insert(7);
        let mut session = None;
        let refused = bring_up(
            7,
            up(&[("shop.test", [198, 18, 90, 10])]),
            &mut session,
            &options,
            &active,
        )
        .unwrap_err()
        .to_string();
        assert!(refused.contains("one at a time"), "{refused}");
        take_down(7, &mut session, &active);
    }
}
