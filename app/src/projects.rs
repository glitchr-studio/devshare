//! The projects of the app: the ones found in the folders where the owner
//! keeps projects, and the ones added by hand (a folder, or a devshare.toml
//! of any name), each switched on or off, or taken off the list. These
//! lists are the app's memory, kept with the app's data; the folders to
//! look in are a general setting.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use devshare_core::{
    discover,
    environment::{Config, Settings},
};
use serde::Serialize;

/// The projects added by hand: folders, or devshare.toml files.
const ADDED: &str = "projects.json";
/// The projects switched on.
const ON: &str = "selected.json";
/// The projects taken off the list.
const HIDDEN: &str = "hidden.json";
/// Projects with nothing to share that were put back on the list anyway.
const SHOWN: &str = "shown.json";
/// How the owner starts and stops some projects on this computer.
const COMMANDS: &str = "commands.json";

pub struct Projects {
    folder: PathBuf,
}

/// One project as the window lists it.
#[derive(Debug, Clone, Serialize)]
pub struct Project {
    /// Its folder, or the devshare.toml it was added as.
    pub folder: String,
    pub name: String,
    /// The name guests reach it under first.
    pub hostname: Option<String>,
    /// Every name and port it shares.
    pub names: Vec<String>,
    pub ports: Vec<u16>,
    /// Each shared name and port, with where this machine reaches it itself.
    pub addresses: Vec<Address>,
    /// Where this machine reaches the project's entry point itself, for a
    /// preview: `https://localhost:8633`.
    pub preview: Option<String>,
    pub on: bool,
    /// Added by hand, rather than found.
    pub added: bool,
    /// Whether the app knows how to start and stop it.
    pub startable: bool,
    /// The owner's own start and stop commands for it, on this computer.
    pub local_up: Option<String>,
    pub local_down: Option<String>,
    /// What starts and stops it without those: its devshare.toml, the
    /// owner's defaults, or what it is.
    pub usual_up: Option<String>,
    pub usual_down: Option<String>,
    /// Why it cannot be shared as it is.
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Address {
    pub host: String,
    pub port: u16,
    /// `http://localhost:8124`: the same port on this machine.
    pub local: String,
}

/// The projects as the window shows them, and the ones it does not.
#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    pub projects: Vec<Project>,
    /// Taken off the list, or with nothing to share: to put back.
    pub hidden: Vec<Hidden>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hidden {
    pub folder: String,
    pub name: String,
    /// Why it is not listed.
    pub why: String,
}

impl Projects {
    /// The lists kept in `folder`, the app's own data folder.
    pub fn in_folder(folder: PathBuf) -> Self {
        Self { folder }
    }

