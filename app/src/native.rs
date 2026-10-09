//! What makes the app a native one around its window: the menu bar's
//! commands with their usual shortcuts, and the menu bar icon that switches
//! projects and shares them without the window. The window's own requests
//! are in [`crate::commands`].

use tauri::{
    menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder},
    App, Emitter, Manager, Runtime,
};

/// The menu: the app's (Settings… ⌘,), Edit (copy and paste in the
/// fields), View (Show Sidebar ⌃⌘S), Window. Settings and the sidebar
/// toggle are said to the window as `sidebar` events.
pub fn menu<R: Runtime>(app: &App<R>) -> tauri::Result<()> {
    let settings = MenuItemBuilder::with_id("settings", "Settings…")
        .accelerator("CmdOrCtrl+,")
        .build(app)?;
    let sidebar = MenuItemBuilder::with_id("sidebar", "Show or Hide Sidebar")
        .accelerator("Ctrl+CmdOrCtrl+S")
        .build(app)?;
    let application = SubmenuBuilder::new(app, "DevShare")
        .about(None)
        .separator()
        .item(&settings)
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    let view = SubmenuBuilder::new(app, "View")
        .item(&sidebar)
        .separator()
        .fullscreen()
        .build()?;
    let window = SubmenuBuilder::new(app, "Window")
        .minimize()
        .maximize()
        .separator()
        .close_window()
        .build()?;
    let menu = MenuBuilder::new(app)
        .items(&[&application, &edit, &view, &window])
        .build()?;
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| match event.id().as_ref() {
        "settings" => {
            app.emit("sidebar", "open").ok();
        }
        "sidebar" => {
            app.emit("sidebar", "toggle").ok();
        }
        _ => {}
    });
    Ok(())
}

/// The menu bar icon's id.
const TRAY: &str = "devshare";

/// The menu bar icon: every project with a check to switch it on or off,
/// Share or Stop sharing, Open DevShare, Quit.
pub fn tray<R: Runtime>(app: &App<R>) -> tauri::Result<()> {
    let mut icon = tauri::tray::TrayIconBuilder::with_id(TRAY)
        .tooltip("DevShare")
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| chosen(app, event.id().as_ref()));
    if let Some(image) = app.default_window_icon() {
        icon = icon.icon(image.clone());
    }
    icon.build(app)?;
    refresh(app.handle());
    Ok(())
}

/// Rebuilds the menu bar icon's menu, from the list as it is now. In the
/// background: the list is read from the disk.
pub fn refresh<R: Runtime>(app: &tauri::AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(tray) = app.tray_by_id(TRAY) else {
            return;
        };
        let Ok(projects) = crate::commands::projects(&app) else {
            return;
        };
        let settings = devshare_core::environment::Settings::load().unwrap_or_default();
        let listing = projects.list(&settings, &crate::commands::discovery_options());
        let sharing = app.state::<crate::commands::Sharing>().current().is_some();
        if let Ok(menu) = build(&app, &listing.projects, sharing) {
            tray.set_menu(Some(menu)).ok();
        }
    });
}

fn build<R: Runtime>(
    app: &tauri::AppHandle<R>,
    projects: &[crate::projects::Project],
    sharing: bool,
) -> tauri::Result<tauri::menu::Menu<R>> {
    use tauri::menu::{CheckMenuItemBuilder, IsMenuItem, PredefinedMenuItem};

    let mut items: Vec<Box<dyn IsMenuItem<R>>> = Vec::new();
    for project in projects {
        let label = match &project.hostname {
            Some(hostname) => format!("{}  —  {hostname}", project.name),
            None => project.name.clone(),
        };
        let item = CheckMenuItemBuilder::with_id(format!("project:{}", project.folder), label)
            .checked(project.on)
            // While sharing, what is shared does not change.
            .enabled(project.problem.is_none() && !sharing)
            .build(app)?;
        items.push(Box::new(item));
    }
    items.push(Box::new(PredefinedMenuItem::separator(app)?));
    let switched = projects
        .iter()
        .filter(|project| project.on && project.problem.is_none())
        .count();
    let action = if sharing {
        MenuItemBuilder::with_id("stop", "Stop sharing").build(app)?
    } else {
        let label = match switched {
            0 => "Share".to_string(),
            1 => "Share 1 project".to_string(),
            many => format!("Share {many} projects"),
        };
        MenuItemBuilder::with_id("share", label)
            .enabled(switched > 0)
            .build(app)?
    };
    items.push(Box::new(action));
    items.push(Box::new(PredefinedMenuItem::separator(app)?));
    items.push(Box::new(
        MenuItemBuilder::with_id("open", "Open DevShare").build(app)?,
    ));
    items.push(Box::new(
        MenuItemBuilder::with_id("quit", "Quit DevShare").build(app)?,
    ));
    let references: Vec<&dyn IsMenuItem<R>> = items.iter().map(|item| item.as_ref()).collect();
    MenuBuilder::new(app).items(&references).build()
}

/// What was chosen in the menu bar icon's menu.
fn chosen<R: Runtime>(app: &tauri::AppHandle<R>, id: &str) {
    if let Some(path) = id.strip_prefix("project:") {
        if let Ok(projects) = crate::commands::projects(app) {
            let path = std::path::PathBuf::from(path);
            let on = projects.switched_on().contains(&path);
            projects.switch(&path, !on).ok();
            app.emit("projects", ()).ok();
            refresh(app);
        }
        return;
    }
    match id {
        "share" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let Ok(projects) = crate::commands::projects(&app) else {
                    return;
                };
                let paths = projects
                    .switched_on()
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect();
                let (minutes, guests) = crate::commands::usual();
                if let Err(error) =
                    crate::commands::start_sharing(&app, paths, minutes, guests).await
                {
                    app.emit("trouble", error).ok();
                    show(&app);
                }
            });
        }
        "stop" => {
            if let Some(session) = app.state::<crate::commands::Sharing>().current() {
                tauri::async_runtime::spawn(async move { session.stop().await });
            }
        }
        "open" => show(app),
        "quit" => app.exit(0),
        _ => {}
    }
}

/// Brings the window back, wherever it was.
pub fn show<R: Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        window.show().ok();
        window.unminimize().ok();
        window.set_focus().ok();
    }
}
