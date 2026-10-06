//! An invitation as something to click or to scan: the link, and the QR
//! code of that link for a terminal or for a window.

use devshare_protocol::code;
use qrcode::{
    render::{svg, unicode::Dense1x2},
    EcLevel, QrCode,
};

/// How much damage a code survives. The lowest level: a code on a screen is
/// not scratched or torn, and a smaller code is easier to scan.
const CORRECTION: EcLevel = EcLevel::L;

/// The invitation as a web address: `https://join.example/7GX2-KLM9`.
///
/// It is the control plane's own address followed by the code, so that any
/// device can do something with it: a browser opens the page the control
/// plane serves there, and `devshare join` reads from it both the code and
/// which control plane to ask.
///
/// A link is for another device, so it never says `localhost`: a control
/// plane on this machine is named by this machine's address on the network.
pub fn link(server: &str, invitation: &str) -> String {
    format!(
        "{}/{}",
        for_other_devices(server).trim_end_matches('/'),
        code::display(invitation)
    )
}

/// The control plane an invitation link points at, when it is a link.
pub fn server_of(invitation: &str) -> Option<String> {
    let (scheme, rest) = invitation.trim().split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    (!authority.is_empty()).then(|| format!("{scheme}://{authority}"))
}

/// `server` as another device must write it: this machine's address on the
/// network instead of a name that means "myself" everywhere.
fn for_other_devices(server: &str) -> String {
    let Some((scheme, rest)) = server.split_once("://") else {
        return server.to_string();
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    // `host:port`, the host possibly an IPv6 address in brackets.
    let port_at = match authority.rfind(']') {
        Some(bracket) => authority[bracket..].find(':').map(|colon| bracket + colon),
        None => authority.rfind(':'),
    };
    let (host, port) = authority.split_at(port_at.unwrap_or(authority.len()));
    if !is_this_machine(host) {
        return server.to_string();
    }
    match network_address() {
        Some(address) => format!("{scheme}://{address}{port}{path}"),
        None => server.to_string(),
    }
}

fn is_this_machine(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "0.0.0.0")
}

/// Whether `server` is a control plane on this very machine: one that only
/// devices of this network can reach, if that.
pub fn on_this_machine(server: &str) -> bool {
    let rest = server.split_once("://").map_or(server, |(_, rest)| rest);
    let authority = rest.split('/').next().unwrap_or_default();
    let host = match authority.rfind(']') {
        Some(bracket) => &authority[..=bracket],
        None => authority
            .rsplit_once(':')
            .map_or(authority, |(host, _)| host),
    };
    is_this_machine(host)
}

/// The address other devices of the network reach this machine at: the one
/// its traffic to the outside leaves from. No packet is sent to find it.
fn network_address() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let address = socket.local_addr().ok()?.ip();
    (!address.is_loopback() && !address.is_unspecified()).then_some(address)
}

/// The QR code of `link` as an SVG image, black on white whatever the
/// colours around it: that is what a camera reads everywhere.
pub fn svg(link: &str) -> String {
    QrCode::with_error_correction_level(link.as_bytes(), CORRECTION)
        .map(|code| {
            code.render::<svg::Color>()
                .min_dimensions(168, 168)
                .dark_color(svg::Color("#000000"))
                .light_color(svg::Color("#ffffff"))
                .build()
        })
        .unwrap_or_default()
}

