//! What a host declares: the environments of a project, in the project's
//! own folder, and the general settings common to all of this user's
//! projects.
//!
//! A declaration knows where each service really listens (`target`). The
//! manifest sent to guests does not.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use devshare_protocol::{normalize_host, Environment, Service, Transport};
use serde::{Deserialize, Serialize};

/// The file of a project's environments, in the project's folder.
pub const PROJECT_FILE: &str = "devshare.toml";

/// Where the control plane is looked for when nothing says otherwise.
pub const DEFAULT_SERVER: &str = "http://localhost:8787";
pub const DEFAULT_DURATION: Duration = Duration::from_secs(5 * 60);
pub const DEFAULT_GUESTS: u32 = 3;
/// Where invitation links and QR codes point unless told otherwise: the
/// public invitation page, the file `docs/index.html` of this repository.
pub const DEFAULT_JOIN: &str = "https://join.glitchr.dev";
/// Guests reach a project as `<project>.test` unless told otherwise.
pub const DEFAULT_DOMAIN: &str = "test";

/// General settings, common to every project of this user. This file is the
/// user's: DevShare writes in it only when its owner changes a setting in
/// the app, and then only that setting, comments kept. Nothing about one
/// project in particular belongs there.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// URL of the control plane.
    #[serde(default)]
    pub server: Option<String>,
    /// How long a session lasts unless asked otherwise: `90s`, `15m`, `1h`.
    #[serde(default)]
    pub duration: Option<String>,
    /// How many guests a session accepts at once unless asked otherwise.
    #[serde(default)]
    pub guests: Option<u32>,
    /// What comes after the project's name in the hostname guests use.
    #[serde(default)]
    pub domain: Option<String>,
    /// The relay sessions go through: the address of a relay of one's own,
    /// or `disabled` for direct connections only. The public relays of the
    /// iroh project when it is not set.
    #[serde(default)]
    pub relay: Option<String>,
    /// The public address of the invitation page. Invitation links and QR
    /// codes point there, so that a device on any network can open them;
    /// empty, they point at the control plane instead.
    #[serde(default)]
    pub join: Option<String>,
    /// Where the desktop app looks for projects. The usual folders under
    /// the home folder (`~/Sites`, `~/Projects`…) when it is not set.
    #[serde(default)]
    pub folders: Option<Vec<String>>,
}

/// Removes `key` from a settings file. The comments written above it are
/// its owner's: they go to the key that followed, or to the end of the file.
fn remove_keeping_comments(document: &mut toml_edit::DocumentMut, key: &str) {
    let keys: Vec<String> = document.iter().map(|(name, _)| name.to_string()).collect();
    let Some(position) = keys.iter().position(|name| name == key) else {
        return;
    };
    let above = document
        .as_table()
        .key(key)
        .and_then(|key| key.leaf_decor().prefix())
        .and_then(|prefix| prefix.as_str())
        .unwrap_or_default()
        .to_string();
    document.remove(key);
    if !above.contains('#') {
        return;
    }
    match keys.get(position + 1) {
        Some(next) => {
            if let Some(mut next) = document.as_table_mut().key_mut(next) {
                let own = next
                    .leaf_decor()
                    .prefix()
                    .and_then(|prefix| prefix.as_str())
                    .unwrap_or_default()
                    .to_string();
                next.leaf_decor_mut().set_prefix(format!("{above}{own}"));
            }
        }
        None => {
            let trailing = document.trailing().as_str().unwrap_or_default().to_string();
            document.set_trailing(format!("{above}{trailing}"));
        }
    }
}

impl Settings {
    /// What `devshare settings --init` writes: every setting, commented
    /// out, so the file changes nothing until its owner edits it.
    pub const TEMPLATE: &'static str = "\
# General settings of DevShare, common to all your projects.
# The environments of a project are in the project's own folder, in the
# devshare.toml that `devshare discover` writes there.

# The control plane sessions are announced on.
# server = \"http://localhost:8787\"

# How long a session lasts unless you ask otherwise: 90s, 15m, 1h.
# duration = \"5m\"

# How many guests a session accepts at once unless you ask otherwise.
# guests = 3

# What comes after a project's name in the hostname guests use:
# a project named shop is reached as shop.test.
# domain = \"test\"

# The relay sessions go through when the two machines cannot reach each
# other directly: a relay of your own, or \"disabled\" for none. The public
# relays of the iroh project are used when this is not set.
# relay = \"https://relay.example\"

# The public page invitation links and QR codes point at, so that they open
# from any network. Set it to \"\" to have them point at the control plane
# instead; the one a session starts itself answers on this machine only.
# join = \"https://join.glitchr.dev\"

# Where the desktop app looks for projects. The usual folders under your
# home folder (~/Sites, ~/Projects…) when this is not set.
# folders = [\"~/Sites\"]
";

