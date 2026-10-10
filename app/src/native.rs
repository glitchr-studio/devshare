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
            app.emit_to("main", "settings", ()).ok();
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
/// The window that drops down from it.
pub const PANEL: &str = "panel";

/// The menu bar icon. A click drops the panel down under it: the projects
/// with a switch each, Share or Stop sharing, the window, Quit. A right
/// click gives the same in a plain menu.
pub fn tray<R: Runtime>(app: &App<R>) -> tauri::Result<()> {
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let menu = MenuBuilder::new(app)
        .item(&MenuItemBuilder::with_id("open", "Open DevShare").build(app)?)
        .item(&MenuItemBuilder::with_id("settings", "Settings…").build(app)?)
        .separator()
        .item(&MenuItemBuilder::with_id("quit", "Quit DevShare").build(app)?)
        .build()?;
    let mut icon = TrayIconBuilder::with_id(TRAY)
        .tooltip("DevShare")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show(app),
            "settings" => {
                show(app);
                app.emit_to("main", "settings", ()).ok();
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                toggle_panel(tray.app_handle(), rect);
            }
        });
    // A template image of its own, the glyph filling the frame: the app's
    // icon has margins that leave it small in the menu bar.
    match tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png")) {
        Ok(image) => icon = icon.icon(image).icon_as_template(true),
        Err(error) => tracing::warn!("no menu bar icon: {error}"),
    }
    icon.build(app)?;
    Ok(())
}

/// The menu bar icon turns while something is under way: a project starting
/// or stopping, a session being opened or joined, a certificate issued.
/// Each such action holds a [`Busy`] for as long as it lasts.
static UNDER_WAY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

const FRAMES: [&[u8]; 8] = [
    include_bytes!("../icons/spin/0.png"),
    include_bytes!("../icons/spin/1.png"),
    include_bytes!("../icons/spin/2.png"),
    include_bytes!("../icons/spin/3.png"),
    include_bytes!("../icons/spin/4.png"),
    include_bytes!("../icons/spin/5.png"),
    include_bytes!("../icons/spin/6.png"),
    include_bytes!("../icons/spin/7.png"),
];

pub struct Busy;

impl Drop for Busy {
    fn drop(&mut self) {
        UNDER_WAY.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Something under way, until the returned guard goes.
pub fn busy<R: Runtime>(app: &tauri::AppHandle<R>) -> Busy {
    use std::sync::atomic::Ordering;
    if UNDER_WAY.fetch_add(1, Ordering::SeqCst) == 0 {
        let app = app.clone();
        std::thread::spawn(move || {
            let Some(tray) = app.tray_by_id(TRAY) else {
                return;
            };
            let frames: Vec<_> = FRAMES
                .iter()
                .filter_map(|bytes| tauri::image::Image::from_bytes(bytes).ok())
                .collect();
            let mut frame = 0;
            while UNDER_WAY.load(Ordering::SeqCst) > 0 && !frames.is_empty() {
                tray.set_icon(Some(frames[frame % frames.len()].clone())).ok();
                tray.set_icon_as_template(true).ok();
                frame += 1;
                std::thread::sleep(std::time::Duration::from_millis(90));
            }
            if let Ok(still) = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png")) {
                tray.set_icon(Some(still)).ok();
                tray.set_icon_as_template(true).ok();
            }
        });
    }
    Busy
}

/// Drops the panel down under the menu bar icon, or puts it away.
fn toggle_panel<R: Runtime>(app: &tauri::AppHandle<R>, rect: tauri::Rect) {
    let Some(panel) = app.get_webview_window(PANEL) else {
        return;
    };
    if panel.is_visible().unwrap_or(false) {
        panel.hide().ok();
        return;
    }
    let scale = panel.scale_factor().unwrap_or(1.0);
    let icon = rect.position.to_physical::<f64>(scale);
    let size = rect.size.to_physical::<f64>(scale);
    let width = panel
        .outer_size()
        .map(|size| size.width as f64)
        .unwrap_or(360.0 * scale);
    let x = icon.x + size.width / 2.0 - width / 2.0;
    let y = icon.y + size.height + 6.0 * scale;
    panel.set_position(tauri::PhysicalPosition::new(x, y)).ok();
    panel.emit("refresh", ()).ok();
    panel.show().ok();
    panel.set_focus().ok();
}

/// Tells the panel and the window that the session changed, so that both
/// show it.
pub fn refresh<R: Runtime>(app: &tauri::AppHandle<R>) {
    changed(app, "");
}

/// Tells the panel and the window that the projects changed, and who did
/// it: the window that did ignores its own change, the other shows it.
pub fn changed<R: Runtime>(app: &tauri::AppHandle<R>, by: &str) {
    app.emit("changed", by).ok();
}

/// Brings the window back, wherever it was.
pub fn show<R: Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        window.show().ok();
        window.unminimize().ok();
        window.set_focus().ok();
    }
}
