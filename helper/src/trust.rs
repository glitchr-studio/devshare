//! Making the system trust a guest device's own certificate authority, and
//! stop trusting it.
//!
//! Only certificates that pass [`devshare_protocol::authority::check`] are
//! accepted: self-signed authorities constrained to the domains a session
//! may name, excluding every IP address, valid for two years at most. Each
//! is recorded with the user who asked, in [`TRUSTED_CAS`]; a user removes
//! only its own, and uninstalling the helper removes them all. Nothing the
//! helper did not record is ever touched.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

use anyhow::{anyhow, bail, Context, Result};
use devshare_protocol::{
    authority,
    helper::{MOST_CAS_PER_USER, TRUSTED_CAS},
    names::NamePolicy,
};

/// Where the certificates the helper trusted are kept, by SHA-256.
const KEPT: &str = "/etc/devshare/cas";

/// One change to the record and the system's store at a time.
static CHANGING: Mutex<()> = Mutex::new(());

pub struct Store {
    pub record: PathBuf,
    pub kept: PathBuf,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            record: PathBuf::from(TRUSTED_CAS),
            kept: PathBuf::from(KEPT),
        }
    }
}

/// Makes the system trust the authority in `pem` for `uid`. Returns its
/// SHA-256. Trusting it again is harmless.
pub fn trust(store: &Store, uid: u32, pem: &str, domains: Vec<String>) -> Result<String> {
    let policy = NamePolicy {
        domains,
        trust_all: false,
    };
    let authority =
        authority::check(pem, &policy).map_err(|reason| anyhow!("refused: {reason}"))?;
    let sha256 = authority.sha256;

    let _changing = CHANGING.lock().unwrap();
    let entries = read(&store.record);
    let theirs: Vec<&(String, u32)> = entries.iter().filter(|(_, owner)| *owner == uid).collect();
    let known = entries.iter().any(|(sha, _)| *sha == sha256);
    if !known && theirs.len() >= MOST_CAS_PER_USER {
        bail!(
            "this user already has {} authorities trusted: remove one first (devshare ca remove)",
            theirs.len()
        );
    }
    if let Some((_, owner)) = entries.iter().find(|(sha, _)| *sha == sha256) {
        if *owner != uid {
            bail!("this authority was trusted for another user");
        }
    }

    fs::create_dir_all(&store.kept)
        .with_context(|| format!("creating {}", store.kept.display()))?;
    let kept = store.kept.join(format!("{sha256}.pem"));
    fs::write(&kept, der_to_pem(&authority.der))?;
    fs::set_permissions(&kept, fs::Permissions::from_mode(0o644))?;
    install(&sha256, &kept)?;

    if !known {
        let mut entries = entries;
        entries.push((sha256.clone(), uid));
        write(&store.record, &entries)?;
    }
    tracing::info!(
        "trusted {} ({sha256:.16}) for uid {uid}, for {}",
        devshare_protocol::clean(&authority.common_name, 80),
        authority.domains.join(", ")
    );
    Ok(sha256)
}

/// Stops trusting the authority `sha256`, if `uid` trusted it.
pub fn untrust(store: &Store, uid: u32, sha256: &str) -> Result<()> {
    let sha256 = sha256.to_ascii_lowercase();
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("not a SHA-256 in hexadecimal");
    }
    let _changing = CHANGING.lock().unwrap();
    let mut entries = read(&store.record);
    let Some(index) = entries
        .iter()
        .position(|(sha, owner)| *sha == sha256 && (*owner == uid || uid == 0))
    else {
        bail!("this user has no trusted authority with that SHA-256");
    };
    remove(store, &sha256)?;
    entries.remove(index);
    write(&store.record, &entries)?;
    tracing::info!("no longer trusted: {sha256:.16}, for uid {uid}");
    Ok(())
}

/// Removes every authority the helper trusted. For uninstalling.
pub fn untrust_all(store: &Store) {
    let _changing = CHANGING.lock().unwrap();
    for (sha256, _) in read(&store.record) {
        if let Err(error) = remove(store, &sha256) {
            eprintln!("could not remove the authority {sha256:.16}: {error:#}");
        }
    }
    fs::remove_file(&store.record).ok();
    fs::remove_dir(&store.kept).ok();
}

fn remove(store: &Store, sha256: &str) -> Result<()> {
    let kept = store.kept.join(format!("{sha256}.pem"));
    uninstall(sha256, &kept)?;
    fs::remove_file(&kept).ok();
    Ok(())
}

fn read(record: &Path) -> Vec<(String, u32)> {
    fs::read_to_string(record)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let sha = parts.next()?.to_string();
            let uid = parts.next()?.parse().ok()?;
            Some((sha, uid))
        })
        .collect()
}

