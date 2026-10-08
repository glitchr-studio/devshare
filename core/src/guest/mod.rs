//! The guest side of a sharing session.
//!
//! [`GuestLink`] joins a session and opens streams to its services; it needs
//! no privilege. [`Tunnel`] makes those services reachable under their real
//! hostnames for every program of the device: a virtual interface, a
//! resolver for the shared names and a userspace TCP stack.

mod addresses;
mod dns;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod helper;
mod link;
pub mod proxy;
mod summary;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod system;
mod tls;
mod tunnel;

pub use addresses::AddressPlan;
pub use devshare_protocol::names::{is_hostname, NamePolicy, DEV_DOMAINS};
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub use helper::Helper;
pub use link::{End, GuestLink, JoinError, OpenError, Opener};
pub use summary::{Summary, SummaryEnvironment, SummaryService};
pub use tls::Termination;
pub use tunnel::Tunnel;

use devshare_protocol::Device;

/// This computer, as a host will see it in its list of guests.
pub fn this_device() -> Device {
    // A guest without the privileged helper joins with sudo: the login that
    // matters is the one that asked for administrator rights, not root.
    let user = ["SUDO_USER", "USER", "LOGNAME", "USERNAME"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|user| !user.is_empty() && user != "root");
    Device {
        name: gethostname::gethostname().to_string_lossy().into_owned(),
        platform: std::env::consts::OS.to_string(),
        user,
    }
}
