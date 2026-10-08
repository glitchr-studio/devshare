use std::{
    collections::HashMap,
    io::IsTerminal,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use devshare_core::{
    ca::DeviceCa,
    discover,
    environment::{self, Config, Settings},
    guest::{self, GuestLink, Termination, Tunnel},
    host::{Activity, Share, ShareOptions},
    invite::DeviceSecret,
    link::Route,
    probe::Probe,
    protocol::{clean, code, Service},
    qr,
};
use tokio::io::{AsyncBufReadExt, BufReader};

/// How often the CLI looks whether a link went from relayed to direct.
const ROUTE_CHECK: Duration = Duration::from_secs(2);

#[derive(Parser)]
#[command(
    name = "devshare",
    version,
    about = "Share local development environments for a few minutes"
)]
struct Cli {
    /// URL of the control plane. Defaults to the one the project names,
    /// else the one of the general settings, else http://localhost:8787.
    #[arg(long, global = true, env = "DEVSHARE_SERVER")]
    server: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Share environments in one ephemeral session.
    Share {
        /// The folders of the projects to share. The current folder when
        /// none is given. A folder without a devshare.toml gets one from
        /// its compose file.
        folders: Vec<PathBuf>,
        /// Share only this environment of the projects. May be repeated.
        #[arg(long, value_name = "ENVIRONMENT")]
        only: Vec<String>,
        /// How long the session lasts: 90s, 5m, 1h.
        #[arg(long, value_parser = environment::duration)]
        duration: Option<Duration>,
        /// Maximum number of guests connected at once.
        #[arg(long)]
        guests: Option<u32>,
        /// A file declaring the environments, instead of project folders.
        #[arg(long, env = "DEVSHARE_CONFIG")]
        config: Option<PathBuf>,
        /// Do not draw the QR code.
        #[arg(long)]
        no_qr: bool,
    },
    /// Read the Docker Compose file of a folder and write its devshare.toml.
    Discover {
        /// The project's folder. This one when none is given.
        directory: Option<PathBuf>,
        /// The hostname guests will use. <project>.test when none is given.
        #[arg(long)]
        host: Option<String>,
        /// Show what would be written, and write nothing.
        #[arg(long)]
        print: bool,
        /// Replace a devshare.toml that was written by hand.
        #[arg(long)]
        force: bool,
    },
    /// Show the general settings, common to all projects.
    Settings {
        /// Write the settings file, every setting commented out, if there
        /// is none yet.
        #[arg(long)]
        init: bool,
    },
    /// Join a session with its code or link.
    Join {
        invitation: String,
        /// Accept a session that names hosts outside the test domains
        /// (.test, .localhost, …): your traffic for those names will go to
        /// the host for the whole session.
        #[arg(long)]
        trust_names: bool,
    },
    /// List the declared environments.
    Environments {
        #[arg(long, env = "DEVSHARE_CONFIG")]
        config: Option<PathBuf>,
    },
    /// This device's own certificate authority: with it, https:// to the
    /// services of a session opens without a warning, in every program.
    Ca {
        #[command(subcommand)]
        action: Option<CaAction>,
    },
}

#[derive(Subcommand)]
enum CaAction {
    /// Show it and whether this computer trusts it. What `devshare ca` does.
    Status,
    /// Make it if there is none, and have this computer trust it (through
    /// DevShare's helper).
    Install,
    /// Replace it with a new one, trusted in its place.
    Renew,
    /// Stop trusting it and delete it.
    Remove,
}