fn write(record: &Path, entries: &[(String, u32)]) -> Result<()> {
    let text: String = entries
        .iter()
        .map(|(sha, uid)| format!("{sha} {uid}\n"))
        .collect();
    if let Some(folder) = record.parent() {
        fs::create_dir_all(folder)?;
    }
    let staged = record.with_extension("new");
    fs::write(&staged, text)?;
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o644))?;
    fs::rename(&staged, record).with_context(|| format!("writing {}", record.display()))
}

/// The certificate as it is installed: its DER only, whatever text came
/// around it in the request.
fn der_to_pem(der: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let text = STANDARD.encode(der);
    let lines: Vec<&str> = text
        .as_bytes()
        .chunks(64)
        .map(|line| std::str::from_utf8(line).unwrap_or_default())
        .collect();
    format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        lines.join("\n")
    )
}

#[cfg(target_os = "macos")]
const KEYCHAIN: &str = "/Library/Keychains/System.keychain";

#[cfg(target_os = "macos")]
fn install(_sha256: &str, kept: &Path) -> Result<()> {
    let kept = kept.to_string_lossy();
    run(
        "security",
        &[
            "add-trusted-cert",
            "-d",
            "-r",
            "trustRoot",
            "-k",
            KEYCHAIN,
            &kept,
        ],
    )
}

#[cfg(target_os = "macos")]
fn uninstall(sha256: &str, kept: &Path) -> Result<()> {
    if kept.exists() {
        run(
            "security",
            &["remove-trusted-cert", "-d", &kept.to_string_lossy()],
        )
        .ok();
    }
    run("security", &["delete-certificate", "-Z", sha256, KEYCHAIN])
}

/// Debian and its family, then Fedora and its family.
#[cfg(target_os = "linux")]
const ANCHORS: [(&str, &str, &[&str]); 2] = [
    (
        "/usr/local/share/ca-certificates",
        "update-ca-certificates",
        &[],
    ),
    (
        "/etc/pki/ca-trust/source/anchors",
        "update-ca-trust",
        &["extract"],
    ),
];

#[cfg(target_os = "linux")]
fn anchor(sha256: &str) -> Result<(PathBuf, &'static str, &'static [&'static str])> {
    ANCHORS
        .iter()
        .find(|(folder, _, _)| Path::new(folder).is_dir())
        .map(|(folder, update, arguments)| {
            (Path::new(folder).join(format!("devshare-{sha256:.16}.crt")), *update, *arguments)
        })
        .ok_or_else(|| {
            anyhow!("this system has no store of certificate authorities the helper knows: install ca-certificates")
        })
}

#[cfg(target_os = "linux")]
fn install(sha256: &str, kept: &Path) -> Result<()> {
    let (path, update, arguments) = anchor(sha256)?;
    fs::copy(kept, &path).with_context(|| format!("copying to {}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
    run(update, arguments)
}

#[cfg(target_os = "linux")]
fn uninstall(sha256: &str, _kept: &Path) -> Result<()> {
    let (path, update, arguments) = anchor(sha256)?;
    match fs::remove_file(&path) {
        Ok(()) => run(update, arguments),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context(format!("removing {}", path.display())),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn install(_sha256: &str, _kept: &Path) -> Result<()> {
    bail!("this system is not supported")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn uninstall(_sha256: &str, _kept: &Path) -> Result<()> {
    Ok(())
}

fn run(program: &str, arguments: &[&str]) -> Result<()> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} failed ({}): {}",
            output.status,
            devshare_protocol::clean(&String::from_utf8_lossy(&output.stderr), 300)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pem_is_base64_in_lines_of_64() {
        let pem = der_to_pem(&[0u8; 60]);
        let lines: Vec<usize> = pem.lines().map(str::len).collect();
        assert_eq!(lines, [27, 64, 16, 25]);
    }

    #[test]
    fn the_record_is_read_back_as_written() {
        let record = std::env::temp_dir().join(format!("devshare-trusted-{}", std::process::id()));
        let entries = vec![("a".repeat(64), 501), ("b".repeat(64), 1000)];
        write(&record, &entries).unwrap();
        assert_eq!(read(&record), entries);
        let text = fs::read_to_string(&record).unwrap();
        assert!(devshare_protocol::helper::records(&text, &"b".repeat(64)));
        assert!(!devshare_protocol::helper::records(&text, &"c".repeat(64)));
        fs::remove_file(&record).ok();
    }

    #[test]
    fn what_is_not_a_device_authority_never_reaches_the_system() {
        let store = Store {
            record: std::env::temp_dir().join("devshare-never-record"),
            kept: std::env::temp_dir().join("devshare-never-kept"),
        };
        let error = trust(
            &store,
            501,
            "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
            vec![],
        )
        .unwrap_err()
        .to_string();
        assert!(error.starts_with("refused"), "{error}");
        assert!(!store.record.exists() && !store.kept.exists());
        assert!(untrust(&store, 501, "../../etc/passwd").is_err());
        assert!(untrust(&store, 501, &"a".repeat(64)).is_err());
    }
}
