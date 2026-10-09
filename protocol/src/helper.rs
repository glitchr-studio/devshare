//! What a guest without administrator rights and the privileged helper say
//! to each other, on a Unix socket only the helper listens on.
//!
//! The helper does what needs root, and only that: it creates the session's
//! interface, routes the session's network into it, installs the session's
//! names in the system's resolver, and hands the interface over as a file
//! descriptor. Messages are framed as on the guest/host link: a 4-byte
//! big-endian length, then JSON.
//!
//! ```text
//! {"hello":{"protocol":1}}                        {"version":"0.1.0","protocol":1}
//! {"up":{"names":[…],"address":…,"resolver":…,"prefix":24}}
//!                                                 {"up":{"interface":"utun5"}} and the descriptor
//! {"down":{}}                                     {"down":{}}
//! {"trust_ca":{"certificate":"-----BEGIN…"}}      {"trusted":{"sha256":"…"}}
//! {"untrust_ca":{"sha256":"…"}}                   {"untrusted":{}}
//! {"local":{"names":["shop.local"]}}              {"local":{}}
//! ```
//!
//! Anything refused is answered `{"error":"…"}`.

use std::{
    io::{self, Read},
    net::Ipv4Addr,
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// Version of the messages below.
pub const HELPER_PROTOCOL: u32 = 1;

/// Where the helper listens.
#[cfg(target_os = "macos")]
pub const SOCKET: &str = "/var/run/devshare/helper.sock";
#[cfg(not(target_os = "macos"))]
pub const SOCKET: &str = "/run/devshare/helper.sock";

/// Where the helper records the authorities it made the system trust, one
/// `<sha256> <uid>` per line. Readable by everyone: a guest checks there
/// whether its own is trusted.
pub const TRUSTED_CAS: &str = "/etc/devshare/trusted-cas";

/// How many authorities one user may have trusted at once: the current one
/// and the one it replaces, while it is renewed.
pub const MOST_CAS_PER_USER: usize = 2;

/// Whether the helper recorded `sha256` as trusted, in `record`'s text.
pub fn records(record: &str, sha256: &str) -> bool {
    record
        .lines()
        .any(|line| line.split_whitespace().next() == Some(sha256))
}

/// Upper bound of a message: 241 names of 253 characters fit, with room.
pub const MAX_MESSAGE: usize = 128 * 1024;

/// As many names as a session's network has addresses for them.
pub const MAX_NAMES: usize = 241;

/// The interface's MTU, which the guest's network stack is sized for.
pub const MTU: u16 = 1500;

/// `198.18.0.0/15`, reserved for network testing (RFC 2544) and never routed
/// on the internet. Nothing outside it is ever routed into an interface the
/// helper creates.
pub const TEST_NETWORK: (Ipv4Addr, u8) = (Ipv4Addr::new(198, 18, 0, 0), 15);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    Hello {
        protocol: u32,
    },
    Up(Up),
    Down {},
    /// Makes the system trust the device's own certificate authority. Its
    /// certificate only, in PEM; what is accepted is in
    /// [`crate::authority`].
    TrustCa {
        certificate: String,
    },
    /// Stops trusting an authority the same user had trusted.
    UntrustCa {
        sha256: String,
    },
    /// Points the names of this machine's own projects at itself, for as
    /// long as the connection stays open; none removes them.
    Local {
        names: Vec<String>,
    },
}

