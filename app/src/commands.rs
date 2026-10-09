//! What the window can ask of the app. Each function here is one request;
//! the session answers through the `session` and `ended` events.

use std::{path::PathBuf, sync::Mutex, time::Duration};

use devshare_core::{
    discover,
    environment::{self, Settings},
    host::ShareOptions,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, RunEvent, Runtime, State};

use crate::{
    joined::{JoinUpdate, Joined},
    projects::{folder_of, Hidden, Project, Projects},
    session::{Session, Update},
};

/// The scheme the invitation pages hand an invitation to the app with:
/// `devshare://open?link=<the invitation, URL-encoded>`.
pub const SCHEME: &str = "devshare";

/// The longest invitation taken from a link: a few times the longest one.
const LONGEST_INVITATION: usize = 2048;

/// The session in progress, if any.
#[derive(Default)]
pub(crate) struct Sharing(Mutex<Option<Session>>);

/// The session this computer joined, if any.
#[derive(Default)]
struct Joining(Mutex<Option<Joined>>);

/// An invitation handed over by a link before the window asked for it.
#[derive(Default)]
struct Handed(Mutex<Option<String>>);

/// The invitation in a `devshare://open?link=…` link, if it is one.
pub fn invitation_of(link: &url::Url) -> Option<String> {
    if link.scheme() != SCHEME {
        return None;
    }
    let (_, invitation) = link.query_pairs().find(|(key, _)| key == "link")?;
    let invitation = invitation.trim();
    let fine = !invitation.is_empty()
        && invitation.len() <= LONGEST_INVITATION
        && !invitation.chars().any(char::is_control);
    fine.then(|| invitation.to_string())
}

impl Sharing {
    pub(crate) fn current(&self) -> Option<Session> {
        self.0.lock().unwrap().clone()
    }
}

/// The app's list of projects, kept with the app's data. `DEVSHARE_APP_DATA`
/// names another folder for it.
pub(crate) fn projects<R: Runtime>(app: &AppHandle<R>) -> Result<Projects, String> {
    let folder = match std::env::var_os("DEVSHARE_APP_DATA") {
        Some(folder) => PathBuf::from(folder),
        None => app
            .path()
            .app_data_dir()
            .map_err(|error| format!("no folder for the app's data: {error}"))?,
    };
    Ok(Projects::in_folder(folder))
}

pub(crate) fn discovery_options() -> discover::Options {
    discover::Options {
        hostname: None,
        domain: Settings::load().ok().map(|settings| settings.domain()),
        environment: std::env::vars().collect(),
        hosts: Some("/etc/hosts".into()),
    }
}

/// What the window shows before sharing: every project, where they were
/// looked for, and the usual duration and number of guests.
#[derive(Serialize)]
struct Overview {
    projects: Vec<Project>,
    /// Taken off the list, or found with nothing to share: to put back.
    hidden: Vec<Hidden>,
    /// The folders projects are looked for in.
    folders: Vec<String>,
    /// The usual duration in minutes; 0 for no time limit.
    minutes: u64,
    guests: u32,
    /// Why the general settings could not be read, if they could not.
    problem: Option<String>,
}

#[tauri::command]
async fn overview<R: Runtime>(app: AppHandle<R>) -> Result<Overview, String> {
    let projects = projects(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        // Settings that cannot be read are said, and the defaults are used.
        let (settings, problem) = match Settings::load() {
            Ok(settings) => (settings, None),
            Err(error) => (Settings::default(), Some(format!("{error:#}"))),
        };
        let listing = projects.list(&settings, &discovery_options());
        let lifetime = settings.duration().unwrap_or(environment::DEFAULT_DURATION);
        let minutes = if devshare_core::protocol::unlimited(lifetime.as_secs()) {
            0
        } else {
            lifetime.as_secs().div_ceil(60)
        };
        Overview {
            folders: projects
                .roots(&settings)
                .iter()
                .map(|folder| folder.display().to_string())
                .collect(),
            projects: listing.projects,
            hidden: listing.hidden,
            minutes,
            guests: settings.guests(),
            problem,
        }
    })
    .await
    .map_err(|error| error.to_string())
}

