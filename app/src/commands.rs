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
    projects::Projects,
    session::{Session, Update},
};

/// The scheme the invitation pages hand an invitation to the app with:
/// `devshare://open?link=<the invitation, URL-encoded>`.
pub const SCHEME: &str = "devshare";

/// The longest invitation taken from a link: a few times the longest one.
const LONGEST_INVITATION: usize = 2048;

/// The session in progress, if any.
#[derive(Default)]
struct Sharing(Mutex<Option<Session>>);

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
    fn current(&self) -> Option<Session> {
        self.0.lock().unwrap().clone()
    }
}

/// The app's list of projects, kept with the app's data. `DEVSHARE_APP_DATA`
/// names another folder for it.
fn projects<R: Runtime>(app: &AppHandle<R>) -> Result<Projects, String> {
    let folder = match std::env::var_os("DEVSHARE_APP_DATA") {
        Some(folder) => PathBuf::from(folder),
        None => app
            .path()
            .app_data_dir()
            .map_err(|error| format!("no folder for the app's data: {error}"))?,
    };
    Ok(Projects::in_folder(folder))
}

/// What the owner can choose from before sharing.
#[derive(Serialize)]
struct Declared {
    environments: Vec<DeclaredEnvironment>,
    /// What could not be read of a project, and the folder it is about.
    problems: Vec<Problem>,
    /// The general settings: where they are, and the defaults they give.
    settings: String,
    minutes: u64,
    guests: u32,
}

#[derive(Serialize)]
struct DeclaredEnvironment {
    name: String,
    services: Vec<String>,
    /// The project's folder.
    folder: String,
}

#[derive(Serialize)]
struct Problem {
    text: String,
    folder: Option<String>,
}

#[tauri::command]
fn declared<R: Runtime>(app: AppHandle<R>) -> Result<Declared, String> {
    let found = projects(&app)?.declared();
    let mut problems: Vec<Problem> = found
        .problems
        .into_iter()
        .map(|(folder, text)| Problem {
            text,
            folder: Some(folder.display().to_string()),
        })
        .collect();
    // Settings that cannot be read are said, and the defaults are used.
    let settings = Settings::load().unwrap_or_else(|error| {
        problems.push(Problem {
            text: format!("{error:#}"),
            folder: None,
        });
        Settings::default()
    });
    let minutes = settings
        .duration()
        .map_or(5, |duration| duration.as_secs().div_ceil(60));

    Ok(Declared {
        environments: found
            .environments
            .into_iter()
            .map(|(folder, name, services)| DeclaredEnvironment {
                name,
                services,
                folder: folder.display().to_string(),
            })
            .collect(),
        problems,
        settings: Settings::file().display().to_string(),
        minutes,
        guests: settings.guests(),
    })
}

/// Adds a project by its folder. Its `devshare.toml` is written from its
/// compose file, unless the owner wrote one by hand: that one is used as it
/// is. Returns the name of the project.
#[tauri::command]
fn add_project<R: Runtime>(app: AppHandle<R>, path: String) -> Result<String, String> {
    let folder = PathBuf::from(path.trim());
    let by_hand = folder.join(discover::FILE).is_file();
    let name = match discover::discover(&folder, &discovery_options()) {
        Ok(found) => {
            if let Err(error) = discover::write(&folder, &found, false) {
                // A file written by hand stays; anything else is a refusal.
                if !by_hand {
                    return Err(format!("{error:#}"));
                }
            }
            found.project
        }
        // No compose file, but a devshare.toml of the owner's own.
        Err(_) if by_hand => folder
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        Err(error) => return Err(format!("{error:#}")),
    };
    projects(&app)?
        .add(&folder)
        .map_err(|error| format!("{error:#}"))?;
    Ok(name)
}

/// A project found on this computer and not on the list yet.
#[derive(Serialize)]
struct Candidate {
    folder: String,
    name: String,
    /// The name guests would reach it under.
    hostname: Option<String>,
}

fn discovery_options() -> discover::Options {
    discover::Options {
        hostname: None,
        domain: Settings::load().ok().map(|settings| settings.domain()),
        environment: std::env::vars().collect(),
        hosts: Some("/etc/hosts".into()),
    }
}

