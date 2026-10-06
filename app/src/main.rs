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

    commands::create(tauri::Builder::default()).run(commands::on_event);
}