#[tokio::main]
async fn main() {
    // The libraries underneath report every hiccup of the network; only
    // DevShare's own warnings are worth a line unless RUST_LOG asks for more.
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| {
                "off,devshare=warn,devshare_core=warn,devshare_server=info".into()
            }),
        )
        .with_writer(std::io::stderr)
        // Read by a person at a terminal: the message, not when or where.
        .without_time()
        .with_target(false)
        .init();

    let cli = Cli::parse();
    // The commands that only print die quietly when their reader goes away
    // (`devshare settings | head -1`). Not the ones that hold a session:
    // there a closed connection must be an error to handle, not a signal.
    #[cfg(unix)]
    if matches!(
        cli.command,
        Command::Discover { .. }
            | Command::Settings { .. }
            | Command::Environments { .. }
            | Command::Ca { .. }
    ) {
        // SAFETY: called once, before any thread or output exists.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    }
    let outcome = match cli.command {
        Command::Share {
            folders,
            only,
            duration,
            guests,
            config,
            no_qr,
        } => share(cli.server, folders, only, duration, guests, config, no_qr).await,
        Command::Discover {
            directory,
            host,
            print,
            force,
        } => discover(directory, host, print, force),
        Command::Settings { init } => settings(init),
        Command::Join {
            invitation,
            trust_names,
        } => join(cli.server, &invitation, trust_names).await,
        Command::Environments { config } => list(config),
        Command::Ca { action } => ca(action.unwrap_or(CaAction::Status)),
    };
    if let Err(error) = outcome {
        eprintln!("devshare: {error:#}");
        std::process::exit(1);
    }
    // Not a plain return: a read of the terminal still pending would keep
    // the process alive until the next key press.
    std::process::exit(0);
}

/// The environments of the projects to share, as one configuration. One
/// session can hold several projects; a folder without a devshare.toml gets
/// it from its compose file on the way.
fn projects(folders: &[PathBuf], settings: &Settings) -> Result<Config> {
    let options = discover::Options {
        hostname: None,
        domain: Some(settings.domain()),
        environment: std::env::vars().collect(),
        hosts: Some("/etc/hosts".into()),
    };
    let mut together = Config::default();
    for folder in folders {
        let (project, found) = discover::project(folder, &options)?;
        if let Some(found) = found {
            println!(
                "{} written from {}.\n",
                folder.join(discover::FILE).display(),
                found.sources.join(", ")
            );
        }
        for (name, environment) in project.environments {
            if together
                .environments
                .insert(name.clone(), environment)
                .is_some()
            {
                bail!("two of these projects declare an environment named {name}");
            }
        }
        together.server = together.server.or(project.server);
    }
    Ok(together)
}