/// The QR code of `link` as text, two modules to a character.
///
/// With `colours`, each line sets black on white itself: the code is dark on
/// light on any terminal. Without (a file, a pipe), nothing can set the
/// colours, so the blocks are the light modules: right on a dark background,
/// which is what a terminal usually is.
pub fn terminal(link: &str, colours: bool) -> String {
    let Ok(code) = QrCode::with_error_correction_level(link.as_bytes(), CORRECTION) else {
        return String::new();
    };
    if !colours {
        return code
            .render::<Dense1x2>()
            .dark_color(Dense1x2::Light)
            .light_color(Dense1x2::Dark)
            .build();
    }
    code.render::<Dense1x2>()
        .build()
        .lines()
        .map(|line| format!("\x1b[30;107m{line}\x1b[0m"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reads back the QR codes this module draws, as a camera would: for tests.
#[doc(hidden)]
pub mod reading {
    /// Pixels per module: a decoder wants more than one.
    const SCALE: usize = 4;

    /// The text QR code as an image, one byte of grey per pixel, with its
    /// width. `blocks_are_dark` says how the characters are to be seen:
    /// true when the terminal draws them black on white.
    pub fn picture(text: &str, blocks_are_dark: bool) -> (Vec<u8>, usize) {
        let (block, space) = if blocks_are_dark {
            (0u8, 255u8)
        } else {
            (255u8, 0u8)
        };
        let mut rows: Vec<Vec<u8>> = Vec::new();
        for line in text.lines() {
            let line = strip_colours(line);
            let (mut upper, mut lower) = (Vec::new(), Vec::new());
            for character in line.chars() {
                let (top, bottom) = match character {
                    '\u{2588}' => (block, block),
                    '\u{2580}' => (block, space),
                    '\u{2584}' => (space, block),
                    _ => (space, space),
                };
                upper.extend(std::iter::repeat_n(top, SCALE));
                lower.extend(std::iter::repeat_n(bottom, SCALE));
            }
            for _ in 0..SCALE {
                rows.push(upper.clone());
            }
            for _ in 0..SCALE {
                rows.push(lower.clone());
            }
        }
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        let pixels = rows
            .into_iter()
            .flat_map(|mut row| {
                row.resize(width, space);
                row
            })
            .collect();
        (pixels, width)
    }

    /// A line without the escape sequences that colour it.
    pub fn strip_colours(line: &str) -> String {
        let mut plain = String::new();
        let mut characters = line.chars();
        while let Some(character) = characters.next() {
            if character == '\x1b' {
                // `ESC [ ... m`
                for skipped in characters.by_ref() {
                    if skipped == 'm' {
                        break;
                    }
                }
            } else {
                plain.push(character);
            }
        }
        plain
    }

    /// Whether a line of output is a line of a text QR code.
    pub fn is_code_line(line: &str) -> bool {
        let line = strip_colours(line);
        line.chars().count() > 20
            && line
                .chars()
                .all(|c| matches!(c, ' ' | '\u{2588}' | '\u{2580}' | '\u{2584}'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a decoder reads in a grey picture.
    fn read(pixels: &[u8], width: usize) -> String {
        let height = pixels.len() / width;
        let mut image = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| {
            pixels[y * width + x]
        });
        let grids = image.detect_grids();
        assert_eq!(grids.len(), 1, "one code is expected in the picture");
        grids[0].decode().unwrap().1
    }

    #[test]
    fn the_terminal_code_reads_back_as_the_link_in_both_modes() {
        let link = link("https://join.example", "7GX2KLM9");
        assert_eq!(link, "https://join.example/7GX2-KLM9");

        // Coloured: black blocks on the white the lines set themselves.
        let coloured = terminal(&link, true);
        assert!(coloured
            .lines()
            .all(|line| line.starts_with("\x1b[30;107m") && line.ends_with("\x1b[0m")));
        let (pixels, width) = reading::picture(&coloured, true);
        assert_eq!(read(&pixels, width), link);

        // Plain, on a dark terminal: the blocks are the light modules.
        let plain = terminal(&link, false);
        assert!(!plain.contains('\x1b'));
        assert!(plain.lines().all(reading::is_code_line));
        let (pixels, width) = reading::picture(&plain, false);
        assert_eq!(read(&pixels, width), link);
    }

    #[test]
    fn a_plain_code_seen_in_negative_is_not_what_is_drawn() {
        // The reason for the two modes: the same characters, seen on the
        // wrong background, are another picture.
        let plain = terminal(&link("https://join.example", "7GX2KLM9"), false);
        assert_ne!(
            reading::picture(&plain, false).0,
            reading::picture(&plain, true).0
        );
    }

    #[test]
    fn a_link_names_its_control_plane_and_never_localhost() {
        // The control plane's address, then the code.
        assert_eq!(
            link("https://join.example/", "7GX2KLM9"),
            "https://join.example/7GX2-KLM9"
        );
        assert_eq!(
            link("http://10.0.0.5:8787", "7GX2KLM9"),
            "http://10.0.0.5:8787/7GX2-KLM9"
        );

        // Read back: which control plane, and (by the protocol crate) which code.
        let pasted = "  http://10.0.0.5:8787/7GX2-KLM9?from=slack ";
        assert_eq!(server_of(pasted).as_deref(), Some("http://10.0.0.5:8787"));
        assert_eq!(code::parse(pasted).as_deref(), Some("7GX2KLM9"));
        assert_eq!(server_of("7GX2-KLM9"), None);
        assert_eq!(server_of("devshare://join/7GX2-KLM9"), None);

        // On this machine, the link carries this machine's address on the
        // network, with the same port: another device cannot use localhost.
        for local in [
            "http://localhost:8787",
            "http://127.0.0.1:8787",
            "http://[::1]:8787",
        ] {
            let link = link(local, "7GX2KLM9");
            assert!(link.ends_with(":8787/7GX2-KLM9"), "{link}");
            if network_address().is_some() {
                assert!(
                    !link.contains("localhost")
                        && !link.contains("127.0.0.1")
                        && !link.contains("::1"),
                    "{link}"
                );
            }
        }
    }

    #[test]
    fn a_control_plane_on_this_machine_is_recognised() {
        for here in [
            "http://localhost:8787",
            "http://127.0.0.1:8787/",
            "http://[::1]:8787",
            "http://localhost",
        ] {
            assert!(on_this_machine(here), "{here}");
        }
        for elsewhere in [
            "https://join.example",
            "http://10.0.0.5:8787",
            "https://localhost.example:8787",
        ] {
            assert!(!on_this_machine(elsewhere), "{elsewhere}");
        }
    }
}
