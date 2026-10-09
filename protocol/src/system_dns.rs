//! Makes the device ask a session's resolver for the shared hostnames, and
//! only for them. Run as root: by a guest that joined with `sudo`, or by the
//! privileged helper for a guest that did not. Both use this one
//! implementation, so that what they write and what they remove agree.

use std::{io, net::Ipv4Addr};

const BEGIN: &str = "# >>> devshare session, removed when it ends";
const END: &str = "# <<< devshare";
/// The names of this machine's own projects, pointed at itself while the
/// desktop app runs: a block of its own, which a session never touches.
const LOCAL_BEGIN: &str = "# >>> devshare projects of this machine, removed when the app quits";
const LOCAL_END: &str = "# <<< devshare projects";

/// What [`install`] put in place, to give back to [`remove`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// Per-interface settings of systemd-resolved: gone with the interface.
    #[cfg(target_os = "linux")]
    Resolved { interface: String },
    /// A marked block in the hosts file.
    #[cfg(target_os = "linux")]
    Hosts,
    /// One file per hostname in `/etc/resolver`.
    #[cfg(target_os = "macos")]
    ResolverFiles,
}

/// Points the session's names at its resolver on `interface`. Nothing is
/// left in place when this fails.
#[cfg(target_os = "linux")]
pub fn install(
    interface: &str,
    names: &[(String, Ipv4Addr)],
    resolver: Ipv4Addr,
) -> io::Result<Installed> {
    if linux::resolved(interface, names, resolver) {
        return Ok(Installed::Resolved {
            interface: interface.to_string(),
        });
    }
    rewrite_hosts(|hosts| with_block(hosts, names)).inspect_err(|_| {
        rewrite_hosts(without_block).ok();
    })?;
    Ok(Installed::Hosts)
}

#[cfg(target_os = "macos")]
pub fn install(
    _interface: &str,
    names: &[(String, Ipv4Addr)],
    resolver: Ipv4Addr,
) -> io::Result<Installed> {
    macos::remove();
    macos::install(names, resolver).inspect_err(|_| macos::remove())?;
    Ok(Installed::ResolverFiles)
}

pub fn remove(installed: &Installed) -> io::Result<()> {
    match installed {
        #[cfg(target_os = "linux")]
        Installed::Resolved { interface } => {
            // For a session ended before its interface is closed; once the
            // interface is gone, there is nothing left to revert.
            linux::revert(interface);
            Ok(())
        }
        #[cfg(target_os = "linux")]
        Installed::Hosts => rewrite_hosts(without_block),
        #[cfg(target_os = "macos")]
        Installed::ResolverFiles => {
            macos::remove();
            Ok(())
        }
    }
}

/// Removes whatever a session that was killed left behind.
pub fn remove_leftovers() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    rewrite_hosts(without_block)?;
    #[cfg(target_os = "macos")]
    macos::remove();
    Ok(())
}

/// Points the names of this machine's own projects at itself, replacing the
/// ones pointed before; none removes them. In the hosts file on every
/// system, IPv4 and IPv6 alike (a `.local` name asked for IPv6 alone would
/// otherwise wait for Bonjour).
pub fn set_local(names: &[String]) -> io::Result<()> {
    rewrite_hosts(|hosts| with_local(hosts, names))?;
    flush_cache();
    Ok(())
}

/// The hosts file with this machine's own block of `names`, or without it.
fn with_local(hosts: &str, names: &[String]) -> String {
    let mut content = between(hosts, LOCAL_BEGIN, LOCAL_END);
    if names.is_empty() {
        return content;
    }
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(LOCAL_BEGIN);
    content.push('\n');
    for name in names {
        content.push_str(&format!("127.0.0.1\t{name}\n::1\t{name}\n"));
    }
    content.push_str(LOCAL_END);
    content.push('\n');
    content
}

/// The system's resolver forgets what it knew of names just changed.
fn flush_cache() {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("dscacheutil")
            .arg("-flushcache")
            .status()
            .ok();
        std::process::Command::new("killall")
            .args(["-HUP", "mDNSResponder"])
            .status()
            .ok();
    }
}

fn hosts_file() -> String {
    std::env::var("DEVSHARE_HOSTS_FILE").unwrap_or_else(|_| "/etc/hosts".to_string())
}

/// Written in place: the hosts file is often a mount that cannot be
/// replaced by a rename.
fn rewrite_hosts(change: impl FnOnce(&str) -> String) -> io::Result<()> {
    let path = hosts_file();
    let context = |doing: &str, error: io::Error| {
        io::Error::new(error.kind(), format!("{doing} {path}: {error}"))
    };
    let current = std::fs::read_to_string(&path).map_err(|error| context("reading", error))?;
    let changed = change(&current);
    if changed != current {
        std::fs::write(&path, changed).map_err(|error| context("writing", error))?;
    }
    Ok(())
}

/// The hosts file with the session's block, replacing any earlier one.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn with_block(hosts: &str, names: &[(String, Ipv4Addr)]) -> String {
    let mut content = without_block(hosts);
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(BEGIN);
    content.push('\n');
    for (name, address) in names {
        content.push_str(&format!("{address}\t{name}\n"));
    }
    content.push_str(END);
    content.push('\n');
    content
}

