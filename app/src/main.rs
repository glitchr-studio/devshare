// No console window behind the app on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use devshare_app::commands;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "off,devshare_app=warn,devshare_core=warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let builder = tauri::Builder::default();
    // A second launch, by a link on Linux or Windows, hands its link to the
    // running app and ends.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(
        |app, _arguments, _folder| {
            use tauri::Manager;
            if let Some(window) = app.get_webview_window("main") {
                window.set_focus().ok();
            }
        },
    ));
    let builder = builder
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            use tauri_plugin_deep_link::DeepLinkExt;
            devshare_app::native::menu(app)?;
            devshare_app::native::tray(app)?;
            devshare_app::commands::sync_names(app.handle());
            // Registered at each start where the system allows it: an app
            // run from its build folder has no installer to do it.
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            app.deep_link().register_all().ok();
            let handle = app.handle().clone();
            app.deep_link()
                .on_open_url(move |event| commands::hand_over(&handle, event.urls()));
            if let Ok(Some(links)) = app.deep_link().get_current() {
                commands::hand_over(app.handle(), links);
            }
            Ok(())
        });
    // Closing the window hides it: the menu bar icon keeps the app at hand,
    // and Quit (⌘Q) ends it.
    let builder = builder.on_window_event(|window, event| match event {
        tauri::WindowEvent::CloseRequested { api, .. } => {
            window.hide().ok();
            api.prevent_close();
        }
        // The panel goes away with the first click elsewhere.
        tauri::WindowEvent::Focused(false) if window.label() == devshare_app::native::PANEL => {
            window.hide().ok();
        }
        _ => {}
    });
    commands::create(builder).run(commands::on_event);
}
