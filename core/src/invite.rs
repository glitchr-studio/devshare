//! What makes an invitation code worth something without ever sending it:
//! the lookup key the control plane knows the host by, the SPAKE2 exchange
//! between guest and host, and the guest's lasting identity toward a host.

use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use devshare_protocol::code;
use hkdf::Hkdf;
use hmac::{Hmac, KeyInit, Mac};
use iroh::{EndpointId, SecretKey};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};

/// Fixed so that both sides derive the same key; changing it is a new
/// protocol.
const LOOKUP_SALT: &[u8] = b"devshare-lookup-2";
/// About 150 ms and 64 MiB a guess: a table of all 2^40 codes would cost an
/// operator thousands of CPU-years, while an invitation lives a day at most.
const LOOKUP_MEMORY_KIB: u32 = 64 * 1024;
const LOOKUP_PASSES: u32 = 3;

const GUEST_IDENTITY: &[u8] = b"devshare-guest";
/// The labels of the two proofs: a guest's proof cannot pass for the host's.
pub const GUEST_PROOF: &[u8] = b"devshare-2 guest proves the code";
pub const HOST_PROOF: &[u8] = b"devshare-2 host proves the code";

const DEVICE_SALT: &[u8] = b"devshare-guest-2";
const DEVICE_FILE: &str = "device-secret";

/// The key the control plane knows an invitation by, from its code: 32
/// hexadecimal digits. Slow on purpose; run off the async threads.
pub async fn lookup_key(code: &str) -> Result<String> {
    let code = code.to_string();
    tokio::task::spawn_blocking(move || derive_lookup(&code)).await?
}

fn derive_lookup(code: &str) -> Result<String> {
    let params = Params::new(LOOKUP_MEMORY_KIB, LOOKUP_PASSES, 1, Some(16))
        .map_err(|error| anyhow!("{error}"))?;
    let mut key = [0u8; 16];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(code.as_bytes(), LOOKUP_SALT, &mut key)
        .map_err(|error| anyhow!("{error}"))?;
    Ok(key.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// One side of the SPAKE2 exchange. The host plays A, the guest B; both
/// name the host by its endpoint identity, so that a key agreed with one
/// host is worth nothing with another.
pub struct Pake(Spake2<Ed25519Group>);

/// The key both sides agree on when they used the same code.
pub struct SharedKey(Vec<u8>);

impl Pake {
    /// The host's side, and its message. `None` when `code` is not a code.
    pub fn host(code: &str, host: &EndpointId) -> Option<(Self, Vec<u8>)> {
        let password = code::to_bytes(code)?;
        let (state, message) = Spake2::<Ed25519Group>::start_a(
            &Password::new(password),
            &Identity::new(host.as_bytes()),
            &Identity::new(GUEST_IDENTITY),
        );
        Some((Self(state), message))
    }

    /// The guest's side, and its message.
    pub fn guest(code: &str, host: &EndpointId) -> Option<(Self, Vec<u8>)> {
        let password = code::to_bytes(code)?;
        let (state, message) = Spake2::<Ed25519Group>::start_b(
            &Password::new(password),
            &Identity::new(host.as_bytes()),
            &Identity::new(GUEST_IDENTITY),
        );
        Some((Self(state), message))
    }

    /// The key, from the other side's message. `None` when that message is
    /// not one; a wrong code gives a key, just not the other side's.
    pub fn finish(self, theirs: &[u8]) -> Option<SharedKey> {
        self.0.finish(theirs).ok().map(SharedKey)
    }
}

impl SharedKey {
    /// The proof that this side holds the key, under one of the two labels.
    pub fn prove(&self, label: &[u8]) -> Vec<u8> {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.0)
            .expect("HMAC takes a key of any length");
        mac.update(label);
        mac.finalize().into_bytes().to_vec()
    }

    /// Whether the other side's proof is right, in constant time.
    pub fn verify(&self, label: &[u8], proof: &[u8]) -> bool {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.0)
            .expect("HMAC takes a key of any length");
        mac.update(label);
        mac.verify_slice(proof).is_ok()
    }
}

/// What a guest's device is known by: a secret made once and kept, from
/// which it derives a different key toward every host. Stable toward one
/// host, so that a host can turn that device away for good; unlinkable from
/// one host to the next.
pub struct DeviceSecret([u8; 32]);

impl DeviceSecret {
    /// A secret for this process only: a guest with no lasting identity.
    pub fn random() -> Self {
        Self(rand::random())
    }

    /// This device's secret, made the first time. Under sudo it is kept in
    /// the home of the user who asked, not root's.
    pub fn load() -> Result<Self> {
        let folder = data_folder()?;
        let path = folder.join(DEVICE_FILE);
        if let Ok(bytes) = fs::read(&path) {
            let secret: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow!("{} is not a device secret", path.display()))?;
            return Ok(Self(secret));
        }
        fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
        let secret = Self::random();
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("creating {}", path.display()))?;
        file.write_all(&secret.0)?;
        give_back(&folder);
        give_back(&path);
        Ok(secret)
    }

    /// The key this device uses toward `host`.
    pub fn key_for(&self, host: &EndpointId) -> SecretKey {
        let mut key = [0u8; 32];
        Hkdf::<Sha256>::new(Some(DEVICE_SALT), &self.0)
            .expand(host.as_bytes(), &mut key)
            .expect("32 bytes is a valid length for HKDF-SHA256");
        SecretKey::from_bytes(&key)
    }
}

