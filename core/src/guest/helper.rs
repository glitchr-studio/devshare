//! The privileged helper, as a guest without administrator rights sees it.
//!
//! The helper (`devshare-helper`, installed once with sudo) creates the
//! session's interface, routes the session's network into it and installs
//! the session's names, then hands the interface over as a file descriptor.
//! Everything else runs here, as the user. The names stay installed for as
//! long as this connection is open: when the process ends, however it ends,
//! the helper removes them.

use std::{
    io::{IoSliceMut, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::net::UnixStream,
    },
    path::Path,
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use devshare_protocol::helper::{
    self, Name, Outcome, Reply, Request, Up, HELPER_PROTOCOL, MAX_MESSAGE, SOCKET,
};
use nix::sys::socket::{recvmsg, ControlMessageOwned, MsgFlags};

use super::AddressPlan;

/// How long the helper may take to answer. Creating an interface and telling
/// the resolver about a few names takes a fraction of a second.
const ANSWER: Duration = Duration::from_secs(30);

pub struct Helper {
    stream: UnixStream,
}

impl Helper {
    /// The helper, when it is installed. `None` when it is not.
    pub fn connect() -> Result<Option<Self>> {
        Self::connect_to(Path::new(SOCKET))
    }

    pub fn connect_to(socket: &Path) -> Result<Option<Self>> {
        if !socket.exists() {
            return Ok(None);
        }
        let stream = UnixStream::connect(socket).map_err(|error| {
            anyhow!(
                "DevShare's helper is installed but does not answer ({error}). \
                 Start it again with: sudo devshare-helper install"
            )
        })?;
        // On macOS, setting an option on a socket the other side has already
        // closed fails: that is what a helper refusing this user looks like,
        // when it closes before this line is reached.
        let timed = stream.set_read_timeout(Some(ANSWER)).is_ok()
            && stream.set_write_timeout(Some(ANSWER)).is_ok();
        if !timed {
            return Err(refused_user());
        }

        let mut helper = Self { stream };
        // A user the helper does not know gets no answer at all: it closes
        // the connection before reading anything.
        let welcome = helper
            .ask(&Request::Hello {
                protocol: HELPER_PROTOCOL,
            })
            .map_err(|_| refused_user())?;
        match welcome {
            (Reply::Welcome { protocol, .. }, _) if protocol == HELPER_PROTOCOL => Ok(Some(helper)),
            (Reply::Welcome { version, .. }, _) => bail!(
                "DevShare's helper is from another version ({}): install this version's with \
                 sudo devshare-helper install",
                devshare_protocol::clean(&version, 20)
            ),
            (Reply::Outcome(Outcome::Error(error)), _) => {
                bail!(
                    "DevShare's helper refused: {}",
                    devshare_protocol::clean(&error, 400)
                )
            }
            _ => bail!("DevShare's helper gave an unexpected answer"),
        }
    }

    /// Asks for the session's interface and names. Returns the interface's
    /// descriptor, which only this process then holds, and its name.
    pub fn up(&mut self, plan: &AddressPlan) -> Result<(OwnedFd, String)> {
        let (network, prefix) = AddressPlan::network();
        debug_assert!(helper::contains(network, prefix, AddressPlan::local()));
        let request = Request::Up(Up {
            names: plan
                .names()
                .iter()
                .map(|(name, address)| Name {
                    name: name.clone(),
                    address: *address,
                })
                .collect(),
            address: AddressPlan::local(),
            resolver: AddressPlan::resolver(),
            prefix,
        });
        match self.ask(&request).context("asking DevShare's helper")? {
            (Reply::Outcome(Outcome::Up { interface }), Some(descriptor)) => {
                Ok((descriptor, devshare_protocol::clean(&interface, 32)))
            }
            (Reply::Outcome(Outcome::Up { .. }), None) => {
                bail!("DevShare's helper created the interface but did not hand it over")
            }
            (Reply::Outcome(Outcome::Error(error)), _) => {
                bail!(
                    "DevShare's helper refused: {}",
                    devshare_protocol::clean(&error, 400)
                )
            }
            _ => bail!("DevShare's helper gave an unexpected answer"),
        }
    }

    /// Removes the session's names. Closing the connection does the same:
    /// this is for a guest that leaves a session and keeps running.
    pub fn down(&mut self) -> Result<()> {
        match self
            .ask(&Request::Down {})
            .context("asking DevShare's helper")?
        {
            (Reply::Outcome(Outcome::Down {}), _) => Ok(()),
            (Reply::Outcome(Outcome::Error(error)), _) => {
                bail!(
                    "DevShare's helper refused: {}",
                    devshare_protocol::clean(&error, 400)
                )
            }
            _ => bail!("DevShare's helper gave an unexpected answer"),
        }
    }

    /// Makes the system trust this device's own certificate authority, its
    /// certificate in PEM. Returns its SHA-256, as the helper recorded it.
    pub fn trust_ca(&mut self, certificate: &str) -> Result<String> {
        let request = Request::TrustCa {
            certificate: certificate.to_string(),
        };
        match self.ask(&request).context("asking DevShare's helper")? {
            (Reply::Outcome(Outcome::Trusted { sha256 }), _) => {
                Ok(devshare_protocol::clean(&sha256, 64))
            }
            (Reply::Outcome(Outcome::Error(error)), _) => Err(refusal(&error)),
            _ => bail!("DevShare's helper gave an unexpected answer"),
        }
    }

    /// Stops trusting an authority this user had the helper trust.
    pub fn untrust_ca(&mut self, sha256: &str) -> Result<()> {
        let request = Request::UntrustCa {
            sha256: sha256.to_string(),
        };
        match self.ask(&request).context("asking DevShare's helper")? {
            (Reply::Outcome(Outcome::Untrusted {}), _) => Ok(()),
            (Reply::Outcome(Outcome::Error(error)), _) => Err(refusal(&error)),
            _ => bail!("DevShare's helper gave an unexpected answer"),
        }
    }

    /// Points the names of this machine's own projects at itself while this
    /// connection stays open; none removes them.
    pub fn local(&mut self, names: &[String]) -> Result<()> {
        let request = Request::Local {
            names: names.to_vec(),
        };
        match self.ask(&request) {
            Ok((Reply::Outcome(Outcome::Local {}), _)) => Ok(()),
            Ok((Reply::Outcome(Outcome::Error(error)), _)) => Err(refusal(&error)),
            Ok(_) => bail!("DevShare's helper gave an unexpected answer"),
            // An older helper closes on a request it does not know.
            Err(_) => Err(anyhow!(
                "DevShare's helper is older than this app: install this version's \
                 (Settings, This computer, Install)"
            )),
        }
    }

    /// One request, one reply, and the descriptor that came with it if any.
    fn ask(&mut self, request: &Request) -> Result<(Reply, Option<OwnedFd>)> {
        self.stream.write_all(&helper::encode(request))?;

        let mut received = Vec::new();
        let mut descriptor = None;
        loop {
            if let Some((reply, _)) = helper::decode(&received, MAX_MESSAGE)? {
                return Ok((reply, descriptor));
            }
            let mut chunk = [0u8; 4096];
            let (bytes, descriptors) = receive(&self.stream, &mut chunk)?;
            // Owned at once, so that any descriptor not used is closed.
            for fd in descriptors {
                // SAFETY: the kernel just created this descriptor for this
                // process; nothing else refers to it.
                let fd = unsafe { OwnedFd::from_raw_fd(fd) };
                descriptor.get_or_insert(fd);
            }
            if bytes == 0 {
                bail!("DevShare's helper closed the connection");
            }
            received.extend_from_slice(&chunk[..bytes]);
        }
    }
}

/// Bytes from the helper, and the descriptors that came with them.
fn receive(stream: &UnixStream, chunk: &mut [u8]) -> Result<(usize, Vec<RawFd>)> {
    #[cfg(target_os = "linux")]
    let flags = MsgFlags::MSG_CMSG_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let flags = MsgFlags::empty();

    let mut space = nix::cmsg_space!([RawFd; 1]);
    let mut iov = [IoSliceMut::new(chunk)];
    let message = recvmsg::<()>(stream.as_raw_fd(), &mut iov, Some(&mut space), flags)
        .context("reading from DevShare's helper")?;
    let mut descriptors = Vec::new();
    for control in message.cmsgs().context("reading from DevShare's helper")? {
        if let ControlMessageOwned::ScmRights(fds) = control {
            descriptors.extend(fds);
        }
    }
    // Not inherited by the programs this process starts.
    #[cfg(not(target_os = "linux"))]
    for fd in &descriptors {
        use nix::fcntl::{fcntl, FcntlArg, FdFlag};
        // SAFETY: just received, still open, and borrowed only for this call.
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(*fd) };
        fcntl(borrowed, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).ok();
    }
    Ok((message.bytes, descriptors))
}

