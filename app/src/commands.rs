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
    projects::Projects,
    session::{Session, Update},
};

/// The session in progress, if any.
#[derive(Default)]
struct Sharing(Mutex<Option<Session>>);

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
    let options = discover::Options {
        hostname: None,
        domain: Settings::load().ok().map(|settings| settings.domain()),
        environment: std::env::vars().collect(),
        hosts: Some("/etc/hosts".into()),
    };
    let name = match discover::discover(&folder, &options) {
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
        .invoke_handler(tauri::generate_handler![
            declared,
            add_project,
            remove_project,
            share,
            disconnect,
            invite,
            stop
        ])
        .build(tauri::generate_context!())
        .expect("the DevShare window could not be created")
}

/// Quitting the app ends the session: guests are told before the process
/// goes, not left to notice a dead link.
pub fn on_event<R: Runtime>(app: &AppHandle<R>, event: RunEvent) {
    if let RunEvent::Exit = event {
        if let Some(session) = app.state::<Sharing>().current() {
            tauri::async_runtime::block_on(session.stop());
        }
    }
}