async fn share(
    server: Option<String>,
    mut folders: Vec<PathBuf>,
    environments: Vec<String>,
    lifetime: Option<Duration>,
    max_guests: Option<u32>,
    config: Option<PathBuf>,
    no_qr: bool,
) -> Result<()> {
    let settings = Settings::load()?;
    devshare_core::link::use_relay(settings.relay.clone());
    if let Some(stranger) = folders.iter().find(|folder| !folder.is_dir()) {
        bail!(
            "{} is not a folder (to share one environment of a project: --only <name>)",
            stranger.display()
        );
    }
    let mut started_from: Vec<PathBuf> = Vec::new();
    let config = match config {
        Some(file) if folders.is_empty() => Config::load(Some(file))?,
        Some(_) => bail!("give project folders or --config, not both"),
        None => {
            if folders.is_empty() {
                folders.push(PathBuf::from("."));
            }
            // The same folder written two ways is one project.
            let mut seen = Vec::new();
            folders.retain(|folder| {
                let real = folder.canonicalize().unwrap_or_else(|_| folder.clone());
                let new = !seen.contains(&real);
                seen.push(real);
                new
            });
            started_from = folders
                .iter()
                .filter(|folder| discover::has_compose_file(folder))
                .cloned()
                .collect();
            projects(&folders, &settings)?
        }
    };
    // What was asked for, else what the general settings say.
    let lifetime = match lifetime {
        Some(lifetime) => lifetime,
        None => settings.duration()?,
    };
    let max_guests = max_guests.unwrap_or_else(|| settings.guests());
    let selection = config.select(&environments)?;
    let server = environment::server(server, Some(&config), &settings);
    // A control plane meant to be on this machine and not running is not a
    // reason to fail: the session brings its own.
    let on_this_machine = devshare_server::is_local(&server);
    let own = devshare_server::ensure_local(&server)
        .await
        .with_context(|| format!("starting a control plane for {server}"))?;
    if own {
        println!("No control plane was running on this machine: this session runs its own,");
        println!("for as long as it lasts.\n");
    }
    let mut share = Share::start(ShareOptions {
        selection,
        lifetime,
        max_guests,
        server,
        join: settings.join(),
    })
    .await?;

    let manifest = share.manifest();
    let code = code::display(&share.code());
    let link = share.link();

    println!("Session created.\n");
    for (name, environment) in &manifest.environments {
        println!("  {name}");
        for service in &environment.services {
            println!("    {}", describe(service));
        }
    }
    for check in share.checks() {
        let service = format!("{}:{}", check.host, check.port);
        match &check.probe {
            Probe::Down => println!("\n! {service} does not answer on this machine yet."),
            Probe::Tls {
                covers_host: false, ..
            } => println!(
                "\n! {service} presents a certificate that is not for {}.",
                check.host
            ),
            _ => {}
        }
        if !check.local.is_empty() {
            println!(
                "\n! {service}: its pages point at {}. For a guest that is the guest's",
                check.local.join(", ")
            );
            println!("  own machine: those links, redirects, scripts or styles will not follow.");
        }
    }
    // Nothing answers at all: the project is not started, and that is the
    // one thing a guest cannot do anything about.
    let down = |check: &devshare_core::host::ServiceCheck| check.probe == Probe::Down;
    if !share.checks().is_empty() && share.checks().iter().all(down) {
        println!("\n! Nothing answers on these ports: the project is not started. Guests");
        println!("  will get a closed connection until it is. No need to share again.");
        for folder in &started_from {
            println!("    docker compose up -d      (in {})", folder.display());
        }
    }
    if !share.control_plane_is_current().await {
        println!("\n! The control plane is an older version: it serves no page for the");
        println!("  invitation link, so a phone that opens it gets an empty page.");
        println!("  Stop it and start devshare-server again, then share again.");
    }
    println!("\nInvitation:\n{link}\n\nCode:\n{code}\n");
    if !no_qr {
        // On a terminal the code sets its own colours; in a file it cannot.
        println!("{}\n", qr::terminal(&link, std::io::stdout().is_terminal()));
    }
    // One invitation. Said plainly when it cannot leave this network.
    if !share.works_from_anywhere() {
        println!("No relay is in use: this invitation only works on this network.\n");
    } else if on_this_machine {
        println!("The link and the QR code work from any network.\n");
    }
    println!(
        "Expires in {}. Up to {max_guests} guest{}. Ctrl-C stops sharing.",
        clock(share.remaining()),
        if max_guests == 1 { "" } else { "s" },
    );
    println!("Type \"guests\" to list them, \"revoke <number>\" to disconnect one,");
    println!("\"invite\" for a new code.\n");

    let mut routes: HashMap<u32, Route> = HashMap::new();
    let mut tick = tokio::time::interval(ROUTE_CHECK);
    let interrupted = interrupted();
    tokio::pin!(interrupted);
    // Commands come from the terminal, or from whatever drives the CLI.
    let mut commands = BufReader::new(tokio::io::stdin()).lines();
    let mut listening = true;
    loop {
        tokio::select! {
            _ = &mut interrupted => {
                println!("Sharing stopped.");
                break;
            }
            line = commands.next_line(), if listening => match line {
                Ok(Some(line)) => command(&share, &line).await,
                // No input any more is not a reason to stop sharing.
                _ => listening = false,
            },
            // A link starts relayed and usually becomes direct: say so when
            // it changes, a relayed guest is a slower guest.
            _ = tick.tick() => {
                for guest in share.guests() {
                    let Some((route, rtt)) = guest.route else { continue };
                    if routes.insert(guest.id, route) != Some(route) {
                        println!("  guest {}: {route}, {} ms", guest.id, rtt.as_millis());
                    }
                }
            }
            activity = share.activity() => match activity {
                Some(Activity::GuestJoined { id, device }) => {
                    println!("● guest {id} joined: {} ({})", device.label(), device.platform);
                }
                Some(Activity::GuestLeft { id }) => println!("○ guest {id} left"),
                Some(Activity::Refused { reason }) => println!("✗ a device was refused: {reason}"),
                Some(Activity::Locked { attempts }) => {
                    println!("✗ {attempts} wrong codes in a row: the invitation is withdrawn.");
                    println!("  Nobody else can join until you type \"invite\" for a new one.");
                }
                Some(Activity::Denied { guest, host, port }) => {
                    println!("✗ guest {guest} asked for {host}:{port}, which is not shared");
                }
                Some(Activity::Unreachable { guest, host, port }) => {
                    println!("! guest {guest} asked for {host}:{port}, which does not answer here");
                }
                Some(Activity::Ended { reason }) => {
                    println!("Session over: {reason}.");
                    break;
                }
                None => break,
            },
        }
    }
    share.stop().await;
    Ok(())
}

