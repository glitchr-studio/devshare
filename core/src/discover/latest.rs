//! The latest version of each of a project's dependencies, to see at a
//! glance whether what is installed is in sync. Asked of where each manager
//! publishes: Packagist, the npm registry, crates.io, PyPI, the Go proxy,
//! RubyGems. A Composer package followed on a branch (`3.x-dev`) is compared
//! commit to commit, with the head of that branch in the repository the lock
//! file names.
//!
//! This sends the names of the project's packages to those registries: it is
//! done when asked for, never on its own.

use std::time::Duration;

use futures::StreamExt;
use serde::Serialize;
use serde_json::Value;

use super::stack::{Package, Stack};

/// How many registries are asked at once.
const AT_ONCE: usize = 12;
const PATIENCE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Latest {
    /// The manifest and the package this is about.
    pub file: String,
    pub name: String,
    /// The latest version published, or the head of the branch followed
    /// (its first characters).
    pub latest: Option<String>,
    /// `current`, `behind` (a newer version of the same major, or newer
    /// commits on the branch), `major` (a newer major), `unknown`.
    pub state: &'static str,
    /// Why nothing is known, when nothing is.
    pub detail: Option<String>,
}

/// The latest version of every package of every manifest.
pub async fn latest(stack: &Stack) -> Vec<Latest> {
    let client = match reqwest::Client::builder()
        .user_agent(concat!(
            "devshare/",
            env!("CARGO_PKG_VERSION"),
            " (dependency check)"
        ))
        .timeout(PATIENCE)
        .build()
    {
        Ok(client) => client,
        Err(_) => return Vec::new(),
    };
    // Each question owns what it asks about: they are asked side by side.
    let asked: Vec<(String, String, Package)> = stack
        .manifests
        .iter()
        .flat_map(|manifest| {
            manifest.packages.iter().map(|package| {
                (
                    manifest.file.clone(),
                    manifest.manager.clone(),
                    package.clone(),
                )
            })
        })
        .collect();
    futures::stream::iter(asked)
        .map(|(file, manager, package)| {
            let client = client.clone();
            async move {
                let (latest, state, detail) = one(&client, &manager, &package).await;
                Latest {
                    file,
                    name: package.name,
                    latest,
                    state,
                    detail,
                }
            }
        })
        .buffer_unordered(AT_ONCE)
        .collect()
        .await
}