/// `DEVSHARE_DATA`, else the user's data folder for DevShare.
pub(crate) fn data_folder() -> Result<PathBuf> {
    if let Some(folder) = std::env::var_os("DEVSHARE_DATA") {
        return Ok(PathBuf::from(folder));
    }
    let home = match std::env::var("SUDO_USER").ok().filter(|_| is_root()) {
        Some(user) => nix::unistd::User::from_name(&user)?
            .map(|user| user.dir)
            .ok_or_else(|| anyhow!("no user named {user}"))?,
        None => std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("no home folder"))?,
    };
    if cfg!(target_os = "macos") {
        return Ok(home.join("Library/Application Support/devshare"));
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|_| !is_root())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    Ok(data.join("devshare"))
}

fn is_root() -> bool {
    nix::unistd::geteuid().is_root()
}

/// Under sudo, what was created belongs to the user who asked.
pub(crate) fn give_back(path: &std::path::Path) {
    if !is_root() {
        return;
    }
    let id = |name: &str| {
        std::env::var(name)
            .ok()
            .and_then(|id| id.parse::<u32>().ok())
    };
    if let (Some(uid), Some(gid)) = (id("SUDO_UID"), id("SUDO_GID")) {
        nix::unistd::chown(
            path,
            Some(nix::unistd::Uid::from_raw(uid)),
            Some(nix::unistd::Gid::from_raw(gid)),
        )
        .ok();
    }
}

/// Fails unless `code` is one.
pub fn require_code(code: &str) -> Result<()> {
    if code::to_bytes(code).is_none() {
        bail!("not an invitation code");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> EndpointId {
        SecretKey::from_bytes(&[3; 32]).public()
    }

    #[tokio::test]
    async fn the_lookup_key_is_the_same_on_both_sides_and_slow_to_make() {
        let started = std::time::Instant::now();
        let first = lookup_key("7GX2KLM9").await.unwrap();
        let took = started.elapsed();
        assert_eq!(first, lookup_key("7GX2KLM9").await.unwrap());
        assert!(devshare_protocol::is_lookup(&first), "{first}");
        assert_ne!(first, lookup_key("7GX2KLM8").await.unwrap());
        // Slow enough to matter, fast enough to wait for.
        assert!(took.as_millis() >= 20 && took.as_secs() < 5, "{took:?}");
    }

    #[test]
    fn the_same_code_agrees_and_another_does_not() {
        let (host_side, to_guest) = Pake::host("7GX2KLM9", &host()).unwrap();
        let (guest_side, to_host) = Pake::guest("7GX2KLM9", &host()).unwrap();
        let (on_host, on_guest) = (
            host_side.finish(&to_host).unwrap(),
            guest_side.finish(&to_guest).unwrap(),
        );
        let proof = on_guest.prove(GUEST_PROOF);
        assert!(on_host.verify(GUEST_PROOF, &proof));
        // A guest's proof is not the host's.
        assert!(!on_host.verify(HOST_PROOF, &proof));

        // A wrong code: keys are made, proofs fail.
        let (host_side, to_guest) = Pake::host("7GX2KLM9", &host()).unwrap();
        let (guest_side, to_host) = Pake::guest("7GX2KLM8", &host()).unwrap();
        let (on_host, on_guest) = (
            host_side.finish(&to_host).unwrap(),
            guest_side.finish(&to_guest).unwrap(),
        );
        assert!(!on_host.verify(GUEST_PROOF, &on_guest.prove(GUEST_PROOF)));

        // Agreed with another host's identity: worth nothing with this one.
        let other = SecretKey::from_bytes(&[4; 32]).public();
        let (host_side, to_guest) = Pake::host("7GX2KLM9", &host()).unwrap();
        let (guest_side, to_host) = Pake::guest("7GX2KLM9", &other).unwrap();
        let (on_host, on_guest) = (
            host_side.finish(&to_host).unwrap(),
            guest_side.finish(&to_guest).unwrap(),
        );
        assert!(!on_host.verify(GUEST_PROOF, &on_guest.prove(GUEST_PROOF)));

        assert!(Pake::host("not a code", &host()).is_none());
        let (host_side, _) = Pake::host("7GX2KLM9", &host()).unwrap();
        assert!(host_side.finish(b"junk").is_none());
    }

    #[test]
    fn a_device_has_one_key_per_host_and_keeps_its_secret() {
        let folder = std::env::temp_dir().join(format!("devshare-device-{}", std::process::id()));
        std::fs::remove_dir_all(&folder).ok();
        // Read by load(): the test sets it for itself only.
        std::env::set_var("DEVSHARE_DATA", &folder);
        let first = DeviceSecret::load().unwrap();
        let again = DeviceSecret::load().unwrap();
        std::env::remove_var("DEVSHARE_DATA");

        let other = SecretKey::from_bytes(&[4; 32]).public();
        assert_eq!(
            first.key_for(&host()).public(),
            again.key_for(&host()).public()
        );
        assert_ne!(
            first.key_for(&host()).public(),
            first.key_for(&other).public()
        );
        assert_ne!(
            first.key_for(&host()).public(),
            DeviceSecret::random().key_for(&host()).public()
        );
        let mode = std::fs::metadata(folder.join(DEVICE_FILE))
            .unwrap()
            .permissions();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(mode.mode() & 0o777, 0o600);
        std::fs::remove_dir_all(&folder).ok();
    }
}