/// Switches a project on or off: what Share shares.
#[tauri::command]
fn switch<R: Runtime>(
    app: AppHandle<R>,
    window: tauri::Window<R>,
    path: String,
    on: bool,
) -> Result<(), String> {
    projects(&app)?
        .switch(&PathBuf::from(path), on)
        .map_err(|error| format!("{error:#}"))?;
    crate::native::changed(&app, window.label());
    Ok(())
}

/// Ends the app: from the menu bar panel.
#[tauri::command]
fn quit<R: Runtime>(app: AppHandle<R>) {
    on_event(&app, RunEvent::Exit);
    app.exit(0);
}

/// Adds a project, switched on: its folder, or a devshare.toml of any name.
/// Nothing is written into it until it is shared. A folder that is no
/// project but holds some becomes a folder projects are looked for in.
#[tauri::command]
fn add_project<R: Runtime>(app: AppHandle<R>, path: String) -> Result<String, String> {
    let path = PathBuf::from(path.trim());
    let projects = projects(&app)?;
    if path.is_dir()
        && !discover::is_project(&path)
        && !path.join(discover::FILE).is_file()
        && !discover::candidates(std::slice::from_ref(&path), &[]).is_empty()
    {
        change_sources(|sources| sources.push(path.display().to_string()))?;
        return Ok(format!("projects in {}", path.display()));
    }
    let added = projects.add(&path).map_err(|error| format!("{error:#}"))?;
    Ok(added
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default())
}

/// Changes the folders projects are looked for in, starting from the usual
/// ones when none were chosen.
fn change_sources(change: impl FnOnce(&mut Vec<String>)) -> Result<(), String> {
    let mut settings = Settings::load().map_err(|error| format!("{error:#}"))?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut sources = settings.folders.clone().unwrap_or_else(|| {
        discover::usual_folders(&home)
            .iter()
            .map(|folder| tilde(folder))
            .collect()
    });
    change(&mut sources);
    let mut seen = Vec::new();
    sources.retain(|source| {
        let new = !seen.contains(source);
        seen.push(source.clone());
        new
    });
    settings.folders = Some(sources);
    settings.save().map_err(|error| format!("{error:#}"))
}

/// `/Users/me/Sites` as `~/Sites`, as a person writes it.
fn tilde(folder: &std::path::Path) -> String {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    match folder.strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => folder.display().to_string(),
    }
}

/// Adds a folder to look for projects in.
#[tauri::command]
fn add_source(path: String) -> Result<(), String> {
    let folder = PathBuf::from(path.trim());
    if !folder.is_dir() {
        return Err(format!("{} is not a folder", folder.display()));
    }
    change_sources(|sources| sources.push(tilde(&folder)))
}

/// Stops looking for projects in a folder.
#[tauri::command]
fn remove_source(path: String) -> Result<(), String> {
    let folder = PathBuf::from(path.trim());
    let written = tilde(&folder);
    change_sources(|sources| {
        sources.retain(|source| *source != written && std::path::Path::new(source) != folder)
    })
}

/// Asks for a folder, or a devshare.toml, with the system's own dialog.
#[tauri::command]
async fn pick<R: Runtime>(app: AppHandle<R>, file: bool) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let (chosen, picked) = tokio::sync::oneshot::channel();
    let dialog = app.dialog().file();
    if file {
        dialog
            .add_filter("DevShare configuration", &["toml"])
            .pick_file(move |file| {
                chosen.send(file).ok();
            });
    } else {
        dialog.pick_folder(move |folder| {
            chosen.send(folder).ok();
        });
    }
    picked
        .await
        .ok()
        .flatten()
        .and_then(|folder| folder.into_path().ok())
        .map(|folder| folder.display().to_string())
}

/// Takes a project off the list, found or added, until it is put back. Its
/// folder is left as it is.
#[tauri::command]
fn remove_project<R: Runtime>(
    app: AppHandle<R>,
    window: tauri::Window<R>,
    path: String,
) -> Result<(), String> {
    projects(&app)?
        .remove(&PathBuf::from(path))
        .map_err(|error| format!("{error:#}"))?;
    crate::native::changed(&app, window.label());
    Ok(())
}

/// How this project is started and stopped on this computer, whatever its
/// devshare.toml and the defaults say. Empty: back to those.
#[tauri::command]
fn set_commands<R: Runtime>(
    app: AppHandle<R>,
    path: String,
    up: Option<String>,
    down: Option<String>,
) -> Result<(), String> {
    projects(&app)?
        .set_local_commands(&PathBuf::from(path), discover::Commands { up, down })
        .map_err(|error| format!("{error:#}"))
}

