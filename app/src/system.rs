//! What the app asks of the system around it: programs a window's process
//! does not find on its own, and administrator rights for the helper.

use std::path::PathBuf;

/// A program by name. An app opened from the Finder does not get the
/// terminal's PATH: the usual places of Docker and Homebrew are looked in
/// too.
pub fn program(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(
            [
                "/usr/local/bin",
                "/opt/homebrew/bin",
                "/Applications/Docker.app/Contents/Resources/bin",
                "/usr/bin",
            ]
            .map(PathBuf::from),
        )
        .map(|folder| folder.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// The helper that comes with the app: next to its executable, in the app
/// bundle or in the build folder.
fn helper() -> Result<PathBuf, String> {
    let beside = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .with_file_name("devshare-helper");
    if beside.is_file() {
        return Ok(beside);
    }
    let found = program("devshare-helper");
    if found.is_file() {
        return Ok(found);
    }
    Err("the helper is not next to the app: build it with make dmg".into())
}

fn login() -> String {
    std::env::var("USER").unwrap_or_default()
}

/// Installs the helper for this user, the system asking for an
/// administrator's password: macOS's own dialog, polkit's on Linux.
pub async fn install_helper() -> Result<(), String> {
    let helper = helper()?;
    #[cfg(target_os = "macos")]
    let output = tokio::process::Command::new("osascript")
        .args([
            "-e",
            "on run arguments",
            "-e",
            "do shell script (quoted form of item 1 of arguments) & \" install --user \" & \
             (quoted form of item 2 of arguments) with prompt \"DevShare installs its helper, so \
             that joining a session needs no administrator rights from then on.\" with \
             administrator privileges",
            "-e",
            "end run",
        ])
        .arg(&helper)
        .arg(login())
        .output()
        .await;
    #[cfg(not(target_os = "macos"))]
    let output = tokio::process::Command::new("pkexec")
        .arg(&helper)
        .args(["install", "--user", &login()])
        .output()
        .await;
    let output = output.map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    if said.contains("User canceled") || said.contains("-128") {
        return Err("not installed: the password was not given".into());
    }
    Err(devshare_core::protocol::clean(&said, 400))
}
