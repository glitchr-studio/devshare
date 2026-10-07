//! Types shared by every DevShare component: the session manifest, the
//! messages exchanged between a guest and a host, the control-plane API, and
//! what a guest and the privileged helper say to each other.
//!
//! Protocol 2. The invitation code is drawn by the host and never sent to
//! anyone: it is the password of a SPAKE2 exchange between the guest and the
//! host, and the control plane only ever sees a slow-to-compute key derived
//! from it, which lets a guest find the host but not join it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub mod code;
pub mod helper;
pub mod names;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod system_dns;

/// Version of the guest/host wire protocol.
pub const PROTOCOL_VERSION: u32 = 2;

/// Version of the manifest schema.
pub const MANIFEST_VERSION: u32 = 1;

/// ALPN of the guest/host link.
pub const ALPN: &[u8] = b"devshare/2";

/// What a control plane answers to `GET /v2`: how it is told from anything
/// else that listens on a port, and from another version.
pub const CONTROL_PLANE: &str = "devshare control plane, protocol 2\n";

/// Upper bound of a control message, manifest included.
pub const MAX_FRAME: usize = 1024 * 1024;

// ---------------------------------------------------------------- manifest

/// What a guest receives after joining: the shared environments, and nothing
/// about how the host reaches them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub protocol: u32,
    pub manifest_version: u32,
    pub session: SessionInfo,
    pub environments: BTreeMap<String, Environment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    /// Seconds the session lasts in total.
    pub lifetime: u64,
    /// Seconds left when the manifest was sent. Guests count down from this
    /// rather than comparing clocks with the host.
    pub expires_in: u64,
    pub max_guests: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    pub dns: Vec<String>,
    pub services: Vec<Service>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Service {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub protocol: Transport,
    /// Present when the service speaks TLS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<Tls>,
}

/// What the host saw when it connected to the service itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Tls {
    /// SHA-256 of the certificate the service presents, in hexadecimal. A
    /// guest may accept exactly this certificate for the hostname, for the
    /// life of the session, without trusting whoever issued it.
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    #[default]
    Tcp,
}

impl Manifest {
    /// Every hostname a guest must resolve, lowercased, without duplicates.
    pub fn hostnames(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .environments
            .values()
            .flat_map(|env| {
                env.dns
                    .iter()
                    .chain(env.services.iter().map(|service| &service.host))
            })
            .map(|name| normalize_host(name))
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// Text that came from the other side, made safe to print or display:
/// without control characters (which could redraw a terminal or hide what
/// follows), bidirectional overrides, or more than `most` characters.
pub fn clean(text: &str, most: usize) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // A whole escape sequence goes, not just its first byte: `ESC [`
            // up to a final letter, `ESC ]` up to BEL or `ESC \`, else one
            // character.
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.peek() == Some(&'\\')) {
                            chars.next_if_eq(&'\\');
                            break;
                        }
                    }
                }
                // `ESC` then intermediates (`(`, `#`, …) then one final byte.
                Some(c) if ('\u{20}'..='\u{2f}').contains(&c) => {
                    while chars
                        .next_if(|c| ('\u{20}'..='\u{2f}').contains(c))
                        .is_some()
                    {}
                    chars.next();
                }
                _ => {}
            }
            continue;
        }
        if c.is_control() || matches!(c, '\u{2028}'..='\u{202E}' | '\u{2066}'..='\u{2069}') {
            continue;
        }
        if out.chars().count() >= most {
            break;
        }
        out.push(c);
    }
    out
}

/// Hostnames are compared lowercased and without a trailing dot.
pub fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

// ------------------------------------------------------------ guest ↔ host

/// First message of a guest, on the control stream. The invitation code is
/// not in it: `pake` is the guest's SPAKE2 message, made with the code as
/// the password.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub device: Device,
    /// Optional features the guest understands, for manifest negotiation.
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub pake: Vec<u8>,
}