/// Opens one of this machine's own addresses in the browser: a project's
/// port, as the owner would check it. Nothing else is opened from here.
#[tauri::command]
fn open_local(url: String) -> Result<(), String> {
    let parsed = url::Url::parse(&url).map_err(|error| error.to_string())?;
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    let local = matches!(parsed.scheme(), "http" | "https")
        && (matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
            || loopback_names().contains(&host));
    if !local {
        return Err("only this machine's own addresses are opened from here".into());
    }
    tauri_plugin_opener::open_url(&url, None::<&str>).map_err(|error| error.to_string())
}

/// Shows a project's folder in the Finder (or the file manager).
#[tauri::command]
fn reveal(path: String) -> Result<(), String> {
    let folder = folder_of(&PathBuf::from(path));
    tauri_plugin_opener::reveal_item_in_dir(folder).map_err(|error| error.to_string())
}

/// Puts a project back on the list, even one with nothing to share.
#[tauri::command]
fn restore_project<R: Runtime>(
    app: AppHandle<R>,
    window: tauri::Window<R>,
    path: String,
) -> Result<(), String> {
    projects(&app)?
        .restore(&PathBuf::from(path))
        .map_err(|error| format!("{error:#}"))?;
    crate::native::changed(&app, window.label());
    Ok(())
}

/// Whether a project runs: `running` when every port it announces answers,
/// `partial` when only some do, `stopped` when none does.
#[derive(Serialize)]
struct Running {
    path: String,
    state: &'static str,
}

#[tauri::command]
async fn running(paths: Vec<String>) -> Result<Vec<Running>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let options = discovery_options();
        std::thread::scope(|scope| {
            let checks: Vec<_> = paths
                .iter()
                .map(|path| {
                    let options = &options;
                    scope.spawn(move || Running {
                        state: state_of(std::path::Path::new(path), options),
                        path: path.clone(),
                    })
                })
                .collect();
            checks
                .into_iter()
                .filter_map(|check| check.join().ok())
                .collect()
        })
    })
    .await
    .map_err(|error| error.to_string())
}

/// What a project shares, read without writing anything into its folder.
fn config_of(path: &std::path::Path, options: &discover::Options) -> Option<environment::Config> {
    if path.is_file() {
        environment::Config::load(Some(path.to_path_buf())).ok()
    } else if path.join(discover::FILE).is_file() {
        environment::Config::of(path).ok()
    } else {
        discover::discover(path, options)
            .ok()
            .map(|found| found.config())
    }
}

/// Where the host agent dials each of a project's ports, once each.
fn targets(config: &environment::Config) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();
    for service in config
        .environments
        .values()
        .flat_map(|environment| &environment.services)
    {
        let target = service
            .target
            .clone()
            .unwrap_or_else(|| format!("127.0.0.1:{}", service.port));
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets
}

fn listens(target: &str) -> bool {
    std::net::ToSocketAddrs::to_socket_addrs(target)
        .ok()
        .and_then(|mut addresses| addresses.next())
        .is_some_and(|address| {
            std::net::TcpStream::connect_timeout(&address, Duration::from_millis(300)).is_ok()
        })
}

fn state_of(path: &std::path::Path, options: &discover::Options) -> &'static str {
    let Some(config) = config_of(path, options) else {
        return "stopped";
    };
    let targets = targets(&config);
    let answering = targets.iter().filter(|target| listens(target)).count();
    match answering {
        0 => "stopped",
        count if count == targets.len() => "running",
        _ => "partial",
    }
}

/// One address of a project, as this machine reaches it.
#[derive(Serialize)]
struct AddressCheck {
    host: String,
    port: u16,
    /// What to open in a browser here: the name when this machine's
    /// /etc/hosts gives it to the loopback, else the address dialled.
    url: String,
    /// `ok` (a page), `error` (an HTTP error, or not a page), `down`.
    state: &'static str,
    /// `200 OK`, `426 Upgrade Required`, `nothing answers`…
    detail: String,
    tls: bool,
}