type Answer = (Option<String>, &'static str, Option<String>);

fn unknown(why: &str) -> Answer {
    (None, "unknown", Some(why.to_string()))
}

async fn one(client: &reqwest::Client, manager: &str, package: &Package) -> Answer {
    let installed = package.installed.as_deref();
    let published = match manager {
        "Composer" => {
            if let Some(branch) = installed.and_then(branch_of) {
                return on_branch(client, package, &branch).await;
            }
            match packagist(client, &package.name).await {
                Ok(Some(version)) => Ok(version),
                // Not on Packagist: the tags of its own repository.
                Ok(None) => match &package.source {
                    Some(source) => match tags(source).await {
                        Some(version) => Ok(version),
                        None => Err("not on Packagist, and its repository did not answer"),
                    },
                    None => Err("not on Packagist"),
                },
                Err(()) => Err("Packagist did not answer"),
            }
        }
        "npm" | "Yarn" | "pnpm" | "Bun" => {
            // `file:vendor/…`, `link:`, `workspace:`, a git or an http
            // address: not from the registry, nothing to compare with.
            let elsewhere = package.asked.as_deref().is_some_and(|asked| {
                ["file:", "link:", "workspace:", "git", "http", "github:"]
                    .iter()
                    .any(|scheme| asked.starts_with(scheme))
            });
            if elsewhere {
                return unknown("installed from a file or a repository, not from the registry");
            }
            let name = package.name.replace('/', "%2F");
            field(
                client,
                &format!("https://registry.npmjs.org/{name}/latest"),
                &["version"],
            )
            .await
            .ok_or("not on the npm registry")
        }
        "Cargo" => field(
            client,
            &format!("https://crates.io/api/v1/crates/{}", package.name),
            &["crate", "max_stable_version"],
        )
        .await
        .ok_or("not on crates.io"),
        "pip" => field(
            client,
            &format!("https://pypi.org/pypi/{}/json", package.name),
            &["info", "version"],
        )
        .await
        .ok_or("not on PyPI"),
        "Go" => {
            // Capitals are written !lowercase in the proxy's paths.
            let module: String = package
                .name
                .chars()
                .flat_map(|c| {
                    if c.is_ascii_uppercase() {
                        vec!['!', c.to_ascii_lowercase()]
                    } else {
                        vec![c]
                    }
                })
                .collect();
            field(
                client,
                &format!("https://proxy.golang.org/{module}/@latest"),
                &["Version"],
            )
            .await
            .ok_or("not on the Go proxy")
        }
        "Bundler" => field(
            client,
            &format!(
                "https://rubygems.org/api/v1/versions/{}/latest.json",
                package.name
            ),
            &["version"],
        )
        .await
        .ok_or("not on RubyGems"),
        _ => Err("no registry known for this manager"),
    };
    match published {
        Ok(version) => {
            let version = version.trim_start_matches('v').to_string();
            let state = compare(installed, &version);
            (Some(version), state, None)
        }
        Err(why) => unknown(why),
    }
}

/// One field of a JSON answer, as text.
async fn field(client: &reqwest::Client, url: &str, path: &[&str]) -> Option<String> {
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let mut value: Value = response.json().await.ok()?;
    for key in path {
        value = value.get_mut(*key)?.take();
    }
    value.as_str().map(str::to_string)
}

/// The latest stable version Packagist has of a package: `Ok(None)` when it
/// is not there, `Err` when Packagist cannot be asked.
async fn packagist(client: &reqwest::Client, name: &str) -> Result<Option<String>, ()> {
    let url = format!("https://repo.packagist.org/p2/{name}.json");
    let response = client.get(url).send().await.map_err(|_| ())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(());
    }
    let answer: Value = response.json().await.map_err(|_| ())?;
    // Newest first.
    let versions = answer["packages"][name].as_array();
    Ok(versions
        .into_iter()
        .flatten()
        .filter_map(|version| version["version"].as_str())
        .find(|version| stable(version))
        .map(str::to_string))
}

/// `7.3.4` is; `7.4.0-RC1`, `8.0.0-beta2`, `dev-main` are not.
fn stable(version: &str) -> bool {
    let version = version.trim_start_matches('v').to_ascii_lowercase();
    version.starts_with(|c: char| c.is_ascii_digit())
        && !["dev", "alpha", "beta", "rc", "pre", "snapshot"]
            .iter()
            .any(|marker| version.contains(marker))
}

/// The branch a Composer version follows: `3.x-dev` is `3.x`, `dev-main` is
/// `main`; a released version follows none.
fn branch_of(version: &str) -> Option<String> {
    let version = version.split_whitespace().next()?;
    version
        .strip_prefix("dev-")
        .or_else(|| version.strip_suffix("-dev"))
        .map(str::to_string)
}

/// A package followed on a branch: the head of the branch in its
/// repository, against the commit installed.
async fn on_branch(client: &reqwest::Client, package: &Package, branch: &str) -> Answer {
    let installed = package.reference.as_deref();
    // The repository the lock file names says best; else Packagist's copy.
    let head = match &package.source {
        Some(source) => head(source, branch).await,
        None => None,
    };
    let head = match head {
        Some(head) => Some(head),
        None => packagist_branch(client, &package.name, branch).await,
    };
    let Some(head) = head else {
        return unknown("the head of its branch could not be read");
    };
    let short: String = head.chars().take(7).collect();
    let state = match installed {
        Some(installed) if installed == head => "current",
        Some(_) => "behind",
        None => "unknown",
    };
    (Some(short), state, None)
}

/// The commit of a dev version on Packagist.
async fn packagist_branch(client: &reqwest::Client, name: &str, branch: &str) -> Option<String> {
    let url = format!("https://repo.packagist.org/p2/{name}~dev.json");
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let answer: Value = response.json().await.ok()?;
    let wanted = [format!("dev-{branch}"), format!("{branch}-dev")];
    answer["packages"][name]
        .as_array()?
        .iter()
        .find(|version| {
            version["version"]
                .as_str()
                .is_some_and(|version| wanted.iter().any(|w| w == version))
        })?
        .get("source")?
        .get("reference")?
        .as_str()
        .map(str::to_string)
}

