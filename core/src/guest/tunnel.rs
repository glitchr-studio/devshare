use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use devshare_protocol::Manifest;
use futures::{SinkExt, StreamExt};
use netstack_smoltcp::{StackBuilder, TcpListener, TcpStream, UdpSocket};
use tokio::{io::AsyncWriteExt, task::JoinSet};
#[cfg(not(any(target_os = "ios", target_os = "android")))]
use tun::AbstractDevice;

use super::{
    dns,
    tls::{self, Bridged},
    AddressPlan, NamePolicy, OpenError, Opener, Termination,
};
#[cfg(not(any(target_os = "ios", target_os = "android")))]
use super::{helper::Helper, system::SystemDns};
use crate::link;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
use std::os::fd::IntoRawFd;

const MTU: u16 = 1500;
const DNS_PORT: u16 = 53;
/// Bytes buffered per direction for each connection, four times over in the
/// stack. The interface is local, so a small window costs no speed, while
/// the stack's default (320 KiB) costs 1.3 MiB per open connection: too
/// much for a phone, where the whole tunnel must fit in about 50 MiB.
const SOCKET_BUFFER: u32 = 64 * 1024;
const HANDSHAKE: Duration = Duration::from_millis(50);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// The session as the rest of the device sees it. Dropping it removes the
/// interface, its routes and the name resolution it installed.
pub struct Tunnel {
    plan: Arc<AddressPlan>,
    interface: Option<String>,
    /// Everything that runs the interface. Aborted with the tunnel, which
    /// closes the interface.
    _tasks: JoinSet<()>,
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    _system: Option<SystemDns>,
    /// The connection to the privileged helper when it made the interface:
    /// the names stay installed as long as it is open.
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    _helper: Option<Helper>,
}

impl Tunnel {
    /// Desktop: creates the interface and points the system's resolver at it
    /// for the shared names. Needs the privilege to create an interface.
    ///
    /// With a `termination`, TLS connections to the names it covers are
    /// terminated on this device (see [`Termination`]); without one, they
    /// pass through and programs see the services' own certificates.
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    pub async fn start(
        opener: Opener,
        manifest: &Manifest,
        names: &NamePolicy,
        termination: Option<Termination>,
    ) -> Result<Self> {
        refuse_unexpected_names(manifest, names)?;
        let plan = Arc::new(AddressPlan::new(manifest)?);
        let termination = termination.map(Arc::new);
        let root = nix::unistd::geteuid().is_root();

        // Without administrator rights, the helper makes the interface and
        // hands it over; this process does the rest.
        if !root {
            if let Some(mut helper) = Helper::connect()? {
                let (descriptor, interface) = helper.up(&plan)?;
                let mut config = tun::Configuration::default();
                config.raw_fd(descriptor.into_raw_fd()).mtu(MTU);
                let device = tun::create_as_async(&config)
                    .context("attaching to the interface the helper made")?;
                let tasks = serve(device, opener, plan.clone(), termination)?;
                return Ok(Self {
                    plan,
                    interface: Some(interface),
                    _tasks: tasks,
                    _system: None,
                    _helper: Some(helper),
                });
            }
        }

        let mut config = tun::Configuration::default();
        config
            .address(AddressPlan::local())
            .netmask(AddressPlan::netmask())
            .mtu(MTU)
            .up();
        // utun interfaces are point-to-point: the peer address is what the
        // route to the session's addresses goes through.
        #[cfg(target_os = "macos")]
        config.destination(AddressPlan::resolver());
        #[cfg(target_os = "linux")]
        config.platform_config(|platform| {
            platform.ensure_root_privileges(true);
        });

        let device = match tun::create_as_async(&config) {
            Ok(device) => device,
            Err(error) if !root => return Err(super::helper::without_rights(error.into())),
            Err(error) => return Err(error).context("creating the network interface"),
        };
        let interface = device.tun_name().context("reading the interface name")?;

        let tasks = serve(device, opener, plan.clone(), termination)?;
        let system = SystemDns::install(&interface, &plan)?;

        Ok(Self {
            plan,
            interface: Some(interface),
            _tasks: tasks,
            _system: Some(system),
            _helper: None,
        })
    }

