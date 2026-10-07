//! Makes the device ask the tunnel's resolver for the shared hostnames, and
//! only for them. Everything installed here is removed on drop, and anything
//! a killed process left behind is removed at the next start.

use anyhow::Result;

use super::AddressPlan;

const BEGIN: &str = "# >>> devshare session, removed when it ends";
const END: &str = "# <<< devshare";

pub struct SystemDns {
    mode: Mode,
}

enum Mode {
    /// Per-interface settings of systemd-resolved: gone with the interface.
    #[cfg(target_os = "linux")]
    Resolved,
    /// A marked block in the hosts file.
    #[cfg(target_os = "linux")]
    Hosts,
    /// One file per hostname in `/etc/resolver`.
    #[cfg(target_os = "macos")]
    ResolverFiles,
}

impl SystemDns {
    #[cfg(target_os = "linux")]
    pub fn install(interface: &str, plan: &AddressPlan) -> Result<Self> {
        if linux::resolved(interface, plan) {
            return Ok(Self {
                mode: Mode::Resolved,
            });
        }
        // Armed before writing: a half-written block is still removed.
        let installed = Self { mode: Mode::Hosts };
        linux::rewrite_hosts(|hosts| with_block(hosts, plan))?;
        Ok(installed)
    }

    #[cfg(target_os = "macos")]
    pub fn install(_interface: &str, plan: &AddressPlan) -> Result<Self> {
        macos::remove();
        let installed = Self {
            mode: Mode::ResolverFiles,
        };
        macos::install(plan)?;
        Ok(installed)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn install(_interface: &str, _plan: &AddressPlan) -> Result<Self> {
        anyhow::bail!("joining a session is not supported on this system yet")
    }
}

impl Drop for SystemDns {
    fn drop(&mut self) {
        match self.mode {
            #[cfg(target_os = "linux")]
            Mode::Resolved => {}
            #[cfg(target_os = "linux")]
            Mode::Hosts => {
                if let Err(error) = linux::rewrite_hosts(without_block) {
                    tracing::error!("could not clean the hosts file: {error:#}");
                }
            }
            #[cfg(target_os = "macos")]
            Mode::ResolverFiles => macos::remove(),
        }
    }
}

/// The hosts file with the session's block, replacing any earlier one.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn with_block(hosts: &str, plan: &AddressPlan) -> String {
    let mut content = without_block(hosts);
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(BEGIN);
    content.push('\n');
    for (name, address) in plan.names() {
        content.push_str(&format!("{address}\t{name}\n"));
    }
    content.push_str(END);
    content.push('\n');
    content
}

/// The hosts file as it was before any session touched it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn without_block(hosts: &str) -> String {
    let mut content = String::with_capacity(hosts.len());
    let mut inside = false;
    for line in hosts.split_inclusive('\n') {
        match line.trim_end() {
            BEGIN => inside = true,
            END => inside = false,
            _ if !inside => content.push_str(line),
            _ => {}
        }
    }
    content
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{fs, path::Path, process::Command};

    use anyhow::{Context, Result};

    use super::AddressPlan;

    fn hosts_file() -> String {
        std::env::var("DEVSHARE_HOSTS_FILE").unwrap_or_else(|_| "/etc/hosts".to_string())
    }

    /// Routes the shared names, and only them, to the tunnel's resolver.
    pub fn resolved(interface: &str, plan: &AddressPlan) -> bool {
        if !Path::new("/run/systemd/resolve/stub-resolv.conf").exists() {
            return false;
        }
        let resolvectl = |args: &[String]| {
            Command::new("resolvectl")
                .args(args)
                .status()
                .is_ok_and(|status| status.success())
        };
        let mut domains = vec!["domain".to_string(), interface.to_string()];
        domains.extend(plan.names().iter().map(|(name, _)| format!("~{name}")));

        resolvectl(&[
            "dns".into(),
            interface.into(),
            AddressPlan::resolver().to_string(),
        ]) && resolvectl(&domains)
    }

    /// Written in place: the hosts file is often a mount that cannot be
    /// replaced by a rename.
    pub fn rewrite_hosts(change: impl FnOnce(&str) -> String) -> Result<()> {
        let path = hosts_file();
        let current = fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
        let changed = change(&current);
        if changed != current {
            fs::write(&path, changed).with_context(|| format!("writing {path}"))?;
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::{fs, path::PathBuf, process::Command};

    use anyhow::{bail, Context, Result};

    use super::{AddressPlan, BEGIN};

    const DIRECTORY: &str = "/etc/resolver";

    fn ours(path: &PathBuf) -> bool {
        fs::read_to_string(path).is_ok_and(|content| content.starts_with(BEGIN))
    }

    pub fn install(plan: &AddressPlan) -> Result<()> {
        fs::create_dir_all(DIRECTORY).with_context(|| format!("creating {DIRECTORY}"))?;
        for (name, _) in plan.names() {
            let path = PathBuf::from(DIRECTORY).join(name);
            if path.exists() && !ours(&path) {
                bail!("{} already exists and is not DevShare's", path.display());
            }
            let content = format!("{BEGIN}\nnameserver {}\n", AddressPlan::resolver());
            fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
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
    use crate::guest::addresses::tests::manifest;

    #[test]
    fn the_hosts_file_comes_back_exactly_as_it_was() {
        let plan =
            AddressPlan::new(&manifest(&[("shop.test", 443), ("api.shop.test", 8080)])).unwrap();

        for original in ["127.0.0.1\tlocalhost\n", "127.0.0.1 localhost", ""] {
            let shared = with_block(original, &plan);
            assert!(shared.contains("198.18.90.11\tshop.test\n"), "{shared}");
            assert!(shared.contains("198.18.90.10\tapi.shop.test\n"), "{shared}");

            // A second session replaces the block of the first.
            assert_eq!(with_block(&shared, &plan), shared);

            let cleaned = without_block(&shared);
            assert_eq!(
                cleaned.trim_end_matches('\n'),
                original.trim_end_matches('\n')
            );
        }
    }
}
