//! The projects of the app: the ones found where the owner keeps projects
//! and the ones added by hand, each switched on or off. Both lists are the
//! app's memory, kept with the app's data; they are neither a setting nor
//! part of any project.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use devshare_core::{
    discover,
    environment::{Config, Settings},
};
use serde::Serialize;

/// The folders added by hand.
const ADDED: &str = "projects.json";
/// The folders switched on.
const ON: &str = "selected.json";

pub struct Projects {
    folder: PathBuf,
}

/// One project as the window lists it.
#[derive(Debug, Clone, Serialize)]
pub struct Project {
    pub folder: String,
    pub name: String,
    /// The name guests reach it under first.
    pub hostname: Option<String>,
    /// Every name and port it shares.
    pub names: Vec<String>,
    pub ports: Vec<u16>,
    pub on: bool,
    /// Added by hand, rather than found: it can be taken off the list.
    pub added: bool,
    /// Whether the app knows how to start and stop it.
    pub startable: bool,
    /// Why it cannot be shared as it is.
    pub problem: Option<String>,
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

    fn write(&self, file: &str, folders: &[PathBuf]) -> Result<()> {
        std::fs::create_dir_all(&self.folder)
            .with_context(|| format!("creating {}", self.folder.display()))?;
        let path = self.folder.join(file);
        std::fs::write(&path, serde_json::to_string_pretty(folders)?)
            .with_context(|| format!("writing {}", path.display()))
    }

    /// The folders added by hand.
    pub fn added(&self) -> Vec<PathBuf> {
        self.read(ADDED)
    }

    /// The folders switched on.
    pub fn switched_on(&self) -> Vec<PathBuf> {
        self.read(ON)
    }

    /// Adds a project's folder by hand, switched on.
    pub fn add(&self, folder: &Path) -> Result<PathBuf> {
        let folder = folder
            .canonicalize()
            .with_context(|| format!("locating {}", folder.display()))?;
        if !discover::is_project(&folder) && !folder.join(discover::FILE).is_file() {
            bail!(
                "{} is not a project DevShare can read: no compose file, Vite or Symfony \
                 configuration, nor devshare.toml",
                folder.display()
            );
        }
        let mut added = self.added();
        if !added.contains(&folder) {
            added.push(folder.clone());
            self.write(ADDED, &added)?;
        }
        self.switch(&folder, true)?;
        Ok(folder)
    }

    /// Takes a folder added by hand off the list. Nothing in it is touched.
    pub fn remove(&self, folder: &Path) -> Result<()> {
        let mut added = self.added();
        added.retain(|known| known != folder);
        self.write(ADDED, &added)?;
        self.switch(folder, false)
    }

    pub fn switch(&self, folder: &Path, on: bool) -> Result<()> {
        let mut switched = self.switched_on();
        switched.retain(|known| known != folder);
        if on {
            switched.push(folder.to_path_buf());
        }
        self.write(ON, &switched)
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

    /// Every project: the ones added by hand and the ones found, sorted by
    /// name, so that switching one on or off moves nothing.
    pub fn list(&self, settings: &Settings, options: &discover::Options) -> Vec<Project> {
        let added = self.added();
        let on = self.switched_on();
        let mut folders = added.clone();
        folders.extend(discover::candidates(&self.roots(settings), &added));
        let mut projects: Vec<Project> = folders
            .into_iter()
            .map(|folder| {
                describe(
                    &folder,
                    options,
                    added.contains(&folder),
                    on.contains(&folder),
                )
            })
            .collect();
        projects.sort_by_key(|project| project.name.to_ascii_lowercase());
        projects
    }

    /// What `folders` declare together, to share them. A project without a
    /// `devshare.toml` gets one written from what it is, as `devshare share`
    /// does.
    pub fn config(&self, folders: &[PathBuf], options: &discover::Options) -> Result<Config> {
        let mut together = Config::default();
        for folder in folders {
            let (config, _) = discover::project(folder, options)?;
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

/// One project, read without writing anything into its folder.
fn describe(folder: &Path, options: &discover::Options, added: bool, on: bool) -> Project {
    let fallback = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let read = if folder.join(discover::FILE).is_file() {
        Config::of(folder).map(|config| (fallback.clone(), config))
    } else {
        discover::discover(folder, options).map(|found| (found.project.clone(), found.config()))
    };
    let startable = discover::Commands::of(folder).up.is_some();
    match read {
        Ok((name, config)) => {
            let mut names: Vec<String> = Vec::new();
            let mut ports: Vec<u16> = Vec::new();
            for service in config
                .environments
                .values()
                .flat_map(|environment| &environment.services)
            {
                if !names.contains(&service.host) {
                    names.push(service.host.clone());
                }
                if !ports.contains(&service.port) {
                    ports.push(service.port);
                }
            }
            ports.sort_unstable();
            Project {
                folder: folder.display().to_string(),
                name,
                hostname: names.first().cloned(),
                problem: names
                    .is_empty()
                    .then(|| "nothing to share in it".to_string()),
                names,
                ports,
                on,
                added,
                startable,
            }
        }
        Err(error) => Project {
            folder: folder.display().to_string(),
            name: fallback,
            hostname: None,
            names: Vec::new(),
            ports: Vec::new(),
            on,
            added,
            startable,
            problem: Some(format!("{error:#}")),
        },
    }
}