    /// Whether this process will be able to make the interface: as root, or
    /// through a helper that serves this user. Checked before joining, so
    /// that a guest who cannot use a session never appears to its host nor
    /// takes one of its places.
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    pub fn check_rights() -> Result<()> {
        if nix::unistd::geteuid().is_root() || Helper::connect()?.is_some() {
            return Ok(());
        }
        Err(super::helper::without_rights(anyhow::anyhow!(
            "not running as root, and DevShare's helper is not installed"
        )))
    }

    /// Mobile: the system creates the interface (a packet tunnel on iOS, a
    /// `VpnService` on Android) and hands over its file descriptor. The app
    /// configures it from [`Tunnel::plan`]: the address, the route to the
    /// session's network, and the resolver for the shared names only.
    ///
    /// The descriptor stays the system's: it is not closed with the tunnel.
    #[cfg(unix)]
    pub fn attach(
        opener: Opener,
        manifest: &Manifest,
        names: &NamePolicy,
        descriptor: std::os::fd::RawFd,
        termination: Option<Termination>,
    ) -> Result<Self> {
        refuse_unexpected_names(manifest, names)?;
        let plan = Arc::new(AddressPlan::new(manifest)?);

        let mut config = tun::Configuration::default();
        config.raw_fd(descriptor).close_fd_on_drop(false).mtu(MTU);
        let device =
            tun::create_as_async(&config).context("attaching to the system's interface")?;

        let tasks = serve(device, opener, plan.clone(), termination.map(Arc::new))?;
        Ok(Self {
            plan,
            interface: None,
            _tasks: tasks,
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            _system: None,
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            _helper: None,
        })
    }

    pub fn plan(&self) -> &AddressPlan {
        &self.plan
    }

    /// The interface's name, when this process created it.
    pub fn interface(&self) -> Option<&str> {
        self.interface.as_deref()
    }
}

/// A session that names what could be a real site is not joined: the guest
/// would send that site's traffic to the host. See [`NamePolicy`].
fn refuse_unexpected_names(manifest: &Manifest, names: &NamePolicy) -> Result<()> {
    let refused = names.refused(manifest);
    if refused.is_empty() {
        return Ok(());
    }
    let shown: Vec<String> = refused
        .iter()
        .map(|name| crate::protocol::clean(name, 80))
        .collect();
    anyhow::bail!(
        "not joined: the session names {}, which could be a real site, and the guest would \
         send its traffic for it to the host. Sessions are expected to use names under {}. \
         To join anyway: devshare join --trust-names",
        shown.join(", "),
        crate::guest::DEV_DOMAINS.join(", ")
    )
}

/// Runs a TCP/IP stack in this process on the packets of the interface:
/// every connection a program of the device opens to an address of the
/// session ends here, as a stream.
fn serve(
    device: tun::AsyncDevice,
    opener: Opener,
    plan: Arc<AddressPlan>,
    termination: Option<Arc<Termination>>,
) -> Result<JoinSet<()>> {
    let (stack, runner, udp, tcp) = StackBuilder::default()
        .enable_tcp(true)
        .enable_udp(true)
        .tcp_recv_buffer_size(SOCKET_BUFFER)
        .tcp_send_buffer_size(SOCKET_BUFFER)
        .mtu(MTU as usize)
        .build()
        .context("starting the network stack")?;
    let (udp, tcp) = (
        udp.context("the stack has no UDP")?,
        tcp.context("the stack has no TCP")?,
    );

    let mut tasks = JoinSet::new();
    if let Some(runner) = runner {
        tasks.spawn(async move {
            if let Err(error) = runner.await {
                tracing::warn!("the network stack stopped: {error}");
            }
        });
    }

    let (mut to_interface, mut from_interface) = device.into_framed().split();
    let (mut to_stack, mut from_stack) = stack.split();
    tasks.spawn(async move {
        while let Some(Ok(packet)) = from_stack.next().await {
            if let Err(error) = to_interface.send(packet).await {
                tracing::debug!("packet not written to the interface: {error}");
            }
        }
    });
    tasks.spawn(async move {
        while let Some(packet) = from_interface.next().await {
            match packet {
                Ok(packet) => {
                    to_stack.send(packet).await.ok();
                }
                Err(error) => {
                    tracing::warn!("the network interface stopped: {error}");
                    return;
                }
            }
        }
    });

    tasks.spawn(connections(tcp, opener, plan.clone(), termination));
    tasks.spawn(resolver(udp, plan));
    Ok(tasks)
}

