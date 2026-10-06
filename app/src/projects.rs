//! The projects the owner added to the app: a list of folders, each with its
//! own `devshare.toml`. The list is the app's memory, kept with the app's
//! data; it is neither a setting nor part of any project.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use devshare_core::environment::Config;

const FILE: &str = "projects.json";

pub struct Projects {
    file: PathBuf,
}

/// What the folders declare together, and what could not be read of them.
#[derive(Default)]
pub struct Declared {
    /// Every environment, with the folder it comes from.
    pub environments: Vec<(PathBuf, String, Vec<String>)>,
    /// All of them as one configuration, to start a session from.
    pub config: Config,
    /// A folder and why it could not be read: one project that moved must
    /// not hide the others.
    pub problems: Vec<(PathBuf, String)>,
}

impl Projects {
    /// The list kept in `folder`, the app's own data folder.
    pub fn in_folder(folder: PathBuf) -> Self {
        Self {
            file: folder.join(FILE),
        }
    }

    pub fn folders(&self) -> Vec<PathBuf> {
        std::fs::read_to_string(&self.file)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    fn save(&self, folders: &[PathBuf]) -> Result<()> {
        if let Some(folder) = self.file.parent() {
            std::fs::create_dir_all(folder)
                .with_context(|| format!("creating {}", folder.display()))?;
        }
        std::fs::write(&self.file, serde_json::to_string_pretty(folders)?)
            .with_context(|| format!("writing {}", self.file.display()))
    }

    /// Adds a folder, once.
    pub fn add(&self, folder: &Path) -> Result<()> {
        let folder = folder
            .canonicalize()
            .with_context(|| format!("locating {}", folder.display()))?;
        let mut folders = self.folders();
        if !folders.contains(&folder) {
            folders.push(folder);
            self.save(&folders)?;
        }
        Ok(())
    }

    /// Forgets a folder. Nothing in the folder is touched.
    pub fn remove(&self, folder: &Path) -> Result<()> {
        let mut folders = self.folders();
        folders.retain(|known| known != folder);
        self.save(&folders)
    }

    pub fn declared(&self) -> Declared {
        let mut declared = Declared::default();
        for folder in self.folders() {
            match Config::of(&folder) {
                Ok(project) => {
                    for (name, environment) in project.environments {
                        let services = environment
                            .services
                            .iter()
                            .map(|service| format!("{}:{}", service.host, service.port))
                            .collect();
                        if declared.config.environments.contains_key(&name) {
                            declared.problems.push((
                                folder.clone(),
                                format!(
                                    "another project already declares an environment named {name}"
                                ),
                            ));
                            continue;
                        }
                        declared
                            .environments
                            .push((folder.clone(), name.clone(), services));
                        declared.config.environments.insert(name, environment);
                    }
                }
                Err(error) => declared.problems.push((folder, format!("{error:#}"))),
            }
        }
        declared
    }
}