    /// `DEVSHARE_SETTINGS` when it is set, else the configuration folder.
    pub fn file() -> PathBuf {
        std::env::var_os("DEVSHARE_SETTINGS")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default();
                home.join(".config/devshare/devshare.toml")
            })
    }

    /// The user's settings. No file means the defaults.
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::file())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Ok(Self::default());
        };
        let settings: Self = toml::from_str(&content).with_context(|| {
            format!(
                "{} holds general settings only (server, duration, guests, domain, relay, join, \
                 folders); \
                 the environments of a project belong in its own folder",
                path.display()
            )
        })?;
        // Said now rather than when a session starts.
        settings.duration()?;
        Ok(settings)
    }

    /// Writes these settings to the user's file. See [`Settings::save_to`].
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::file())
    }

    /// Writes these settings to `path`, changing nothing else in it: its
    /// comments and its layout stay. A setting that is `None` is removed,
    /// and its default applies. A new file starts from [`Settings::TEMPLATE`].
    pub fn save_to(&self, path: &Path) -> Result<()> {
        self.duration()?;
        if self.guests == Some(0) {
            bail!("a session accepts at least one guest");
        }
        if let Some(domain) = &self.domain {
            if !crate::protocol::names::is_hostname(domain.trim_matches('.')) {
                bail!("\"{}\" is not a domain", crate::protocol::clean(domain, 80));
            }
        }
        let existing = std::fs::read_to_string(path).unwrap_or_else(|_| Self::TEMPLATE.to_string());
        let mut document: toml_edit::DocumentMut = existing
            .parse()
            .with_context(|| format!("{} is not valid TOML", path.display()))?;
        let text =
            |value: &Option<String>| value.as_ref().map(|value| toml_edit::value(value.trim()));
        let values = [
            ("server", text(&self.server)),
            ("duration", text(&self.duration)),
            (
                "guests",
                self.guests
                    .map(|guests| toml_edit::value(i64::from(guests))),
            ),
            ("domain", text(&self.domain)),
            ("relay", text(&self.relay)),
            ("join", text(&self.join)),
            (
                "folders",
                self.folders.as_ref().map(|folders| {
                    toml_edit::value(
                        folders
                            .iter()
                            .map(|folder| folder.trim())
                            .collect::<toml_edit::Array>(),
                    )
                }),
            ),
        ];
        for (key, value) in values {
            match value {
                Some(value) => {
                    document[key] = value;
                }
                None => remove_keeping_comments(&mut document, key),
            }
        }
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)
                .with_context(|| format!("creating {}", folder.display()))?;
        }
        std::fs::write(path, document.to_string())
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn duration(&self) -> Result<Duration> {
        match &self.duration {
            Some(value) => duration(value).context("the duration of the general settings"),
            None => Ok(DEFAULT_DURATION),
        }
    }

    /// Where the desktop app looks for projects, `~` expanded; `None` for
    /// the usual folders.
    pub fn folders(&self) -> Option<Vec<PathBuf>> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        self.folders.as_ref().map(|folders| {
            folders
                .iter()
                .map(|folder| match folder.trim().strip_prefix("~/") {
                    Some(rest) => home.join(rest),
                    None => PathBuf::from(folder.trim()),
                })
                .collect()
        })
    }

    pub fn guests(&self) -> u32 {
        self.guests.unwrap_or(DEFAULT_GUESTS).max(1)
    }

    /// The public invitation page: `DEVSHARE_JOIN` when it is set, else the
    /// setting, else the default one. Set to nothing, there is none: links
    /// then point at the control plane.
    pub fn join(&self) -> Option<String> {
        std::env::var("DEVSHARE_JOIN")
            .ok()
            .or_else(|| self.join.clone())
            .or_else(|| Some(DEFAULT_JOIN.to_string()))
            .map(|join| join.trim().trim_end_matches('/').to_string())
            .filter(|join| !join.is_empty())
    }

    pub fn domain(&self) -> String {
        let domain = self.domain.as_deref().unwrap_or(DEFAULT_DOMAIN);
        domain.trim().trim_matches('.').to_ascii_lowercase()
    }
}

/// `90s`, `5m`, `1h`, or a number of seconds.
pub fn duration(value: &str) -> Result<Duration> {
    let value = value.trim();
    // Until the host stops it.
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "none" | "unlimited" | "0"
    ) {
        return Ok(Duration::from_secs(crate::protocol::NO_LIMIT));
    }
    let (number, unit) = match value.find(|c: char| !c.is_ascii_digit()) {
        Some(index) => value.split_at(index),
        None => (value, "s"),
    };
    let number: u64 = number
        .parse()
        .map_err(|_| anyhow!("expected a duration such as 90s, 5m or 1h, or none"))?;
    let seconds = match unit {
        "s" => number,
        "m" => number * 60,
        "h" => number * 3600,
        _ => bail!("expected a duration such as 90s, 5m or 1h, or none"),
    };
    if seconds == 0 {
        bail!("a session lasts at least one second");
    }
    Ok(Duration::from_secs(seconds))
}