async fn connections(
    mut listener: TcpListener,
    opener: Opener,
    plan: Arc<AddressPlan>,
    termination: Option<Arc<Termination>>,
) {
    // Counted so that a connection which never ends shows in the log.
    let open = Arc::new(AtomicUsize::new(0));
    let said = Arc::new(Mutex::new(HashSet::new()));
    while let Some((stream, _source, destination)) = listener.next().await {
        let SocketAddr::V4(destination) = destination else {
            continue;
        };
        // Refused here when the manifest does not list the port. The host
        // checks again: it never trusts the guest's own filter.
        let shared = plan
            .name_of(*destination.ip())
            .filter(|name| plan.shares(name, destination.port()));
        let Some(name) = shared.map(str::to_string) else {
            tokio::spawn(refuse(stream));
            continue;
        };

        let (opener, open, port) = (opener.clone(), open.clone(), destination.port());
        let said = said.clone();
        // Terminated here when the service speaks TLS and this device's
        // certificates cover its name.
        let terminated = plan
            .pin(&name, port)
            .map(str::to_string)
            .zip(termination.clone().filter(|tls| tls.covers(&name)));
        tokio::spawn(async move {
            let count = open.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::debug!("{name}:{port} opened, {count} open");
            // A browser can only show a closed connection: the reason is
            // said here, once for each service.
            let say_once = |reason: String| {
                if said.lock().unwrap().insert((name.clone(), port)) {
                    tracing::warn!("{name}:{port}: {reason}");
                }
            };
            if let Some((pin, termination)) = terminated {
                let opened = async {
                    let (send, recv) = opener.open(&name, port).await?;
                    Ok::<_, OpenError>(tokio::io::join(recv, send))
                };
                match tls::bridge(stream, &name, &pin, &termination, opened).await {
                    Ok(()) => {}
                    Err(Bridged::Unreachable(OpenError::Failed(error))) => {
                        tracing::debug!("{name}:{port}: {error:#}")
                    }
                    Err(Bridged::Unreachable(error)) => say_once(error.to_string()),
                    Err(Bridged::Changed) => say_once(
                        "refused: its certificate is not the one the host saw when it shared it"
                            .to_string(),
                    ),
                    Err(Bridged::Failed(error)) => tracing::debug!("{name}:{port}: {error:#}"),
                }
            } else {
                match opener.open(&name, port).await {
                    Ok((send, recv)) => {
                        link::pipe(stream, send, recv).await.ok();
                    }
                    Err(OpenError::Failed(error)) => {
                        tracing::debug!("{name}:{port}: {error:#}");
                        refuse(stream).await;
                    }
                    Err(error) => {
                        say_once(error.to_string());
                        refuse(stream).await;
                    }
                }
            }
            let count = open.fetch_sub(1, Ordering::Relaxed) - 1;
            tracing::debug!("{name}:{port} closed, {count} open");
        });
    }
}

/// Closes a connection nothing will answer, so that the program which opened
/// it fails at once. Merely dropping it would not do: the program would send
/// its opening packet again and wait for its own timeout.
async fn refuse(mut stream: TcpStream) {
    // The stack hands a connection over before its handshake is complete,
    // and loses a close requested that early.
    tokio::time::sleep(HANDSHAKE).await;
    tokio::time::timeout(CLOSE_TIMEOUT, stream.shutdown())
        .await
        .ok();
}

/// Answers the queries sent to the session's resolver, and only those.
async fn resolver(socket: UdpSocket, plan: Arc<AddressPlan>) {
    let (mut queries, mut answers) = socket.split();
    while let Some((query, source, destination)) = queries.next().await {
        let SocketAddr::V4(asked) = destination else {
            continue;
        };
        if *asked.ip() != AddressPlan::resolver() || asked.port() != DNS_PORT {
            continue;
        }
        if let Some(answer) = dns::answer(&query, &plan) {
            answers.send((answer, destination, source)).await.ok();
        }
    }
}
