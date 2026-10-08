//! Putting the helper in place so that it runs at boot, and taking it out.
//! Everything here is undone by `uninstall`.

use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use anyhow::{anyhow, bail, Context, Result};

#[cfg(target_os = "macos")]
const BINARY: &str = "/Library/PrivilegedHelperTools/studio.glitchr.devshare.helper";
#[cfg(target_os = "macos")]
const LABEL: &str = "studio.glitchr.devshare.helper";
#[cfg(target_os = "macos")]
const PLIST: &str = "/Library/LaunchDaemons/studio.glitchr.devshare.helper.plist";

#[cfg(target_os = "linux")]
const BINARY: &str = "/usr/local/libexec/devshare-helper";
#[cfg(target_os = "linux")]
const UNIT: &str = "/etc/systemd/system/devshare-helper.service";

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const BINARY: &str = "/usr/local/libexec/devshare-helper";

pub fn install(users: &[String]) -> Result<()> {
    crate::must_be_root("installing the helper")?;

    // The users to serve: whoever ran sudo, and the ones named.
    let mut uids: Vec<u32> = Vec::new();
    if let Some(uid) = std::env::var("SUDO_UID")
        .ok()
        .and_then(|uid| uid.parse().ok())
    {
        uids.push(uid);
    }
    for name in users {
        let user = nix::unistd::User::from_name(name)
            .with_context(|| format!("looking up {name}"))?
            .ok_or_else(|| anyhow!("no user named {name}"))?;
        uids.push(user.uid.as_raw());
    }
    if uids.is_empty() {
        bail!("no user to serve: run this with sudo as yourself, or name one with --user");
    }
    fs::create_dir_all("/etc/devshare").context("creating /etc/devshare")?;
    let known = fs::read_to_string(crate::USERS).unwrap_or_default();
    let mut lines: Vec<String> = known.lines().map(str::to_string).collect();
    for uid in &uids {
        if !lines.iter().any(|line| line.trim() == uid.to_string()) {
            lines.push(uid.to_string());
        }
    }
    fs::write(crate::USERS, format!("{}\n", lines.join("\n"))).context("writing the users file")?;

    // This very binary, where the system expects a daemon's.
    let me = std::env::current_exe().context("finding this program")?;
    if let Some(directory) = Path::new(BINARY).parent() {
        fs::create_dir_all(directory)?;
    }
    stop().ok();
    fs::copy(&me, BINARY).with_context(|| format!("copying to {BINARY}"))?;
    fs::set_permissions(BINARY, fs::Permissions::from_mode(0o755))?;
    start()?;

    println!("DevShare's helper is installed and running.");
    println!("Served users: {}.", lines.join(", "));
    println!(
        "devshare join works without sudo for them from now on; \
         sudo devshare-helper uninstall removes it."
    );
    Ok(())
}

pub fn uninstall() -> Result<()> {
    crate::must_be_root("uninstalling the helper")?;
    stop().ok();
    crate::trust::untrust_all(&crate::trust::Store::default());
    for path in [BINARY, crate::USERS, devshare_protocol::helper::SOCKET] {
        fs::remove_file(path).ok();
    }
    remove_service_files();
    devshare_protocol::system_dns::remove_leftovers().ok();
    println!("DevShare's helper is removed.");
    Ok(())
}

#[cfg(target_os = "macos")]
fn start() -> Result<()> {
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{LABEL}</string>
    <key>ProgramArguments</key><array><string>{BINARY}</string><string>run</string></array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardErrorPath</key><string>/var/log/devshare-helper.log</string>
</dict>
</plist>
"#
    );
    fs::write(PLIST, plist).with_context(|| format!("writing {PLIST}"))?;
    fs::set_permissions(PLIST, fs::Permissions::from_mode(0o644))?;
    run("launchctl", &["bootstrap", "system", PLIST])
}

#[cfg(target_os = "macos")]
fn stop() -> Result<()> {
    // Quietly: there is nothing to stop on a first installation.
    let stopped = Command::new("launchctl")
        .args(["bootout", &format!("system/{LABEL}")])
        .output()
        .context("running launchctl")?;
    if !stopped.status.success() {
        bail!("the helper was not running");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_service_files() {
    fs::remove_file(PLIST).ok();
}

#[cfg(target_os = "linux")]
fn start() -> Result<()> {
    let unit = format!(
        "[Unit]\nDescription=DevShare helper: what needs root on a guest's computer\n\n\
         [Service]\nExecStart={BINARY} run\nRestart=on-failure\n\n\
         [Install]\nWantedBy=multi-user.target\n"
    );
    fs::write(UNIT, unit).with_context(|| format!("writing {UNIT}"))?;
    run("systemctl", &["daemon-reload"])?;
    run("systemctl", &["enable", "--now", "devshare-helper"])
}

#[cfg(target_os = "linux")]
fn stop() -> Result<()> {
    run("systemctl", &["disable", "--now", "devshare-helper"])
}

#[cfg(target_os = "linux")]
fn remove_service_files() {
    fs::remove_file(UNIT).ok();
    run("systemctl", &["daemon-reload"]).ok();
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn start() -> Result<()> {
    bail!("this system is not supported")
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn stop() -> Result<()> {
    Ok(())
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn remove_service_files() {}

fn run(program: &str, arguments: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(arguments)
        .status()
        .with_context(|| format!("running {program}"))?;
    if !status.success() {
        bail!("{program} {} failed ({status})", arguments.join(" "));
    }
    Ok(())
}
