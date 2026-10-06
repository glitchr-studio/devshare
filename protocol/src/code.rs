//! Invitation codes: eight characters shown as `7GX2-KLM9`.

/// No `0`, `1`, `I` or `O`: the code is read aloud and typed by hand.
pub const ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";

pub const LENGTH: usize = 8;

/// Builds a code from random bytes, one character per byte.
pub fn from_random(bytes: [u8; LENGTH]) -> String {
    bytes
        .iter()
        .map(|byte| ALPHABET[(byte & 31) as usize] as char)
        .collect()
}

/// A code as the five bytes it stands for: eight characters of five bits.
pub fn to_bytes(code: &str) -> Option<[u8; 5]> {
    if code.len() != LENGTH {
        return None;
    }
    let mut bits: u64 = 0;
    for character in code.bytes() {
        let value = ALPHABET.iter().position(|known| *known == character)?;
        bits = (bits << 5) | value as u64;
    }
    let bytes = bits.to_be_bytes();
    Some([bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]])
}

/// The code five bytes stand for.
pub fn from_bytes(bytes: [u8; 5]) -> String {
    let bits = u64::from_be_bytes([0, 0, 0, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4]]);
    (0..LENGTH)
        .map(|index| ALPHABET[((bits >> (5 * (LENGTH - 1 - index))) & 31) as usize] as char)
        .collect()
}

/// `7GX2KLM9` → `7GX2-KLM9`.
pub fn display(code: &str) -> String {
    match code.len() {
        LENGTH => format!("{}-{}", &code[..4], &code[4..]),
        _ => code.to_string(),
    }
}

/// Extracts the code from whatever the guest pasted: the bare code, with or
/// without dash, a `devshare://join/…` link or an `https://…/…` link.
pub fn parse(input: &str) -> Option<String> {
    let input = input.trim();
    let tail = input
        .split(['?', '#'])
        .next()?
        .trim_end_matches('/')
        .rsplit('/')
        .next()?;
    let code: String = tail
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let valid = code.len() == LENGTH && code.bytes().all(|b| ALPHABET.contains(&b));
    valid.then_some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_transport_of_an_invitation() {
        for input in [
            "7GX2-KLM9",
            "7gx2klm9",
            " 7GX2 KLM9 ",
            "devshare://join/7GX2-KLM9",
            "https://join.devshare.example/7GX2-KLM9",
            "https://join.devshare.example/7GX2-KLM9/?utm=x",
        ] {
            assert_eq!(parse(input).as_deref(), Some("7GX2KLM9"), "{input}");
        }
    }

    #[test]
    fn rejects_what_is_not_a_code() {
        for input in [
            "",
            "7GX2-KLM",
            "7GX2-KLM90",
            "7GX2-KLM0",
            "https://example.com/",
        ] {
            assert_eq!(parse(input), None, "{input}");
        }
    }

    #[test]
    fn a_code_is_five_bytes_and_back() {
        for code in ["7GX2KLM9", "22222222", "ZZZZZZZZ", "KB5WXR8N"] {
            assert_eq!(from_bytes(to_bytes(code).unwrap()), code);
        }
        assert_eq!(to_bytes("7GX2-KLM9"), None);
        assert_eq!(to_bytes("7GX2KLM0"), None);
    }

    #[test]
    fn displays_in_two_groups() {
        assert_eq!(display(&from_random([0; 8])), "2222-2222");
    }
}