/// What the helper said when it refused; an older helper refuses what it
/// does not know with words of its own.
fn refusal(error: &str) -> anyhow::Error {
    if error.contains("not in this version") {
        return anyhow!(
            "DevShare's helper is older than this guest: install this version's with \
             sudo devshare-helper install"
        );
    }
    anyhow!(
        "DevShare's helper refused: {}",
        devshare_protocol::clean(error, 400)
    )
}

fn login() -> String {
    ["USER", "LOGNAME"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|user| !user.is_empty())
        .unwrap_or_else(|| "<your login>".to_string())
}

fn refused_user() -> anyhow::Error {
    anyhow!(
        "DevShare's helper does not accept this user. Add it once with: \
         sudo devshare-helper install --user {}",
        login()
    )
}

/// What a guest without administrator rights and without the helper is told
/// when the interface could not be created.
pub fn without_rights(error: anyhow::Error) -> anyhow::Error {
    anyhow!(
        "joining needs administrator rights, to create the session's network interface \
         ({error:#}).\n\nInstall DevShare's helper once, and devshare join works without sudo \
         from then on:\n\n    sudo devshare-helper install\n\nOr join this time with: \
         sudo devshare join <invitation>"
    )
}

#[cfg(test)]
mod tests {
    use std::{
        io::{IoSlice, Read},
        os::unix::net::UnixListener,
        path::PathBuf,
        thread,
    };