/// Every address of a project: whether it answers, with what.
#[tauri::command]
async fn check_project(path: String) -> Result<Vec<AddressCheck>, String> {
    let options = discovery_options();
    let path = PathBuf::from(path);
    let config = tauri::async_runtime::spawn_blocking(move || config_of(&path, &options))
        .await
        .map_err(|error| error.to_string())?
        .ok_or("this project cannot be read")?;
    let named = loopback_names();
    let mut checks = Vec::new();
    let mut seen = Vec::new();
    for service in config
        .environments
        .values()
        .flat_map(|environment| &environment.services)
    {
        if seen.contains(&(service.host.clone(), service.port)) {
            continue;
        }
        seen.push((service.host.clone(), service.port));
        let target = service
            .target
            .clone()
            .unwrap_or_else(|| format!("127.0.0.1:{}", service.port));
        let (host, port, named) = (
            service.host.clone(),
            service.port,
            named.contains(&service.host),
        );
        checks.push(tokio::spawn(async move {
            check(host, port, target, named).await
        }));
    }
    let mut done = Vec::new();
    for check in checks {
        if let Ok(check) = check.await {
            done.push(check);
        }
    }
    Ok(done)
}

async fn check(host: String, port: u16, target: String, named: bool) -> AddressCheck {
    use devshare_core::probe::{probe, Probe};
    let probed = probe(&target, &host).await;
    let tls = matches!(probed, Probe::Tls { .. });
    let scheme = if tls { "https" } else { "http" };
    let dialled = target
        .rsplit_once(':')
        .map(|(address, _)| address)
        .unwrap_or(&target);
    let target_port = target
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or(port);
    let shown = if named {
        host.clone()
    } else {
        dialled.replace("127.0.0.1", "localhost")
    };
    let url = format!("{scheme}://{shown}:{target_port}");
    let answer = |state, detail: String| AddressCheck {
        host: host.clone(),
        port,
        url: url.clone(),
        state,
        detail,
        tls,
    };
    if probed == Probe::Down {
        return answer("down", "nothing answers".into());
    }
    // Asked by its name, as a guest would, whatever this machine resolves.
    let Some(address) = std::net::ToSocketAddrs::to_socket_addrs(&target)
        .ok()
        .and_then(|mut addresses| addresses.next())
    else {
        return answer("down", "nothing answers".into());
    };
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(4))
        .resolve(&host, address)
        .build();
    let Ok(client) = client else {
        return answer("error", "cannot be asked".into());
    };
    match client
        .get(format!("{scheme}://{host}:{target_port}/"))
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            let detail = format!(
                "{} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            )
            .trim()
            .to_string();
            answer(if status.as_u16() < 400 { "ok" } else { "error" }, detail)
        }
        Err(error) if error.is_timeout() => answer("error", "answers, but not in time".into()),
        Err(_) => answer("error", "answers, but not with a web page".into()),
    }
}

/// The names this machine's /etc/hosts gives to its loopback.
fn loopback_names() -> Vec<String> {
    std::fs::read_to_string("/etc/hosts")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let line = line.split('#').next().unwrap_or_default();
            let mut words = line.split_whitespace();
            let address: std::net::IpAddr = words.next()?.parse().ok()?;
            address.is_loopback().then(|| {
                words
                    .map(|word| word.to_ascii_lowercase())
                    .collect::<Vec<_>>()
            })
        })
        .flatten()
        .collect()
}

/// Puts the panel away: after a choice made in it that opens the window.
#[tauri::command]
fn hide_panel<R: Runtime>(app: AppHandle<R>) {
    if let Some(panel) = app.get_webview_window(crate::native::PANEL) {
        panel.hide().ok();
    }
}

/// Brings the window to the front: from the panel.
#[tauri::command]
fn show_window<R: Runtime>(app: AppHandle<R>, page: Option<String>) {
    crate::native::show(&app);
    if let Some(page) = page {
        app.emit_to("main", "navigate", page).ok();
    }
}

