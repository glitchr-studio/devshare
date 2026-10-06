//! The host's own address, carried inside an invitation, for a guest that
//! cannot reach the control plane: one on another network, when the control
//! plane runs on the host's machine. It rides at the end of the invitation
//! link, so there is one invitation whatever the guest's network.
//!
//! Nothing has to be open on the host for it: the address is the host's
//! identity on the peer-to-peer link and the relay it is reachable through,
//! and both sides only ever connect outwards. It is longer than a code, so
//! it is copied rather than typed.
//!
//! PROVISIONAL, like the code it wraps: the invitation scheme has not had
//! its security review yet.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use devshare_protocol::code;
use iroh::{EndpointAddr, EndpointId, RelayUrl, TransportAddr};

/// What such an invitation starts with when it stands alone.
pub const PREFIX: &str = "dsh1";

// The address is written as a few bytes rather than as text: it ends up in
// a QR code, where every character costs.
//
//   1 byte    the format, and whether a code follows
//   32 bytes  the host's identity
//   5 bytes   the code, when it is not said elsewhere
//   then, for each way to reach the host, a kind and what it needs:
//     a relay of the default family, by its short name;
//     any other relay, by its address;
//     an IPv4 or IPv6 address and a port.
const FORMAT: u8 = 1;
const WITH_CODE: u8 = 0x80;
const DEFAULT_RELAY: u8 = 1;
const RELAY: u8 = 2;
const IPV4: u8 = 4;
const IPV6: u8 = 6;
/// `https://aps1-1.relay.n0.iroh.link./` is written `aps1-1`.
const DEFAULT_RELAYS: (&str, &str) = ("https://", ".relay.n0.iroh.link./");

fn bytes(host: &EndpointAddr, code: Option<&str>) -> Vec<u8> {
    let code = code.and_then(code::to_bytes);
    let mut out = vec![FORMAT | if code.is_some() { WITH_CODE } else { 0 }];
    out.extend_from_slice(host.id.as_bytes());
    out.extend(code.iter().flatten());
    for address in &portable(host).addrs {
        match address {
            TransportAddr::Relay(url) => {
                let url = url.to_string();
                let short = url
                    .strip_prefix(DEFAULT_RELAYS.0)
                    .and_then(|rest| rest.strip_suffix(DEFAULT_RELAYS.1));
                let (kind, text) = match short {
                    Some(short) => (DEFAULT_RELAY, short),
                    None => (RELAY, url.as_str()),
                };
                if let Ok(length) = u8::try_from(text.len()) {
                    out.extend([kind, length]);
                    out.extend_from_slice(text.as_bytes());
                }
            }
            TransportAddr::Ip(SocketAddr::V4(address)) => {
                out.push(IPV4);
                out.extend(address.ip().octets());
                out.extend(address.port().to_be_bytes());
            }
            TransportAddr::Ip(SocketAddr::V6(address)) => {
                out.push(IPV6);
                out.extend(address.ip().octets());
                out.extend(address.port().to_be_bytes());
            }
            _ => {}
        }
    }
    out
}

fn read(bytes: &[u8]) -> Option<(EndpointAddr, Option<String>)> {
    let (&first, rest) = bytes.split_first()?;
    if first & !WITH_CODE != FORMAT {
        return None;
    }
    let (id, mut rest) = rest.split_at_checked(32)?;
    let id = EndpointId::from_bytes(id.try_into().ok()?).ok()?;
    let mut code = None;
    if first & WITH_CODE != 0 {
        let (five, after) = rest.split_at_checked(5)?;
        code = Some(code::from_bytes(five.try_into().ok()?));
        rest = after;
    }

    let mut host = EndpointAddr::new(id);
    while let Some((&kind, after)) = rest.split_first() {
        let take = |length: usize| after.split_at_checked(length);
        rest = match kind {
            DEFAULT_RELAY | RELAY => {
                let (&length, after) = after.split_first()?;
                let (text, after) = after.split_at_checked(length as usize)?;
                let text = std::str::from_utf8(text).ok()?;
                let url = match kind {
                    DEFAULT_RELAY => format!("{}{text}{}", DEFAULT_RELAYS.0, DEFAULT_RELAYS.1),
                    _ => text.to_string(),
                };
                host = host.with_relay_url(url.parse::<RelayUrl>().ok()?);
                after
            }
            IPV4 => {
                let (data, after) = take(6)?;
                let ip = Ipv4Addr::new(data[0], data[1], data[2], data[3]);
                let port = u16::from_be_bytes([data[4], data[5]]);
                host = host.with_ip_addr(SocketAddr::new(IpAddr::V4(ip), port));
                after
            }
            IPV6 => {
                let (data, after) = take(18)?;
                let ip = Ipv6Addr::from(<[u8; 16]>::try_from(&data[..16]).ok()?);
                let port = u16::from_be_bytes([data[16], data[17]]);
                host = host.with_ip_addr(SocketAddr::new(IpAddr::V6(ip), port));
                after
            }
            _ => return None,
        };
    }
    Some((host, code))
}

/// The invitation `code` of the host reachable at `host`, standing alone.
pub fn encode(host: &EndpointAddr, code: &str) -> String {
    format!(
        "{PREFIX}{}",
        URL_SAFE_NO_PAD.encode(bytes(host, Some(code)))
    )
}

/// The address of `host` as it rides at the end of an invitation link,
/// after the `#`. The link says the code itself.
pub fn for_link(host: &EndpointAddr) -> String {
    URL_SAFE_NO_PAD.encode(bytes(host, None))
}