    fn read(&self, file: &str) -> Vec<PathBuf> {
        std::fs::read_to_string(self.folder.join(file))
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    fn write(&self, file: &str, paths: &[PathBuf]) -> Result<()> {
        std::fs::create_dir_all(&self.folder)
            .with_context(|| format!("creating {}", self.folder.display()))?;
        let path = self.folder.join(file);
        std::fs::write(&path, serde_json::to_string_pretty(paths)?)
            .with_context(|| format!("writing {}", path.display()))
    }

    fn put(&self, file: &str, path: &Path, present: bool) -> Result<()> {
        let mut paths = self.read(file);
        paths.retain(|known| known != path);
        if present {
            paths.push(path.to_path_buf());
        }
        self.write(file, &paths)
    }

    /// The projects added by hand.
    pub fn added(&self) -> Vec<PathBuf> {
        self.read(ADDED)
    }

    /// The projects switched on.
    pub fn switched_on(&self) -> Vec<PathBuf> {
        self.read(ON)
    }

    /// Adds a project by hand, switched on: a folder DevShare can read, or a
    /// devshare.toml, whatever its name and wherever it is.
    pub fn add(&self, path: &Path) -> Result<PathBuf> {
        let path = path
            .canonicalize()
            .with_context(|| format!("locating {}", path.display()))?;
        if path.is_file() {
            Config::load(Some(path.clone()))
                .with_context(|| format!("{} is not a DevShare configuration", path.display()))?;
        } else if !discover::is_project(&path) && !path.join(discover::FILE).is_file() {
            bail!(
                "{} is not a project DevShare can read: no compose file, Vite or Symfony \
                 configuration, nor devshare.toml",
                path.display()
            );
        }
        let mut added = self.added();
        if !added.contains(&path) {
            added.push(path.clone());
            self.write(ADDED, &added)?;
        }
        // Added by hand: listed again if it had been taken off, shown even
        // with nothing to share yet.
        self.put(HIDDEN, &path, false)?;
        self.put(SHOWN, &path, true)?;
        self.switch(&path, true)?;
        Ok(path)
    }

    /// Takes a project off the list, found or added, until it is put back.
    /// Nothing in its folder is touched.
    pub fn remove(&self, path: &Path) -> Result<()> {
        self.put(HIDDEN, path, true)?;
        self.put(SHOWN, path, false)?;
        self.switch(path, false)
    }

    /// Puts a project back on the list, even one with nothing to share.
    pub fn restore(&self, path: &Path) -> Result<()> {
        self.put(HIDDEN, path, false)?;
        self.put(SHOWN, path, true)
    }

    /// The owner's own start and stop commands for a project.
    pub fn local_commands(&self, path: &Path) -> discover::Commands {
        self.all_commands()
            .remove(&path.display().to_string())
            .unwrap_or_default()
    }

    fn all_commands(&self) -> std::collections::BTreeMap<String, discover::Commands> {
        std::fs::read_to_string(self.folder.join(COMMANDS))
            .ok()
            .and_then(|content| {
                serde_json::from_str::<
                    std::collections::BTreeMap<String, (Option<String>, Option<String>)>,
                >(&content)
                .ok()
            })
            .unwrap_or_default()
            .into_iter()
            .map(|(path, (up, down))| (path, discover::Commands { up, down }))
            .collect()
    }

    /// Sets them; empty ones go, and the usual ones apply again.
    pub fn set_local_commands(&self, path: &Path, commands: discover::Commands) -> Result<()> {
        let given = |value: Option<String>| {
            value
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let mut all: std::collections::BTreeMap<String, (Option<String>, Option<String>)> = self
            .all_commands()
            .into_iter()
            .map(|(path, commands)| (path, (commands.up, commands.down)))
            .collect();
        let (up, down) = (given(commands.up), given(commands.down));
        let key = path.display().to_string();
        if up.is_none() && down.is_none() {
            all.remove(&key);
        } else {
            all.insert(key, (up, down));
        }
        std::fs::create_dir_all(&self.folder)?;
        let file = self.folder.join(COMMANDS);
        std::fs::write(&file, serde_json::to_string_pretty(&all)?)
            .with_context(|| format!("writing {}", file.display()))
    }

    pub fn switch(&self, path: &Path, on: bool) -> Result<()> {
        self.put(ON, path, on)
    }

    /// Where projects are looked for: the folders of the settings, else the
    /// usual ones under the home folder (`~/Sites`, `~/Projects`…).
    pub fn roots(&self, settings: &Settings) -> Vec<PathBuf> {
        settings.folders().unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            discover::usual_folders(&home)
        })
    }

    /// Every project, sorted by name, so that switching one on or off moves
    /// nothing. Those taken off the list, and the ones found with nothing to
    /// share, are not listed: they are said apart, to put back.
    pub fn list(&self, settings: &Settings, options: &discover::Options) -> Listing {
        let added = self.added();
        let on = self.switched_on();
        let hidden = self.read(HIDDEN);
        let shown = self.read(SHOWN);
        let mut paths = added.clone();
        paths.extend(discover::candidates(&self.roots(settings), &added));

        let mut listing = Listing {
            projects: Vec::new(),
            hidden: Vec::new(),
        };
        let defaults = discover::Commands {
            up: settings.up.clone(),
            down: settings.down.clone(),
        };
        let local = self.all_commands();
        for path in paths {
            let mut project = describe(&path, options, added.contains(&path), on.contains(&path));
            let folder = folder_of(&path);
            let mine = local
                .get(&path.display().to_string())
                .cloned()
                .unwrap_or_default();
            let usual =
                discover::Commands::resolve(&folder, &discover::Commands::default(), &defaults);
            project.startable = mine.up.is_some() || usual.up.is_some();
            (project.local_up, project.local_down) = (mine.up, mine.down);
            (project.usual_up, project.usual_down) = (usual.up, usual.down);
            let why = if hidden.contains(&path) {
                Some("taken off the list")
            } else if project.names.is_empty() && !shown.contains(&path) {
                Some("nothing to share")
            } else {
                None
            };
            match why {
                Some(why) => listing.hidden.push(Hidden {
                    folder: project.folder,
                    name: project.name,
                    why: why.to_string(),
                }),
                None => listing.projects.push(project),
            }
        }
        listing
            .projects
            .sort_by_key(|project| project.name.to_ascii_lowercase());
        listing
            .hidden
            .sort_by_key(|hidden| hidden.name.to_ascii_lowercase());
        listing
    }