/// One line typed while sharing.
async fn command(share: &Share, line: &str) {
    let words: Vec<&str> = line.split_whitespace().collect();
    match words[..] {
        [] => {}
        ["guests"] => {
            let guests = share.guests();
            if guests.is_empty() {
                println!("No guest is connected.");
            }
            for guest in guests {
                let route = match guest.route {
                    Some((route, rtt)) => format!("{route}, {} ms", rtt.as_millis()),
                    None => "connecting".to_string(),
                };
                println!(
                    "  guest {}: {} ({}), {route}",
                    guest.id,
                    guest.device.label(),
                    guest.device.platform
                );
            }
        }
        ["revoke", guest] => match guest.parse() {
            Ok(guest) if share.revoke(guest).await => println!(
                "Guest {guest} revoked: disconnected, and its device cannot come back to this session."
            ),
            _ => println!("No guest {guest} is connected."),
        },
        ["invite"] => match share.invite().await {
            Ok(code) => {
                println!(
                    "New code: {}. The previous one no longer works.\n{}\n",
                    code::display(&code),
                    share.link()
                );
            }
            Err(error) => println!("No new invitation: {error:#}"),
        },
        _ => println!("Commands: guests, revoke <number>, invite. Ctrl-C stops sharing."),
    }
}

async fn join(server: Option<String>, invitation: &str, trust_names: bool) -> Result<()> {
    // What was asked for, else the control plane the link itself names,
    // else the one of the general settings.
    let settings = Settings::load()?;
    devshare_core::link::use_relay(settings.relay.clone());
    let server = server.or_else(|| qr::server_of(invitation));
    let server = environment::server(server, None, &settings);
    Tunnel::check_rights()?;
    // This device's lasting identity: a host that disconnects it keeps it out.
    let secret = DeviceSecret::load().unwrap_or_else(|error| {
        tracing::warn!("no device secret ({error:#}): joining without a lasting identity");
        DeviceSecret::random()
    });
    let mut link = GuestLink::join_as(invitation, &server, guest::this_device(), &secret).await?;

    let names = guest::NamePolicy {
        domains: vec![settings.domain()],
        trust_all: trust_names,
    };
    let termination = Termination::for_session(&link.manifest, &settings.domain());
    let certified: Vec<Service> = termination
        .as_ref()
        .map(|tls| tls.certified(&link.manifest).into_iter().cloned().collect())
        .unwrap_or_default();
    let tunnel = match Tunnel::start(link.opener(), &link.manifest, &names, termination).await {
        Ok(tunnel) => tunnel,
        Err(error) => {
            link.close().await;
            return Err(error);
        }
    };

    println!("Connected.\n");
    for (name, environment) in &link.manifest.environments {
        match &environment.entrypoint {
            Some(entrypoint) => println!("  {}: {}", clean(name, 64), clean(entrypoint, 200)),
            None => println!("  {}", clean(name, 64)),
        }
        for service in &environment.services {
            if certified.contains(service) {
                println!("    {}  certified by this device", describe(service));
            } else {
                println!("    {}", describe(service));
            }
        }
    }
    println!(
        "\nExpires in {}. Ctrl-C leaves the session.\n",
        clock(Duration::from_secs(link.manifest.session.expires_in)),
    );

    // Scoped: the future watching the session borrows the link until here.
    {
        let opener = link.opener();
        let mut shown = None;
        let mut tick = tokio::time::interval(ROUTE_CHECK);
        let interrupted = interrupted();
        let ended = link.ended();
        tokio::pin!(interrupted, ended);
        loop {
            tokio::select! {
                _ = &mut interrupted => {
                    println!("Session left.");
                    break;
                }
                end = &mut ended => {
                    println!("Session over: {end}.");
                    break;
                }
                _ = tick.tick() => {
                    if let Some((route, rtt)) = opener.route() {
                        if shown.replace(route) != Some(route) {
                            println!("Route: {route}, {} ms", rtt.as_millis());
                        }
                    }
                }
            }
        }
    }

    drop(tunnel);
    link.close().await;
    Ok(())
}