/// `git ls-remote`, without ever asking for a password, given up on after a
/// while: its lines as (commit, ref).
async fn ls_remote(repository: &str, what: &[&str]) -> Option<Vec<(String, String)>> {
    // A URL or an scp-like address, never an option.
    if repository.starts_with('-') {
        return None;
    }
    let mut command = tokio::process::Command::new("git");
    command
        .arg("ls-remote")
        .args(what)
        .arg("--")
        .arg(repository)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes -oConnectTimeout=6")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(PATIENCE, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let (commit, name) = line.split_once('\t')?;
                Some((commit.to_string(), name.to_string()))
            })
            .collect(),
    )
}

/// The head of a branch. Composer writes the branch `1.0` as `1.0.x-dev`:
/// both names are looked for.
async fn head(repository: &str, branch: &str) -> Option<String> {
    let heads = ls_remote(repository, &["--heads"]).await?;
    let names = [branch, branch.trim_end_matches(".x")];
    names.iter().find_map(|name| {
        let wanted = format!("refs/heads/{name}");
        heads
            .iter()
            .find(|(_, found)| *found == wanted)
            .map(|(commit, _)| commit.clone())
    })
}

/// The highest stable version among a repository's tags.
async fn tags(repository: &str) -> Option<String> {
    let tags = ls_remote(repository, &["--tags", "--refs"]).await?;
    tags.iter()
        .filter_map(|(_, name)| name.strip_prefix("refs/tags/"))
        .filter(|tag| stable(tag))
        .max_by_key(|tag| numbers(tag))
        .map(|tag| tag.trim_start_matches('v').to_string())
}

/// `v7.3.4-fpm` as [7, 3, 4].
fn numbers(version: &str) -> Vec<u64> {
    version
        .trim_start_matches(['v', '=', '^', '~', ' '])
        .split(['-', '+', ' '])
        .next()
        .unwrap_or_default()
        .split('.')
        .map_while(|part| part.parse().ok())
        .collect()
}

/// Where what is installed stands against the latest.
fn compare(installed: Option<&str>, latest: &str) -> &'static str {
    let Some(installed) = installed else {
        return "unknown";
    };
    let (mut have, mut newest) = (numbers(installed), numbers(latest));
    if have.is_empty() || newest.is_empty() {
        return "unknown";
    }
    let length = have.len().max(newest.len());
    have.resize(length, 0);
    newest.resize(length, 0);
    match () {
        _ if have >= newest => "current",
        _ if have[0] < newest[0] => "major",
        _ => "behind",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_compared_by_their_numbers() {
        assert_eq!(compare(Some("7.3.4"), "7.3.4"), "current");
        assert_eq!(compare(Some("v7.3.4"), "7.3.4"), "current");
        assert_eq!(compare(Some("7.3"), "7.3.0"), "current");
        // Ahead of the registry (a version just published): not behind.
        assert_eq!(compare(Some("7.4.0"), "7.3.9"), "current");
        assert_eq!(compare(Some("7.3.4"), "7.3.10"), "behind");
        assert_eq!(compare(Some("7.3.4"), "7.4.0"), "behind");
        assert_eq!(compare(Some("7.3.4"), "8.0.0"), "major");
        assert_eq!(compare(Some("3.95.25"), "3.95.25"), "current");
        assert_eq!(compare(None, "1.0.0"), "unknown");
        assert_eq!(compare(Some("dev-main"), "1.0.0"), "unknown");
    }

    #[test]
    fn branches_and_stable_versions_are_told_apart() {
        assert_eq!(branch_of("3.x-dev").as_deref(), Some("3.x"));
        assert_eq!(branch_of("1.0.x-dev").as_deref(), Some("1.0.x"));
        assert_eq!(branch_of("dev-main").as_deref(), Some("main"));
        assert_eq!(branch_of("7.3.4"), None);
        assert!(stable("v7.3.4") && stable("30.0"));
        assert!(!stable("7.4.0-RC1") && !stable("8.0.0-beta2") && !stable("dev-main"));
        assert!(!stable("3.x-dev") && !stable("latest"));
        assert_eq!(numbers("v7.3.4-fpm"), [7, 3, 4]);
        assert_eq!(numbers("^3.2"), [3, 2]);
    }
}