/// The interface a session needs, and the names to resolve through it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Up {
    pub names: Vec<Name>,
    /// The interface's own address.
    pub address: Ipv4Addr,
    /// Where the session's resolver answers, inside the interface's network.
    pub resolver: Ipv4Addr,
    /// The length of the network routed into the interface.
    pub prefix: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Name {
    pub name: String,
    pub address: Ipv4Addr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Reply {
    /// The answer to `hello`.
    Welcome {
        version: String,
        protocol: u32,
    },
    Outcome(Outcome),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Sent with the interface's descriptor.
    Up {
        interface: String,
    },
    Down {},
    Trusted {
        sha256: String,
    },
    Untrusted {},
    Local {},
    Error(String),
}

/// Whether `address` is inside the network of `network/prefix`.
pub fn contains(network: Ipv4Addr, prefix: u8, address: Ipv4Addr) -> bool {
    let mask = mask(prefix);
    u32::from(network) & mask == u32::from(address) & mask
}

/// The netmask of a prefix length, as a number: `24` is `255.255.255.0`.
pub fn mask(prefix: u8) -> u32 {
    match prefix {
        0 => 0,
        prefix => u32::MAX << (32 - u32::from(prefix.min(32))),
    }
}

/// A message as it goes on the socket.
pub fn encode<T: Serialize>(message: &T) -> Vec<u8> {
    // Serializing these types cannot fail: no map has keys that are not
    // strings, and nothing is a custom serializer.
    let body = serde_json::to_vec(message).unwrap_or_default();
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    frame
}

/// Reads one message, refusing any larger than `max`.
pub fn read<T: DeserializeOwned>(reader: &mut impl Read, max: usize) -> io::Result<T> {
    let mut length = [0u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > max {
        return Err(too_long(length, max));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// The first message in `buffer`, if it is all there, and how many bytes it
/// took. For a reader that cannot use [`read`], because the bytes come with
/// a descriptor.
pub fn decode<T: DeserializeOwned>(buffer: &[u8], max: usize) -> io::Result<Option<(T, usize)>> {
    let Some(length) = buffer.get(..4) else {
        return Ok(None);
    };
    let length = u32::from_be_bytes([length[0], length[1], length[2], length[3]]) as usize;
    if length > max {
        return Err(too_long(length, max));
    }
    let Some(body) = buffer.get(4..4 + length) else {
        return Ok(None);
    };
    let message = serde_json::from_slice(body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(Some((message, 4 + length)))
}

fn too_long(length: usize, max: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("message of {length} bytes exceeds the limit of {max}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_messages_look_as_documented() {
        let json = |request: &Request| serde_json::to_string(request).unwrap();
        assert_eq!(
            json(&Request::Hello { protocol: 1 }),
            r#"{"hello":{"protocol":1}}"#
        );
        assert_eq!(json(&Request::Down {}), r#"{"down":{}}"#);
        let up = Request::Up(Up {
            names: vec![Name {
                name: "shop.test".into(),
                address: Ipv4Addr::new(198, 18, 90, 10),
            }],
            address: Ipv4Addr::new(198, 18, 90, 1),
            resolver: Ipv4Addr::new(198, 18, 90, 2),
            prefix: 24,
        });
        assert_eq!(
            json(&up),
            r#"{"up":{"names":[{"name":"shop.test","address":"198.18.90.10"}],"address":"198.18.90.1","resolver":"198.18.90.2","prefix":24}}"#
        );

        let reply = |text: &str| serde_json::from_str::<Reply>(text).unwrap();
        assert_eq!(
            reply(r#"{"version":"0.1.0","protocol":1}"#),
            Reply::Welcome {
                version: "0.1.0".into(),
                protocol: 1
            }
        );
        assert_eq!(
            reply(r#"{"up":{"interface":"utun5"}}"#),
            Reply::Outcome(Outcome::Up {
                interface: "utun5".into()
            })
        );
        assert_eq!(
            reply(r#"{"error":"no"}"#),
            Reply::Outcome(Outcome::Error("no".into()))
        );
    }

    #[test]
    fn frames_round_trip_and_the_limit_holds() {
        let frame = encode(&Request::Down {});
        let back: Request = read(&mut frame.as_slice(), 64).unwrap();
        assert_eq!(back, Request::Down {});
        assert!(read::<Request>(&mut frame.as_slice(), 4).is_err());

        assert!(decode::<Request>(&frame[..frame.len() - 1], 64)
            .unwrap()
            .is_none());
        let mut two = frame.clone();
        two.extend_from_slice(&frame);
        let (_, used) = decode::<Request>(&two, 64).unwrap().unwrap();
        assert_eq!(used, frame.len());
    }

    #[test]
    fn networks() {
        let (block, prefix) = TEST_NETWORK;
        assert!(contains(block, prefix, Ipv4Addr::new(198, 19, 255, 255)));
        assert!(!contains(block, prefix, Ipv4Addr::new(198, 20, 0, 1)));
        assert!(!contains(block, prefix, Ipv4Addr::new(10, 0, 0, 1)));
        assert_eq!(Ipv4Addr::from(mask(24)), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(mask(0), 0);
    }
}