fn ca(action: CaAction) -> Result<()> {
    let settings = Settings::load()?;
    let domains = [settings.domain()];
    #[cfg(not(target_os = "macos"))]
    let helper = || {
        guest::Helper::connect()?.context(
            "trusting a certificate authority goes through DevShare's helper, which is not \
             installed: sudo devshare-helper install",
        )
    };
    match action {
        CaAction::Status => {
            let Some(ca) = DeviceCa::load(&domains)? else {
                println!("This device has no certificate authority of its own.");
                println!(
                    "devshare ca install makes one and has this computer trust it: https:// to \
                     the services of the sessions you join then opens without a warning."
                );
                return Ok(());
            };
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs() as i64;
            println!("{}", clean(ca.common_name(), 120));
            println!("  SHA-256   {}", ca.sha256());
            println!("  for       {}", ca.domains().join(", "));
            println!("  expires   in {} days", (ca.not_after() - now) / 86_400);
            println!("  file      {}", DeviceCa::certificate_path()?.display());
            if ca.trusted() {
                println!(
                    "  trusted   yes: sessions' https:// services are certified by this device"
                );
            } else {
                println!("  trusted   no: devshare ca install has this computer trust it");
            }
        }
        CaAction::Install => {
            let ca = match DeviceCa::load(&domains)? {
                Some(ca) => ca,
                None => DeviceCa::create(&domains)?,
            };
            #[cfg(target_os = "macos")]
            {
                println!("macOS asks for your password to change your trust settings, once.");
                ca.trust_for_this_user()?;
            }
            #[cfg(not(target_os = "macos"))]
            helper()?.trust_ca(ca.certificate_pem())?;
            println!(
                "This computer trusts {} for {}.",
                clean(ca.common_name(), 120),
                ca.domains().join(", ")
            );
            println!(
                "In the sessions you join, https:// to their services opens without a warning; \
                 only this device trusts it, and only for those names."
            );
            if cfg!(target_os = "linux") {
                println!(
                    "Firefox keeps a list of its own: import {} in its certificate settings to \
                     use it there.",
                    DeviceCa::certificate_path()?.display()
                );
            }
        }
        CaAction::Renew => {
            let old = DeviceCa::load(&domains)?;
            let new = DeviceCa::generate(&domains)?;
            // Trusted before it is kept, the old one untrusted last: a step
            // that fails leaves a working authority behind.
            #[cfg(target_os = "macos")]
            {
                new.trust_for_this_user()?;
                new.save()?;
                if let Some(old) = old.filter(DeviceCa::trusted) {
                    old.untrust_for_this_user()?;
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                let mut helper = helper()?;
                helper.trust_ca(new.certificate_pem())?;
                new.save()?;
                if let Some(old) = old.filter(DeviceCa::trusted) {
                    helper.untrust_ca(old.sha256())?;
                }
            }
            println!(
                "Renewed: this computer trusts {} in place of the earlier one.",
                clean(new.common_name(), 120)
            );
        }
        CaAction::Remove => {
            let Some(ca) = DeviceCa::load(&domains)? else {
                println!("This device has no certificate authority of its own.");
                return Ok(());
            };
            if ca.trusted() {
                #[cfg(target_os = "macos")]
                ca.untrust_for_this_user()?;
                #[cfg(not(target_os = "macos"))]
                helper()?.untrust_ca(ca.sha256())?;
            }
            DeviceCa::delete()?;
            println!("Removed: this computer no longer trusts it, and its key is deleted.");
        }
    }
    Ok(())
}

fn discover(
    directory: Option<PathBuf>,
    hostname: Option<String>,
    print: bool,
    force: bool,
) -> Result<()> {
    let directory = directory.unwrap_or_else(|| PathBuf::from("."));
    let options = discover::Options {
        hostname,
        domain: Some(Settings::load()?.domain()),
        environment: std::env::vars().collect(),
        hosts: Some("/etc/hosts".into()),
    };
    let found = discover::discover(&directory, &options)?;
    if print {
        print!("{}", found.to_toml(None));
        return Ok(());
    }

    println!("{} ({})\n", found.project, found.sources.join(", "));
    println!("  Shared");
    for port in found.shared() {
        println!(
            "    {:<6} {:<28} {}",
            port.host.unwrap_or_default(),
            port.label(),
            found.names_for(port).join(", ")
        );
    }
    if !found.routes.is_empty() {
        println!("\n  Names read from the project's proxies");
        for route in &found.routes {
            println!("    {:<28} {}", route.name, route.source);
        }
    }
    if found.left_out().next().is_some() {
        println!("\n  Left out");
        for port in found.left_out() {
            let host = port.host.map(|host| host.to_string()).unwrap_or_default();
            let reason = port.left_out.as_deref().unwrap_or_default();
            println!("    {host:<6} {}: {reason}", port.label());
        }
    }
    for note in &found.notes {
        println!("\n  {note}");
    }

    let path = discover::write(&directory, &found, force)?;
    println!("\nWritten to {}.", path.display());
    if let Some(entrypoint) = found.entrypoint() {
        println!("Guests will open {entrypoint}: the name comes with the session,");
        println!("they have nothing to set up. You keep your own address, localhost.");
    }
    // The exact command, from where the developer stands.
    let here = directory == Path::new(".");
    match here {
        true => println!("Share it with: devshare share"),
        false => println!("Share it with: devshare share {}", directory.display()),
    }
    Ok(())
}

/// The general settings as they apply, and where they come from.
fn settings(init: bool) -> Result<()> {
    let file = Settings::file();
    if init {
        if file.exists() {
            println!("{} exists already: it is left as it is.", file.display());
        } else {
            if let Some(folder) = file.parent() {
                std::fs::create_dir_all(folder)
                    .with_context(|| format!("creating {}", folder.display()))?;
            }
            std::fs::write(&file, Settings::TEMPLATE)
                .with_context(|| format!("writing {}", file.display()))?;
            println!(
                "Written to {}: every setting is commented out.",
                file.display()
            );
        }
    }

    let settings = Settings::load()?;
    let origin = |set: bool| if set { "" } else { "  (default)" };
    println!("General settings, common to all projects");
    match file.exists() {
        true => println!("  from {}\n", file.display()),
        false => println!(
            "  no {} yet: \"devshare settings --init\" writes it\n",
            file.display()
        ),
    }
    println!(
        "  server    {}{}",
        environment::server(None, None, &settings),
        origin(settings.server.is_some())
    );
    println!(
        "  duration  {}{}",
        clock(settings.duration()?),
        origin(settings.duration.is_some())
    );
    println!(
        "  guests    {}{}",
        settings.guests(),
        origin(settings.guests.is_some())
    );
    println!(
        "  domain    {}{}",
        settings.domain(),
        origin(settings.domain.is_some())
    );
    println!(
        "  relay     {}",
        match settings.relay.as_deref() {
            Some(relay) => relay.to_string(),
            None => "the public relays of the iroh project  (default)".to_string(),
        }
    );
    match settings.join() {
        Some(join) => println!("  join      {join}{}", origin(settings.join.is_some())),
        None => println!("  join      none: links point at the control plane"),
    }
    println!(
        "\nThe environments of a project are in its own folder: devshare discover writes them."
    );
    Ok(())
}

fn list(config: Option<PathBuf>) -> Result<()> {
    for (name, environment) in &Config::load(config)?.environments {
        println!("{name}");
        for service in &environment.services {
            match &service.target {
                Some(target) => println!("  {}:{} → {target}", service.host, service.port),
                None => println!("  {}:{}", service.host, service.port),
            }
        }
    }
    Ok(())
}

/// `shop.test:443  TLS 3f9a1c0e…`: the start of the certificate's fingerprint
/// is enough to compare two screens by eye.
fn describe(service: &Service) -> String {
    match &service.tls {
        Some(tls) => format!(
            "{}:{}  TLS {}…",
            service.host,
            service.port,
            &tls.sha256[..tls.sha256.len().min(16)]
        ),
        None => format!("{}:{}", service.host, service.port),
    }
}

fn clock(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

/// Ctrl-C, or the termination signal a service manager sends.
async fn interrupted() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate()).expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await.ok();
}