    /// What `paths` declare together, to share them. A folder without a
    /// `devshare.toml` gets one written from what it is, as `devshare share`
    /// does; a devshare.toml added by hand is read as it is.
    pub fn config(&self, paths: &[PathBuf], options: &discover::Options) -> Result<Config> {
        let mut together = Config::default();
        for path in paths {
            let config = if path.is_file() {
                Config::load(Some(path.clone()))?
            } else {
                discover::project(path, options)?.0
            };
            for (name, environment) in config.environments {
                if together.environments.contains_key(&name) {
                    bail!(
                        "two projects declare an environment named {name}: rename one in its \
                         devshare.toml"
                    );
                }
                together.environments.insert(name, environment);
            }
        }
        Ok(together)
    }
}

/// The folder a project runs in: its own, or the one of its devshare.toml.
pub fn folder_of(path: &Path) -> PathBuf {
    if path.is_file() {
        path.parent().map(Path::to_path_buf).unwrap_or_default()
    } else {
        path.to_path_buf()
    }
}

/// One project, read without writing anything into its folder.
fn describe(path: &Path, options: &discover::Options, added: bool, on: bool) -> Project {
    let folder = folder_of(path);
    let folder_name = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A devshare.toml of another name is known by that name.
    let fallback = match path.file_stem() {
        Some(stem) if path.is_file() && stem != "devshare" => stem.to_string_lossy().into_owned(),
        _ => folder_name,
    };
    let read = if path.is_file() {
        Config::load(Some(path.to_path_buf())).map(|config| (fallback.clone(), config))
    } else if path.join(discover::FILE).is_file() {
        Config::of(path).map(|config| (fallback.clone(), config))
    } else {
        discover::discover(path, options).map(|found| (found.project.clone(), found.config()))
    };
    let mut project = Project {
        folder: path.display().to_string(),
        name: fallback,
        hostname: None,
        names: Vec::new(),
        ports: Vec::new(),
        addresses: Vec::new(),
        preview: None,
        on,
        added,
        startable: false,
        local_up: None,
        local_down: None,
        usual_up: None,
        usual_down: None,
        problem: None,
    };
    match read {
        Ok((name, config)) => {
            project.name = name;
            // The entry point says which port speaks HTTPS; this machine
            // reaches it as localhost, which its certificate usually covers.
            let entry = config
                .environments
                .values()
                .find_map(|environment| environment.entrypoint.as_deref())
                .and_then(|entrypoint| url::Url::parse(entrypoint).ok())
                .filter(|entry| matches!(entry.scheme(), "http" | "https"));
            let entry_port = entry
                .as_ref()
                .and_then(|entry| entry.port_or_known_default());
            let scheme_of = |port: u16| match &entry {
                Some(entry) if Some(port) == entry_port => entry.scheme().to_string(),
                _ => "http".to_string(),
            };
            project.preview = entry.as_ref().zip(entry_port).map(|(entry, port)| {
                format!(
                    "{}://localhost:{port}{}",
                    entry.scheme(),
                    entry.path().trim_end_matches('/')
                )
            });
            for service in config
                .environments
                .values()
                .flat_map(|environment| &environment.services)
            {
                if !project.names.contains(&service.host) {
                    project.names.push(service.host.clone());
                }
                if !project.ports.contains(&service.port) {
                    project.ports.push(service.port);
                }
                let seen = project
                    .addresses
                    .iter()
                    .any(|known| known.host == service.host && known.port == service.port);
                if !seen {
                    project.addresses.push(Address {
                        host: service.host.clone(),
                        port: service.port,
                        local: format!("{}://localhost:{}", scheme_of(service.port), service.port),
                    });
                }
            }
            project.ports.sort_unstable();
            project
                .addresses
                .sort_by_key(|address| (address.host.clone(), address.port));
            project.hostname = project.names.first().cloned();
            if project.names.is_empty() {
                project.problem =
                    Some("it publishes no port on this machine: nothing to share".to_string());
            }
        }
        Err(error) => project.problem = Some(format!("{error:#}")),
    }
    project
}