/// The guest's proof that it derived the same key as the host, sent after
/// the host's [`HelloReply::Challenge`]: an HMAC of a fixed label under that
/// key. A guest with the wrong code cannot make it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Confirm {
    pub mac: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// The computer's name.
    pub name: String,
    pub platform: String,
    /// The login of whoever joined, on their own computer. Said by the guest
    /// itself: it tells the host who to expect, it proves nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

impl Device {
    /// The same device, as it may be shown: see [`clean`].
    pub fn cleaned(&self) -> Self {
        Self {
            name: clean(&self.name, 64),
            platform: clean(&self.platform, 32),
            user: self.user.as_deref().map(|user| clean(user, 64)),
        }
    }

    /// `alice on Alices-MacBook`, or the computer's name alone.
    pub fn label(&self) -> String {
        match &self.user {
            Some(user) => format!("{user} on {}", self.name),
            None => self.name.clone(),
        }
    }
}

/// The host's answers during admission: a challenge to [`Hello`], then a
/// welcome to [`Confirm`]; a refusal at either step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HelloReply {
    /// The host's SPAKE2 message.
    Challenge {
        pake: Vec<u8>,
    },
    /// `mac` is the host's own proof, under the same key: a guest checks it
    /// before trusting anything else the host says.
    Welcome {
        guest_id: u32,
        manifest: Manifest,
        mac: Vec<u8>,
    },
    Reject {
        reason: RejectReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    UnsupportedProtocol,
    InvalidInvitation,
    SessionFull,
    SessionEnded,
    /// The host disconnected this device: it may not come back.
    Revoked,
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedProtocol => "this version of DevShare is not supported by the host",
            Self::InvalidInvitation => "the invitation is invalid or has expired",
            Self::SessionFull => "the session has reached its maximum number of guests",
            Self::SessionEnded => "the session has ended",
            Self::Revoked => "the host disconnected this device from the session",
        })
    }
}

/// Pushed by the host on the control stream after the welcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostEvent {
    /// The manifest changed during the session.
    Manifest {
        manifest: Manifest,
    },
    Ended {
        reason: EndReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Expired,
    Stopped,
    Revoked,
    /// The same device joined again, from another process: that one stays.
    Replaced,
}

impl std::fmt::Display for EndReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Expired => "the session expired",
            Self::Stopped => "the host stopped sharing",
            Self::Revoked => "the host revoked this device",
            Self::Replaced => "this device joined the session again from another process",
        })
    }
}

/// First message of every data stream: which service the guest wants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Open {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenReply {
    Ok,
    Denied,
    Unreachable,
}

// ----------------------------------------------------------- control plane

/// `POST /v2/invitations`. `lookup` is the key a guest finds the host by
/// (32 hexadecimal digits); the code it is derived from never reaches the
/// control plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInvitation {
    pub lookup: String,
    /// Where to reach the host. Opaque to the control plane.
    pub host: serde_json::Value,
    /// Seconds the invitation stays redeemable.
    pub ttl: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvitationCreated {
    /// Lets the host, and only the host, withdraw the invitation.
    pub owner_token: String,
    pub ttl: u64,
}

/// `GET /v2/invitations/{lookup}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvitationLookup {
    pub host: serde_json::Value,
}

/// Whether `text` is shaped like a lookup key: 32 lowercase hexadecimal
/// digits.
pub fn is_lookup(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_other_side_says_cannot_redraw_a_terminal() {
        assert_eq!(clean("a\x1b[2J\x07b\r\nc\u{202e}d", 10), "abcd");
        assert_eq!(clean("x\x1b]0;title\x1b\\y\x1b(Bz\x1b", 10), "xyz");
        assert_eq!(clean("é漢字 ok", 10), "é漢字 ok");
        assert_eq!(clean(&"x".repeat(100), 8), "xxxxxxxx");
        let device = Device {
            name: "evil\x1b]0;owned\x07".into(),
            platform: "linux".into(),
            user: Some("\u{2066}alice".into()),
        };
        assert_eq!(device.cleaned().label(), "alice on evil");
    }
}