/// Starts or stops a project its own way: `make up` / `make down` when its
/// Makefile has them, else Docker Compose, else what its devshare.toml says.
#[tauri::command]
async fn run_project<R: Runtime>(
    app: AppHandle<R>,
    path: String,
    action: String,
) -> Result<(), String> {
    let path = PathBuf::from(path);
    let folder = folder_of(&path);
    let defaults = Settings::load().unwrap_or_default();
    let commands = discover::Commands::resolve(
        &folder,
        &projects(&app)?.local_commands(&path),
        &discover::Commands {
            up: defaults.up,
            down: defaults.down,
        },
    );
    let command = match action.as_str() {
        "up" => commands.up,
        "down" => commands.down,
        other => return Err(format!("no such action: {other}")),
    }
    .ok_or(
        "DevShare does not know how to start this project: give it a start command in its details",
    )?;
    crate::system::run_in(&folder, &command).await
}
/// The general settings as they are written, and the defaults that apply
/// where nothing is.
#[derive(Serialize, serde::Deserialize)]
struct SettingsView {
    server: Option<String>,
    duration: Option<String>,
    guests: Option<u32>,
    domain: Option<String>,
    relay: Option<String>,
    join: Option<String>,
    /// Where projects are looked for, one folder after another. Left as it
    /// is when not sent: the sidebar's list edits it.
    #[serde(default)]
    folders: Option<Vec<String>>,
    /// How projects are started and stopped when they say nothing of their
    /// own.
    #[serde(default)]
    up: Option<String>,
    #[serde(default)]
    down: Option<String>,
}

#[tauri::command]
fn settings() -> Result<SettingsView, String> {
    let settings = Settings::load().map_err(|error| format!("{error:#}"))?;
    Ok(SettingsView {
        server: settings.server,
        duration: settings.duration,
        guests: settings.guests,
        domain: settings.domain,
        relay: settings.relay,
        join: settings.join,
        folders: settings.folders,
        up: settings.up,
        down: settings.down,
    })
}

/// Saves the general settings. An empty field is removed from the file,
/// and its default applies; the file's comments stay.
#[tauri::command]
fn save_settings(values: SettingsView) -> Result<(), String> {
    let filled = |value: Option<String>| {
        value
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let current = Settings::load().unwrap_or_default();
    let folders = match values.folders {
        Some(folders) => Some(
            folders
                .into_iter()
                .map(|folder| folder.trim().to_string())
                .filter(|folder| !folder.is_empty())
                .collect::<Vec<_>>(),
        )
        .filter(|folders| !folders.is_empty()),
        None => current.folders,
    };
    Settings {
        server: filled(values.server),
        duration: filled(values.duration),
        guests: values.guests,
        domain: filled(values.domain),
        relay: filled(values.relay),
        join: filled(values.join),
        folders,
        up: filled(values.up),
        down: filled(values.down),
    }
    .save()
    .map_err(|error| format!("{error:#}"))
}

/// What this computer has for joining sessions: the helper, the device's
/// certificate authority.
#[derive(Serialize)]
struct Computer {
    /// `ready`, `absent`, or why the helper does not serve this user.
    helper: String,
    authority: Option<Authority>,
}

#[derive(Serialize)]
struct Authority {
    name: String,
    trusted: bool,
    domains: Vec<String>,
    days_left: i64,
    /// Whether it may vouch for the domain of the settings: an authority
    /// made before that domain was chosen does not, until renewed.
    covers_domain: bool,
}

fn domains() -> Vec<String> {
    vec![Settings::load().unwrap_or_default().domain()]
}

#[tauri::command]
async fn computer() -> Result<Computer, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let helper = match devshare_core::guest::Helper::connect() {
            Ok(Some(_)) => "ready".to_string(),
            Ok(None) => "absent".to_string(),
            Err(error) => format!("{error:#}"),
        };
        let domain = Settings::load().unwrap_or_default().domain();
        let authority = devshare_core::ca::DeviceCa::load(&domains())
            .ok()
            .flatten()
            .map(|ca| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_secs() as i64)
                    .unwrap_or_default();
                Authority {
                    name: ca.common_name().to_string(),
                    trusted: ca.trusted(),
                    covers_domain: ca.covers(&format!("project.{domain}")),
                    domains: ca.domains().to_vec(),
                    days_left: (ca.not_after() - now) / 86_400,
                }
            });
        Computer { helper, authority }
    })
    .await
    .map_err(|error| error.to_string())
}

/// Installs the helper: the system asks for an administrator's password.
#[tauri::command]
async fn install_helper() -> Result<(), String> {
    crate::system::install_helper().await
}

