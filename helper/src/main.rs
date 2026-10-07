//! `devshare-helper`: what needs administrator rights on a guest's computer,
//! and nothing else. It creates a session's network interface, routes the
//! session's network into it, installs the session's names in the system's
//! resolver, and hands the interface to the guest's own process as a file
//! descriptor. The guest's process, running as the user, does everything
//! else.
//!
//! Installed once with `sudo devshare-helper install`, it runs at boot and
//! listens on a Unix socket; it serves the users recorded at installation
//! and closes the connection of anyone else before reading a byte.

mod install;
mod serve;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Where the users the helper serves are listed, one uid per line.
const USERS: &str = "/etc/devshare/users";
/// Domains a session may name besides the test domains, one per line. Root
/// writes it; the helper only reads it.
const TRUSTED_DOMAINS: &str = "/etc/devshare/trusted-domains";

#[derive(Parser)]
#[command(
    name = "devshare-helper",
    version,
    about = "The part of DevShare that needs administrator rights on a guest's computer"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Install the helper so that it runs at boot, serving the user who
    /// runs this command (with sudo).
    Install {
        /// Serve this user too. May be repeated; may be run again later.
        #[arg(long = "user")]
        users: Vec<String>,
    },
    /// Stop the helper and remove everything install put in place.
    Uninstall,
    /// Serve guests. What the installed helper runs; needs root.
    Run {
        #[arg(long, default_value = devshare_protocol::helper::SOCKET)]
        socket: PathBuf,
        #[arg(long, default_value = USERS)]
        users: PathBuf,
        #[arg(long, default_value = TRUSTED_DOMAINS)]
        trusted_domains: PathBuf,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .without_time()
        .with_target(false)
        .init();

    match Cli::parse().command {
        Command::Install { users } => install::install(&users),
        Command::Uninstall => install::uninstall(),
        Command::Run {
            socket,
            users,
            trusted_domains,
        } => serve::run(serve::Options {
            socket,
            users,
            trusted_domains,
        }),
    }
}

/// Everything here changes the system: only root may.
fn must_be_root(doing: &str) -> Result<()> {
    if !nix::unistd::geteuid().is_root() {
        anyhow::bail!("{doing} needs administrator rights: run it with sudo");
    }
    Ok(())
}