/// The control plane to use: what was asked for explicitly, else what the
/// project names, else what the general settings name, else the default.
pub fn server(explicit: Option<String>, config: Option<&Config>, settings: &Settings) -> String {
    explicit
        .or_else(|| config.and_then(|config| config.server.clone()))
        .or_else(|| settings.server.clone())
        .unwrap_or_else(|| DEFAULT_SERVER.to_string())
}

/// What one project declares, in its own folder.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// URL of the control plane, when this project uses another one than
    /// the general settings say.
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub environments: BTreeMap<String, EnvironmentDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentDef {
    #[serde(default)]
    pub entrypoint: Option<String>,
    pub services: Vec<ServiceDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDef {
    pub host: String,
    pub port: u16,
    /// `address:port` the host agent dials. Defaults to `host:port` as the
    /// host machine resolves it.
    #[serde(default)]
    pub target: Option<String>,
}

/// The environments of one session: what guests see, and the only
/// `(hostname, port)` pairs the host agent will ever dial.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    pub environments: BTreeMap<String, Environment>,
    pub routes: HashMap<(String, u16), String>,
}

impl Config {
    /// The file that is named, else the one of the current folder.
    pub fn load(explicit: Option<PathBuf>) -> Result<Self> {
        let path = explicit.unwrap_or_else(|| PathBuf::from(PROJECT_FILE));
        if !path.is_file() {
            bail!(
                "no {}: run \"devshare discover\" in the project's folder to write it from the compose file",
                path.display()
            );
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&content).with_context(|| format!("parsing {}", path.display()))
    }

    /// The file of a project's folder.
    pub fn of(folder: &Path) -> Result<Self> {
        Self::load(Some(folder.join(PROJECT_FILE)))
    }