/// `install`, `renew` or `remove` this device's certificate authority. On
/// macOS the system asks for the user's password.
#[tauri::command]
async fn authority(action: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let domains = domains();
        let done = match action.as_str() {
            "install" => devshare_core::ca::manage::install(&domains).map(|_| ()),
            "renew" => devshare_core::ca::manage::renew(&domains).map(|_| ()),
            "remove" => devshare_core::ca::manage::remove(&domains).map(|_| ()),
            other => return Err(format!("no such action: {other}")),
        };
        done.map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn share<R: Runtime>(
    app: AppHandle<R>,
    paths: Vec<String>,
    minutes: u64,
    guests: u32,
) -> Result<(), String> {
    start_sharing(&app, paths, minutes, guests).await
}

/// Shares these projects: from the window's Share button, or the menu bar's.
pub(crate) async fn start_sharing<R: Runtime>(
    app: &AppHandle<R>,
    paths: Vec<String>,
    minutes: u64,
    guests: u32,
) -> Result<(), String> {
    if app.state::<Sharing>().current().is_some() {
        return Err("a session is already in progress".into());
    }
    if paths.is_empty() {
        return Err("switch on at least one project".into());
    }
    let folders: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    let projects = projects(app)?;
    let config = tauri::async_runtime::spawn_blocking(move || {
        projects.config(&folders, &discovery_options())
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| format!("{error:#}"))?;
    let settings = Settings::load().unwrap_or_default();
    devshare_core::link::use_relay(settings.relay.clone());
    let selection = config.select(&[]).map_err(|error| format!("{error:#}"))?;
    // One session, one control plane: the one asked for, else the one of
    // the general settings. When it is meant to be on this machine and is
    // not running, the app runs it itself: its owner never has to.
    let server = environment::server(std::env::var("DEVSHARE_SERVER").ok(), None, &settings);
    devshare_server::ensure_local(&server)
        .await
        .map_err(|error| format!("starting a control plane for {server}: {error}"))?;
    // 0 minutes: no time limit, until the owner stops sharing.
    let lifetime = match minutes {
        0 => Duration::from_secs(devshare_core::protocol::NO_LIMIT),
        minutes => Duration::from_secs(minutes.clamp(1, 24 * 60) * 60),
    };
    let options = ShareOptions {
        selection,
        lifetime,
        max_guests: guests.clamp(1, 50),
        server,
        join: settings.join(),
    };

    let window = app.clone();
    let session = Session::start(options, move |update| match update {
        Update::Session(snapshot) => {
            window.emit("session", snapshot).ok();
        }
        Update::Ended(reason) => {
            window.state::<Sharing>().0.lock().unwrap().take();
            window.emit("ended", reason).ok();
            crate::native::refresh(&window);
        }
    })
    .await
    .map_err(|error| format!("{error:#}"))?;

    *app.state::<Sharing>().0.lock().unwrap() = Some(session);
    crate::native::refresh(app);
    Ok(())
}

/// Joins a session with its invitation. The window is told about it through
/// the `joined` and `left` events.
#[tauri::command]
async fn join<R: Runtime>(
    app: AppHandle<R>,
    joining: State<'_, Joining>,
    invitation: String,
) -> Result<(), String> {
    if joining.0.lock().unwrap().is_some() {
        return Err("this computer is already in a session: leave it first".into());
    }
    let window = app.clone();
    let joined = Joined::start(
        invitation.trim(),
        std::env::var("DEVSHARE_SERVER").ok(),
        move |update| match update {
            JoinUpdate::Joined(view) => {
                window.emit("joined", view).ok();
            }
            JoinUpdate::Left(reason) => {
                window.state::<Joining>().0.lock().unwrap().take();
                window.emit("left", reason).ok();
            }
        },
    )
    .await
    .map_err(|error| format!("{error:#}"))?;
    *joining.0.lock().unwrap() = Some(joined);
    Ok(())
}

#[tauri::command]
async fn leave(joining: State<'_, Joining>) -> Result<(), String> {
    let joined = joining.0.lock().unwrap().clone();
    if let Some(joined) = joined {
        joined.leave().await;
    }
    Ok(())
}

/// Opens one of the joined session's addresses in the system's browser.
/// Nothing else: the window cannot open whatever a host sent.
#[tauri::command]
fn open(joining: State<'_, Joining>, url: String) -> Result<(), String> {
    let allowed = joining
        .0
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|joined| joined.may_open(&url));
    if !allowed {
        return Err("not an address of the session".into());
    }
    tauri_plugin_opener::open_url(&url, None::<&str>).map_err(|error| error.to_string())
}

/// The invitation a link handed over before the window was there to hear
/// it, once.
#[tauri::command]
fn handed(handed: State<'_, Handed>) -> Option<String> {
    handed.0.lock().unwrap().take()
}

#[tauri::command]
async fn disconnect(sharing: State<'_, Sharing>, guest: u32) -> Result<bool, String> {
    match sharing.current() {
        Some(session) => Ok(session.disconnect(guest).await),
        None => Ok(false),
    }
}

#[tauri::command]
async fn invite(sharing: State<'_, Sharing>) -> Result<(), String> {
    match sharing.current() {
        Some(session) => session.invite().await,
        None => Err("no session is in progress".into()),
    }
}

#[tauri::command]
async fn stop(sharing: State<'_, Sharing>) -> Result<(), String> {
    if let Some(session) = sharing.current() {
        session.stop().await;
    }
    Ok(())
}

/// The app, on the runtime it is given: the system's web view when it runs,
/// none at all when it is tested.
pub fn create<R: Runtime>(builder: tauri::Builder<R>) -> tauri::App<R> {
    builder
        .manage(Sharing::default())
        .manage(Joining::default())
        .manage(Handed::default())
        .invoke_handler(tauri::generate_handler![
            overview,
            switch,
            add_project,
            pick,
            add_source,
            remove_source,
            remove_project,
            restore_project,
            set_commands,
            open_local,
            reveal,
            hide_panel,
            show_window,
            quit,
            share,
            disconnect,
            invite,
            stop,
            join,
            leave,
            open,
            handed,
            running,
            check_project,
            run_project,
            settings,
            save_settings,
            computer,
            install_helper,
            authority
        ])
        .build(tauri::generate_context!())
        .expect("the DevShare window could not be created")
}

/// An invitation handed over by a link: shown in the window, which asks
/// before joining. A page must never be able to make this computer join a
/// session on its own.
pub fn hand_over<R: Runtime>(app: &AppHandle<R>, links: Vec<url::Url>) {
    let Some(invitation) = links.iter().find_map(invitation_of) else {
        return;
    };
    *app.state::<Handed>().0.lock().unwrap() = Some(invitation.clone());
    app.emit("invitation", invitation).ok();
    if let Some(window) = app.get_webview_window("main") {
        window.unminimize().ok();
        window.set_focus().ok();
    }
}

/// Quitting the app ends the session: guests are told before the process
/// goes, not left to notice a dead link. A joined session is left.
pub fn on_event<R: Runtime>(app: &AppHandle<R>, event: RunEvent) {
    // The Dock icon brings back a window that was closed.
    #[cfg(target_os = "macos")]
    if let RunEvent::Reopen { .. } = event {
        crate::native::show(app);
        return;
    }
    if let RunEvent::Exit = event {
        if let Some(session) = app.state::<Sharing>().current() {
            tauri::async_runtime::block_on(session.stop());
        }
        let joined = app.state::<Joining>().0.lock().unwrap().clone();
        if let Some(joined) = joined {
            tauri::async_runtime::block_on(joined.leave());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(link: &str) -> Option<String> {
        invitation_of(&url::Url::parse(link).unwrap())
    }

    #[test]
    fn an_invitation_comes_out_of_its_link_as_it_went_in() {
        let page = "https://join.glitchr.dev/#gVOxtm5P6ALN7P-x_y";
        let encoded: String = url::form_urlencoded::byte_serialize(page.as_bytes()).collect();
        assert_eq!(
            read(&format!("devshare://open?link={encoded}")).as_deref(),
            Some(page)
        );
        assert_eq!(
            read("devshare://open?link=7GX2-KLM9").as_deref(),
            Some("7GX2-KLM9")
        );
    }

    #[test]
    fn a_link_without_an_invitation_hands_nothing_over() {
        assert_eq!(read("devshare://open"), None);
        assert_eq!(read("devshare://open?link="), None);
        assert_eq!(read("devshare://open?link=7GX2%0A-KLM9"), None);
        assert_eq!(
            read(&format!("devshare://open?link={}", "a".repeat(3000))),
            None
        );
        assert_eq!(read("https://join.glitchr.dev/?link=7GX2-KLM9"), None);
    }
}