/// The address of `host` and the code together, for a link to a public page
/// that is the same page for every invitation: everything is after the `#`.
pub fn for_page(host: &EndpointAddr, code: &str) -> String {
    URL_SAFE_NO_PAD.encode(bytes(host, Some(code)))
}

/// The host's address and the code that `text` carries: standing alone, or
/// at the end of an invitation link. `None` when it carries no address;
/// `Some(Err(()))` when it stands alone and cannot be read.
pub fn decode(text: &str) -> Option<Result<(EndpointAddr, String), ()>> {
    let text = text.trim();
    if let Some(encoded) = text.strip_prefix(PREFIX) {
        let alone = URL_SAFE_NO_PAD
            .decode(encoded)
            .ok()
            .and_then(|bytes| read(&bytes))
            .and_then(|(host, code)| Some((host, code?)));
        return Some(alone.ok_or(()));
    }
    // In a link: what follows the `#`, if it is an address at all, with the
    // code the link says. Anything else there is none of our business.
    let (link, carried) = text.rsplit_once('#')?;
    let (host, code) = read(&URL_SAFE_NO_PAD.decode(carried).ok()?)?;
    Some(Ok((host, code.or_else(|| code::parse(link))?)))
}

/// Whether a guest on another network can reach `host`: only through a relay.
pub fn crosses_networks(host: &EndpointAddr) -> bool {
    host.relay_urls().next().is_some()
}

/// The part of an address that means something elsewhere. With a relay, the
/// relay is enough and the host's local addresses stay at home: the link
/// finds a direct route by itself once it is up. Without one, they are all
/// there is.
fn portable(host: &EndpointAddr) -> EndpointAddr {
    if !crosses_networks(host) {
        return host.clone();
    }
    let relays = host
        .addrs
        .iter()
        .filter(|address| matches!(address, TransportAddr::Relay(_)))
        .cloned();
    EndpointAddr::from_parts(host.id, relays)
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    fn host() -> EndpointAddr {
        let id = SecretKey::from_bytes(&[7; 32]).public();
        EndpointAddr::new(id)
            .with_ip_addr("192.168.1.20:51000".parse().unwrap())
            .with_ip_addr("[fd00::3]:51000".parse().unwrap())
    }

    #[test]
    fn it_reads_back_and_leaves_local_addresses_at_home_when_there_is_a_relay() {
        // No relay: the local addresses are all there is.
        let local = host();
        assert!(!crosses_networks(&local));
        let (read, code) = decode(&encode(&local, "7GX2KLM9")).unwrap().unwrap();
        assert_eq!((read, code.as_str()), (local, "7GX2KLM9"));

        // With a relay: the identity and the relay, nothing of the host's network.
        for relay in [
            "https://aps1-1.relay.n0.iroh.link./",
            "https://relay.glitchr.dev/",
        ] {
            let relayed = host().with_relay_url(relay.parse().unwrap());
            assert!(crosses_networks(&relayed));
            let invitation = encode(&relayed, "7GX2KLM9");
            assert!(invitation.starts_with("dsh1"));
            let (read, _) = decode(&format!("  {invitation}\n")).unwrap().unwrap();
            assert_eq!(read.id, relayed.id);
            assert_eq!(read.ip_addrs().count(), 0);
            assert_eq!(
                read.relay_urls()
                    .map(|url| url.to_string())
                    .collect::<Vec<_>>(),
                [relay]
            );
        }
    }

    #[test]
    fn at_the_end_of_a_link_it_is_short_and_takes_the_code_from_the_link() {
        let relayed = host().with_relay_url("https://aps1-1.relay.n0.iroh.link./".parse().unwrap());
        let carried = for_link(&relayed);
        // An identity, a relay by its short name, and nothing said twice.
        assert!(carried.len() <= 56, "{} characters", carried.len());

        let link = format!("http://10.0.0.5:8787/7GX2-KLM9#{carried}");
        let (read, code) = decode(&link).unwrap().unwrap();
        assert_eq!(read.id, relayed.id);
        assert_eq!(code, "7GX2KLM9");

        // A link without an address carries none, whatever follows its `#`.
        assert!(decode("http://10.0.0.5:8787/7GX2-KLM9").is_none());
        assert!(decode("http://10.0.0.5:8787/7GX2-KLM9#top").is_none());
        assert!(decode(&link[..link.len() - 9]).is_none());
    }

    #[test]
    fn a_link_to_a_public_page_says_everything_after_the_hash() {
        let relayed = host().with_relay_url("https://relay.glitchr.dev/".parse().unwrap());
        let link = format!(
            "https://join.glitchr.dev/#{}",
            for_page(&relayed, "7GX2KLM9")
        );
        let (read, code) = decode(&link).unwrap().unwrap();
        assert_eq!(read.id, relayed.id);
        assert_eq!(read.relay_urls().count(), 1);
        assert_eq!(code, "7GX2KLM9");
        // No code anywhere: not an invitation.
        let bare = format!("https://join.glitchr.dev/#{}", for_link(&relayed));
        assert!(decode(&bare).is_none());
    }

    #[test]
    fn what_is_not_one_is_told_from_one_that_is_broken() {
        assert!(decode("7GX2-KLM9").is_none());
        assert_eq!(
            decode("dsh1not-an-invitation").map(|read| read.is_err()),
            Some(true)
        );
        assert_eq!(decode("dsh1").map(|read| read.is_err()), Some(true));
        // Alone, it must say its code: an address is not an invitation.
        let alone = format!("dsh1{}", for_link(&host()));
        assert_eq!(decode(&alone).map(|read| read.is_err()), Some(true));
    }
}
