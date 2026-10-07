//! Servers a project runs on the machine itself, outside Docker: Vite's dev
//! server and the Symfony CLI's local web server. Read from their
//! configuration files, like the compose file, never by asking what runs.

use std::path::Path;

use super::Published;

const VITE_CONFIGS: [&str; 6] = [
    "vite.config.ts",
    "vite.config.js",
    "vite.config.mts",
    "vite.config.mjs",
    "vite.config.cts",
    "vite.config.cjs",
];
const VITE_PORT: u16 = 5173;
const SYMFONY_PORT: u16 = 8000;

/// What a local server publishes, and what to tell the developer about it.
pub(super) struct Local {
    pub source: String,
    pub port: Published,
    pub notes: Vec<String>,
}

/// Vite's dev server, when the project has a Vite configuration.
pub(super) fn vite(directory: &Path, hostname: &str) -> Option<Local> {
    let (source, config) = VITE_CONFIGS.iter().find_map(|name| {
        std::fs::read_to_string(directory.join(name))
            .ok()
            .map(|text| (name.to_string(), text))
    })?;
    let server = object(&config, "server").unwrap_or_default();
    let scripts = std::fs::read_to_string(directory.join("package.json")).unwrap_or_default();
    let port = number_after(server, "port")
        .or_else(|| flag_port(&scripts))
        .unwrap_or(VITE_PORT);

    let mut notes = Vec::new();
    if !server.contains("strictPort") {
        notes.push(format!(
            "Vite moves to the next port when {port} is taken: set server.strictPort in \
             {source} so that guests always find it."
        ));
    }
    // Since 6.0.9, Vite refuses a Host it was not told about.
    let allowed = object(server, "allowedHosts").is_some()
        || server.contains("allowedHosts: true")
        || server.contains(hostname)
        || server.contains(&format!(".{}", tld(hostname)));
    if !allowed {
        notes.push(format!(
            "Vite refuses requests for names it does not know: add \"{hostname}\" to \
             server.allowedHosts in {source}, or guests get \"Blocked request\"."
        ));
    }
    Some(Local {
        source,
        port: Published {
            service: "vite".into(),
            container: port,
            host: Some(port),
            // Vite listens on `localhost`, which can be IPv6 only.
            address: Some("localhost".into()),
            udp: false,
            left_out: None,
        },
        notes,
    })
}

/// The Symfony CLI's web server, for a Symfony project.
pub(super) fn symfony(directory: &Path) -> Option<Local> {
    let symfony =
        directory.join("symfony.lock").is_file() || directory.join("bin/console").is_file();
    if !symfony {
        return None;
    }
    let settings = [".symfony.local.yaml", ".symfony.local.yml"]
        .iter()
        .find_map(|name| {
            std::fs::read_to_string(directory.join(name))
                .ok()
                .map(|text| (name.to_string(), text))
        });
    let pinned = settings.as_ref().and_then(|(_, text)| {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(text).ok()?;
        let port = value.get("http")?.get("port")?;
        port.as_u64()
            .or_else(|| port.as_str().and_then(|port| port.parse().ok()))
            .and_then(|port| u16::try_from(port).ok())
    });
    let mut notes = Vec::new();
    if pinned.is_none() {
        notes.push(format!(
            "symfony server:start takes the next free port when {SYMFONY_PORT} is taken: pin it \
             with http.port in .symfony.local.yaml so that guests always find it."
        ));
    }
    Some(Local {
        source: settings
            .map(|(name, _)| name)
            .unwrap_or_else(|| "the Symfony CLI's defaults".into()),
        port: Published {
            service: "symfony".into(),
            container: pinned.unwrap_or(SYMFONY_PORT),
            host: Some(pinned.unwrap_or(SYMFONY_PORT)),
            address: None,
            udp: false,
            left_out: None,
        },
        notes,
    })
}

/// The text of the object that follows `key:` in JavaScript or TypeScript,
/// braces included: `server: { port: 3000 }`.
fn object<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = text;
    while let Some(found) = rest.find(key) {
        let after = &rest[found + key.len()..];
        rest = after;
        let Some(value) = after.trim_start().strip_prefix(':') else {
            continue;
        };
        let value = value.trim_start();
        if !value.starts_with(['{', '[']) {
            continue;
        }
        let (open, close) = if value.starts_with('{') {
            ('{', '}')
        } else {
            ('[', ']')
        };
        let mut depth = 0usize;
        for (index, character) in value.char_indices() {
            if character == open {
                depth += 1;
            } else if character == close {
                depth -= 1;
                if depth == 0 {
                    return Some(&value[..=index]);
                }
            }
        }
        return None;
    }
    None
}

/// The number after `key:` in `text`.
fn number_after(text: &str, key: &str) -> Option<u16> {
    let mut rest = text;
    while let Some(found) = rest.find(key) {
        let before = &rest[..found];
        rest = &rest[found + key.len()..];
        // `strictPort` and `hmr.clientPort` are other settings.
        if before.ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let Some(value) = rest.trim_start().strip_prefix(':') else {
            continue;
        };
        let digits: String = value
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(port) = digits.parse() {
            return Some(port);
        }
    }
    None
}

/// `--port 3000` or `--port=3000` in the scripts of `package.json`.
fn flag_port(text: &str) -> Option<u16> {
    let at = text.find("--port")?;
    let digits: String = text[at + "--port".len()..]
        .trim_start_matches([' ', '='])
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// `test` of `shop.test`.
fn tld(hostname: &str) -> &str {
    hostname.rsplit('.').next().unwrap_or(hostname)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_port_of_vite_s_server_and_not_another() {
        let config = "export default defineConfig({\n  preview: { port: 4173 },\n  server: {\n    strictPort: true,\n    hmr: { clientPort: 443 },\n    port: 3000,\n  },\n})";
        let server = object(config, "server").unwrap();
        assert!(server.starts_with('{') && server.ends_with('}'));
        assert_eq!(number_after(server, "port"), Some(3000));
        assert_eq!(object("export default { plugins: [] }", "server"), None);
        assert_eq!(
            flag_port(r#"{"scripts": {"dev": "vite --port=4000"}}"#),
            Some(4000)
        );
        assert_eq!(
            flag_port(r#"{"scripts": {"dev": "vite --port 4001"}}"#),
            Some(4001)
        );
        assert_eq!(flag_port(r#"{"scripts": {"dev": "vite"}}"#), None);
    }
}