/// The projects found where projects are kept, those already added left out.
#[tauri::command]
async fn candidates<R: Runtime>(app: AppHandle<R>) -> Result<Vec<Candidate>, String> {
    let known = projects(&app)?.folders();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        let options = discovery_options();
        discover::candidates(&known, &home)
            .into_iter()
            .map(|folder| {
                let found = discover::discover(&folder, &options).ok();
                Candidate {
                    name: found
                        .as_ref()
                        .map(|found| found.project.clone())
                        .or_else(|| {
                            folder
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                        })
                        .unwrap_or_default(),
                    hostname: found.map(|found| found.hostname),
                    folder: folder.display().to_string(),
                }
            })
            .collect()
    })
    .await
    .map_err(|error| error.to_string())
}

/// The environments whose services answer on this machine: the others are
/// not started.
#[tauri::command]
async fn running<R: Runtime>(app: AppHandle<R>) -> Result<Vec<String>, String> {
    let config = projects(&app)?.declared().config;
    tauri::async_runtime::spawn_blocking(move || {
        config
            .environments
            .iter()
            .filter(|(_, environment)| {
                environment.services.iter().any(|service| {
                    let target = service
                        .target
                        .clone()
                        .unwrap_or_else(|| format!("127.0.0.1:{}", service.port));
                    std::net::ToSocketAddrs::to_socket_addrs(&target)
                        .ok()
                        .and_then(|mut addresses| addresses.next())
                        .is_some_and(|address| {
                            std::net::TcpStream::connect_timeout(
                                &address,
                                Duration::from_millis(300),
                            )
                            .is_ok()
                        })
                })
            })
            .map(|(name, _)| name.clone())
            .collect()
    })
    .await
    .map_err(|error| error.to_string())
}

/// Starts a project's services: `docker compose up -d` in its folder.
#[tauri::command]
async fn start_project(path: String) -> Result<(), String> {
    let folder = PathBuf::from(path);
    if !discover::has_compose_file(&folder) {
        return Err("this project has no compose file: start it as you usually do".into());
    }
    let output = tokio::process::Command::new(crate::system::program("docker"))
        .args(["compose", "up", "-d"])
        .current_dir(&folder)
        .output()
        .await
        .map_err(|error| {
            format!("Docker could not be run ({error}): is Docker Desktop installed?")
        })?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let last: Vec<&str> = said
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    Err(last[last.len().saturating_sub(4)..].join("\n"))
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
    Settings {
        server: filled(values.server),
        duration: filled(values.duration),
        guests: values.guests,
        domain: filled(values.domain),
        relay: filled(values.relay),
        join: filled(values.join),
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

/// Takes a project off the list. Its folder is left as it is.
#[tauri::command]
fn remove_project<R: Runtime>(app: AppHandle<R>, path: String) -> Result<(), String> {
    projects(&app)?
        .remove(&PathBuf::from(path))
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
async fn share<R: Runtime>(
    app: AppHandle<R>,
    sharing: State<'_, Sharing>,
    environments: Vec<String>,
    minutes: u64,
    guests: u32,
) -> Result<(), String> {
    if sharing.current().is_some() {
        return Err("a session is already in progress".into());
    }
    if environments.is_empty() {
        return Err("choose at least one environment".into());
    }
    let config = projects(&app)?.declared().config;
    let settings = Settings::load().unwrap_or_default();
    devshare_core::link::use_relay(settings.relay.clone());
    let selection = config
        .select(&environments)
        .map_err(|error| format!("{error:#}"))?;
    // One session, one control plane: the one asked for, else the one of
    // the general settings. When it is meant to be on this machine and is
    // not running, the app runs it itself: its owner never has to.
    let server = environment::server(std::env::var("DEVSHARE_SERVER").ok(), None, &settings);
    devshare_server::ensure_local(&server)
        .await
        .map_err(|error| format!("starting a control plane for {server}: {error}"))?;
    let options = ShareOptions {
        selection,
        lifetime: Duration::from_secs(minutes.clamp(1, 24 * 60) * 60),
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
        }
    })
    .await
    .map_err(|error| format!("{error:#}"))?;

    *sharing.0.lock().unwrap() = Some(session);
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
            declared,
            add_project,
            remove_project,
            share,
            disconnect,
            invite,
            stop,
            join,
            leave,
            open,
            handed,
            candidates,
            running,
            start_project,
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