    /// Keeps the named environments, or all of them when none is named.
    pub fn select(&self, names: &[String]) -> Result<Selection> {
        let chosen: Vec<&String> = if names.is_empty() {
            self.environments.keys().collect()
        } else {
            names.iter().collect()
        };
        if chosen.is_empty() {
            bail!("no environment is declared");
        }

        let mut selection = Selection::default();
        for name in chosen {
            let Some(def) = self.environments.get(name) else {
                let known: Vec<&str> = self.environments.keys().map(String::as_str).collect();
                bail!(
                    "unknown environment \"{name}\" (declared: {})",
                    known.join(", ")
                );
            };
            if def.services.is_empty() {
                bail!("environment \"{name}\" declares no service");
            }

            let mut dns = Vec::new();
            let mut services = Vec::new();
            for service in &def.services {
                let host = normalize_host(&service.host);
                if host.is_empty() || host == "localhost" {
                    bail!(
                        "environment \"{name}\": \"{}\" cannot be shared, a guest would resolve it to itself",
                        service.host
                    );
                }
                let target = service
                    .target
                    .clone()
                    .unwrap_or_else(|| format!("{host}:{}", service.port));
                selection
                    .routes
                    .insert((host.clone(), service.port), target);
                if !dns.contains(&host) {
                    dns.push(host.clone());
                }
                services.push(Service {
                    host,
                    port: service.port,
                    protocol: Transport::Tcp,
                    tls: None,
                });
            }
            selection.environments.insert(
                name.clone(),
                Environment {
                    entrypoint: def.entrypoint.clone(),
                    dns,
                    services,
                },
            );
        }
        Ok(selection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        let def = |host: &str, port, target: Option<&str>| ServiceDef {
            host: host.into(),
            port,
            target: target.map(Into::into),
        };
        Config {
            server: None,
            environments: BTreeMap::from([
                (
                    "shop".to_string(),
                    EnvironmentDef {
                        entrypoint: Some("https://shop.test".into()),
                        services: vec![
                            def("Shop.test", 443, None),
                            def("api.shop.test", 8080, Some("127.0.0.1:9000")),
                        ],
                    },
                ),
                (
                    "infra".to_string(),
                    EnvironmentDef {
                        entrypoint: None,
                        services: vec![def("db.test", 5432, None)],
                    },
                ),
            ]),
        }
    }

    #[test]
    fn only_selected_services_become_routes() {
        let selection = config().select(&["shop".to_string()]).unwrap();

        assert_eq!(selection.routes.len(), 2);
        assert_eq!(
            selection.routes[&("shop.test".to_string(), 443)],
            "shop.test:443"
        );
        assert_eq!(
            selection.routes[&("api.shop.test".to_string(), 8080)],
            "127.0.0.1:9000"
        );
        assert!(!selection
            .routes
            .contains_key(&("db.test".to_string(), 5432)));
        assert_eq!(
            selection.environments["shop"].dns,
            ["shop.test", "api.shop.test"]
        );
    }

    #[test]
    fn refuses_unknown_environments_and_localhost() {
        assert!(config().select(&["nope".to_string()]).is_err());

        let mut config = config();
        config.environments.get_mut("infra").unwrap().services[0].host = "localhost".into();
        assert!(config.select(&[]).is_err());
    }

    #[test]
    fn general_settings_default_and_refuse_what_is_not_theirs() {
        let folder = std::env::temp_dir().join(format!("devshare-settings-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("devshare.toml");

        // No file: the defaults.
        let settings = Settings::load_from(&file).unwrap();
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.duration().unwrap(), DEFAULT_DURATION);
        assert_eq!((settings.guests(), settings.domain().as_str()), (3, "test"));

        // The template changes nothing until it is edited.
        std::fs::write(&file, Settings::TEMPLATE).unwrap();
        assert_eq!(Settings::load_from(&file).unwrap(), Settings::default());

        std::fs::write(
            &file,
            "server = \"https://join.example\"\nduration = \"15m\"\nguests = 5\ndomain = \".Lan\"\n",
        )
        .unwrap();
        let settings = Settings::load_from(&file).unwrap();
        assert_eq!(settings.duration().unwrap(), Duration::from_secs(900));
        assert_eq!((settings.guests(), settings.domain().as_str()), (5, "lan"));

        // Asked for, the project's, the general one, the default: in that order.
        let project = Config {
            server: Some("https://project.example".into()),
            ..Config::default()
        };
        assert_eq!(
            server(
                Some("https://flag.example".into()),
                Some(&project),
                &settings
            ),
            "https://flag.example"
        );
        assert_eq!(
            server(None, Some(&project), &settings),
            "https://project.example"
        );
        assert_eq!(
            server(None, Some(&Config::default()), &settings),
            "https://join.example"
        );
        assert_eq!(server(None, None, &Settings::default()), DEFAULT_SERVER);

        // The public invitation page: a setting, a default, or none at all.
        if std::env::var_os("DEVSHARE_JOIN").is_none() {
            assert_eq!(settings.join().as_deref(), Some(DEFAULT_JOIN));
            let own = Settings {
                join: Some(" https://join.example/ ".into()),
                ..Settings::default()
            };
            assert_eq!(own.join().as_deref(), Some("https://join.example"));
            let none = Settings {
                join: Some(String::new()),
                ..Settings::default()
            };
            assert_eq!(none.join(), None);
        }

        // Environments do not belong there, and a duration must be one.
        std::fs::write(&file, "[environments.shop]\nservices = []\n").unwrap();
        let refused = format!("{:#}", Settings::load_from(&file).unwrap_err());
        assert!(refused.contains("general settings only"), "{refused}");
        std::fs::write(&file, "duration = \"soon\"\n").unwrap();
        assert!(Settings::load_from(&file).is_err());

        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn saving_changes_the_settings_and_keeps_the_owner_s_comments() {
        let path =
            std::env::temp_dir().join(format!("devshare-settings-{}.toml", std::process::id()));
        std::fs::write(
            &path,
            "# my own note\nduration = \"5m\" # short\nrelay = \"disabled\"\n",
        )
        .unwrap();
        let settings = Settings {
            duration: Some("2h".into()),
            guests: Some(4),
            domain: Some("local".into()),
            relay: None,
            ..Settings::load_from(&path).unwrap()
        };
        settings.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my own note"), "{text}");
        assert!(text.contains("duration = \"2h\""), "{text}");
        assert!(!text.contains("relay"), "{text}");
        assert_eq!(Settings::load_from(&path).unwrap(), settings);

        let wrong = Settings {
            duration: Some("soon".into()),
            ..Settings::default()
        };
        assert!(wrong.save_to(&path).is_err());
        assert_eq!(
            Settings::load_from(&path).unwrap(),
            settings,
            "a refused change writes nothing"
        );

        // No file yet: the commented template, then the setting.
        std::fs::remove_file(&path).unwrap();
        Settings {
            guests: Some(2),
            ..Settings::default()
        }
        .save_to(&path)
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# How long a session lasts") && text.contains("guests = 2"),
            "{text}"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_session_can_have_no_time_limit() {
        for value in ["none", "Unlimited", "0"] {
            assert_eq!(
                duration(value).unwrap().as_secs(),
                crate::protocol::NO_LIMIT,
                "{value}"
            );
        }
        assert!(crate::protocol::unlimited(
            duration("none").unwrap().as_secs()
        ));
        assert!(!crate::protocol::unlimited(
            duration("24h").unwrap().as_secs()
        ));
    }
}