    use nix::sys::socket::{sendmsg, ControlMessage};

    use super::*;
    use crate::guest::addresses::tests::manifest;

    fn socket(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("ds-{}-{name}.sock", std::process::id()));
        std::fs::remove_file(&path).ok();
        path
    }

    fn reply(stream: &mut UnixStream, reply: &Reply, descriptor: Option<RawFd>) {
        let frame = helper::encode(reply);
        let fds: Vec<RawFd> = descriptor.into_iter().collect();
        let controls: Vec<ControlMessage> = if fds.is_empty() {
            Vec::new()
        } else {
            vec![ControlMessage::ScmRights(&fds)]
        };
        sendmsg::<()>(
            stream.as_raw_fd(),
            &[IoSlice::new(&frame)],
            &controls,
            MsgFlags::empty(),
            None,
        )
        .unwrap();
    }

    #[test]
    fn no_socket_no_helper() {
        assert!(Helper::connect_to(&socket("none")).unwrap().is_none());
    }

    #[test]
    fn a_user_the_helper_does_not_know_is_told_how_to_be_added() {
        // Whether the helper closes before or after the client is ready to
        // talk, the client gives the same instructions.
        for wait in [Duration::ZERO, Duration::from_millis(100)] {
            let path = socket("refused");
            let listener = UnixListener::bind(&path).unwrap();
            let helper = thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                thread::sleep(wait);
                drop(stream);
            });

            let error = Helper::connect_to(&path).err().unwrap().to_string();
            assert!(
                error.contains("sudo devshare-helper install --user"),
                "{error}"
            );
            helper.join().unwrap();
            std::fs::remove_file(&path).ok();
        }
    }

    #[test]
    fn the_interface_arrives_with_the_reply_and_belongs_to_the_guest() {
        let path = socket("up");
        let listener = UnixListener::bind(&path).unwrap();
        let helper = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let hello: Request = helper::read(&mut stream, MAX_MESSAGE).unwrap();
            assert_eq!(hello, Request::Hello { protocol: 1 });
            reply(
                &mut stream,
                &Reply::Welcome {
                    version: "0.1.0".into(),
                    protocol: 1,
                },
                None,
            );

            let Request::Up(up) = helper::read(&mut stream, MAX_MESSAGE).unwrap() else {
                panic!("not an up");
            };
            assert_eq!(up.prefix, 24);
            assert_eq!(up.names.len(), 2);
            // A pipe stands in for the interface: what matters is that the
            // descriptor arrives, and works.
            let (reader, mut writer) = std::io::pipe().unwrap();
            writer.write_all(b"packet").unwrap();
            reply(
                &mut stream,
                &Reply::Outcome(Outcome::Up {
                    interface: "utun9".into(),
                }),
                Some(reader.as_raw_fd()),
            );
            drop(reader);

            let down: Request = helper::read(&mut stream, MAX_MESSAGE).unwrap();
            assert_eq!(down, Request::Down {});
            reply(&mut stream, &Reply::Outcome(Outcome::Down {}), None);
        });

        let plan =
            AddressPlan::new(&manifest(&[("shop.test", 80), ("api.shop.test", 80)])).unwrap();
        let mut client = Helper::connect_to(&path).unwrap().unwrap();
        let (descriptor, interface) = client.up(&plan).unwrap();
        assert_eq!(interface, "utun9");
        let mut packet = String::new();
        std::fs::File::from(descriptor)
            .take(6)
            .read_to_string(&mut packet)
            .unwrap();
        assert_eq!(packet, "packet");
        client.down().unwrap();

        helper.join().unwrap();
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn without_rights_says_what_to_run() {
        let error = without_rights(anyhow!("Operation not permitted")).to_string();
        assert!(error.contains("sudo devshare-helper install"), "{error}");
        assert!(error.contains("Operation not permitted"), "{error}");
    }
}