/// The hosts file as it was before any session touched it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn without_block(hosts: &str) -> String {
    between(hosts, BEGIN, END)
}

/// `hosts` without the lines from `begin` to `end`.
fn between(hosts: &str, begin: &str, end: &str) -> String {
    let mut content = String::with_capacity(hosts.len());
    let mut inside = false;
    for line in hosts.split_inclusive('\n') {
        let trimmed = line.trim_end();
        if trimmed == begin {
            inside = true;
        } else if trimmed == end {
            inside = false;
        } else if !inside {
            content.push_str(line);
        }
    }
    content
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{net::Ipv4Addr, path::Path, process::Command};

    fn resolvectl(args: &[String]) -> bool {
        Command::new("resolvectl")
            .args(args)
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Routes the shared names, and only them, to the session's resolver.
    pub fn resolved(interface: &str, names: &[(String, Ipv4Addr)], resolver: Ipv4Addr) -> bool {
        if !Path::new("/run/systemd/resolve/stub-resolv.conf").exists() {
            return false;
        }
        let mut domains = vec!["domain".to_string(), interface.to_string()];
        domains.extend(names.iter().map(|(name, _)| format!("~{name}")));

        resolvectl(&["dns".into(), interface.into(), resolver.to_string()]) && resolvectl(&domains)
    }

    pub fn revert(interface: &str) {
        resolvectl(&["revert".into(), interface.into()]);
    }

}

#[cfg(target_os = "macos")]
mod macos {
    use std::{fs, io, net::Ipv4Addr, path::PathBuf, process::Command};

    use super::BEGIN;

    const DIRECTORY: &str = "/etc/resolver";

    fn ours(path: &PathBuf) -> bool {
        fs::read_to_string(path).is_ok_and(|content| content.starts_with(BEGIN))
    }

    pub fn install(names: &[(String, Ipv4Addr)], resolver: Ipv4Addr) -> io::Result<()> {
        let context = |doing: &str, path: &str, error: io::Error| {
            io::Error::new(error.kind(), format!("{doing} {path}: {error}"))
        };
        fs::create_dir_all(DIRECTORY).map_err(|error| context("creating", DIRECTORY, error))?;
        for (name, _) in names {
            let path = PathBuf::from(DIRECTORY).join(name);
            if path.exists() && !ours(&path) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} already exists and is not DevShare's", path.display()),
                ));
            }
            let content = format!("{BEGIN}\nnameserver {resolver}\n");
            fs::write(&path, content)
                .map_err(|error| context("writing", &path.display().to_string(), error))?;
        }
        flush();
        Ok(())
    }

    pub fn remove() {
        let Ok(entries) = fs::read_dir(DIRECTORY) else {
            return;
        };
        let mut removed = false;
        for path in entries.flatten().map(|entry| entry.path()) {
            if ours(&path) {
                removed |= fs::remove_file(&path).is_ok();
            }
        }
        if removed {
            flush();
        }
    }

    fn flush() {
        Command::new("dscacheutil").arg("-flushcache").status().ok();
        Command::new("killall")
            .args(["-HUP", "mDNSResponder"])
            .status()
            .ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hosts_file_comes_back_exactly_as_it_was() {
        let names = [
            ("api.shop.test".to_string(), Ipv4Addr::new(198, 18, 90, 10)),
            ("shop.test".to_string(), Ipv4Addr::new(198, 18, 90, 11)),
        ];

        for original in ["127.0.0.1\tlocalhost\n", "127.0.0.1 localhost", ""] {
            let shared = with_block(original, &names);
            assert!(shared.contains("198.18.90.11\tshop.test\n"), "{shared}");
            assert!(shared.contains("198.18.90.10\tapi.shop.test\n"), "{shared}");

            // A second session replaces the block of the first.
            assert_eq!(with_block(&shared, &names), shared);

            let cleaned = without_block(&shared);
            assert_eq!(
                cleaned.trim_end_matches('\n'),
                original.trim_end_matches('\n')
            );
        }
    }

    #[test]
    fn this_machine_s_own_names_and_a_session_s_live_side_by_side() {
        let hosts = "127.0.0.1 localhost\n";
        let session = with_block(
            hosts,
            &[("shop.test".into(), Ipv4Addr::new(198, 18, 90, 10))],
        );
        let both = with_local(
            &session,
            &["chapaland.local".into(), "www.chapaland.local".into()],
        );
        assert!(
            both.contains("127.0.0.1\tchapaland.local\n::1\tchapaland.local\n"),
            "{both}"
        );
        assert!(
            both.contains("198.18.90.10\tshop.test"),
            "the session's block stays: {both}"
        );
        // Replaced, not added to.
        let again = with_local(&both, &["avocat.local".into()]);
        assert!(
            !again.contains("chapaland") && again.contains("avocat.local"),
            "{again}"
        );
        // A session's end leaves this machine's names; none removes them.
        let ended = without_block(&again);
        assert!(
            ended.contains("avocat.local") && !ended.contains("shop.test"),
            "{ended}"
        );
        assert_eq!(with_local(&ended, &[]), hosts);
    }
}
