//! What the window asks before any session: the projects found on this
//! computer, the general settings, starting a project, and what it cannot
//! ask. A process of its own: it points HOME and the settings elsewhere.

use devshare_app::commands;
use serde_json::{json, Value};
use tauri::{
    ipc::{CallbackFn, InvokeBody},
    test::{get_ipc_response, mock_builder, MockRuntime, INVOKE_KEY},
    webview::InvokeRequest,
    WebviewWindow, WebviewWindowBuilder,
};

fn ask(
    window: &WebviewWindow<MockRuntime>,
    command: &str,
    arguments: Value,
) -> Result<Value, Value> {
    get_ipc_response(
        window,
        InvokeRequest {
            cmd: command.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: InvokeBody::Json(arguments),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
    .map(|body| body.deserialize::<Value>().unwrap())
}

#[test]
fn the_window_finds_projects_edits_the_settings_and_starts_nothing_it_cannot() {
    let home = std::env::temp_dir().join(format!("devshare-app-home-{}", std::process::id()));
    std::fs::remove_dir_all(&home).ok();
    let shop = home.join("Sites/shop");
    std::fs::create_dir_all(&shop).unwrap();
    std::fs::write(
        shop.join("compose.yaml"),
        "services:\n  web:\n    image: nginx\n    ports: [\"8080:80\"]\n",
    )
    .unwrap();
    std::fs::write(shop.join(".env"), "SSL_CERT_DOMAINS=localhost,shop.local\n").unwrap();
    std::fs::create_dir_all(home.join("Sites/notes")).unwrap();
    let settings = home.join("devshare.toml");
    std::fs::write(&settings, "# mine\nrelay = \"disabled\"\n").unwrap();
    // SAFETY: set before anything else of this process reads them, in the
    // only test of this binary.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("DEVSHARE_SETTINGS", &settings);
        std::env::set_var("DEVSHARE_APP_DATA", home.join("app"));
    }

    let app = commands::create(mock_builder());
    let window = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();

    // Found in ~/Sites, under the name its .env gives it, switched off.
    let overview = ask(&window, "overview", json!({})).unwrap();
    let found = &overview["projects"];
    assert_eq!(found.as_array().unwrap().len(), 1, "{overview}");
    assert_eq!(found[0]["hostname"], "shop.local");
    assert_eq!(
        (&found[0]["on"], &found[0]["added"]),
        (&json!(false), &json!(false))
    );
    assert_eq!(
        found[0]["startable"], true,
        "a compose file: docker compose up -d"
    );
    assert!(overview["folders"][0].as_str().unwrap().ends_with("Sites"));
    // Switched on, it is remembered; nothing is written in its folder.
    ask(
        &window,
        "switch",
        json!({ "path": found[0]["folder"], "on": true }),
    )
    .unwrap();
    let overview = ask(&window, "overview", json!({})).unwrap();
    assert_eq!(overview["projects"][0]["on"], true);
    assert!(!shop.join("devshare.toml").exists());

    // Looked for where the settings say, and only there.
    let elsewhere = home.join("Elsewhere/blog");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("vite.config.ts"), "export default {}\n").unwrap();
    // The window sends every field: here the relay as it was.
    let values = json!({ "values": { "folders": ["~/Elsewhere"], "relay": "disabled" } });
    ask(&window, "save_settings", values).unwrap();
    let overview = ask(&window, "overview", json!({})).unwrap();
    assert_eq!(overview["projects"][0]["name"], "blog", "{overview}");
    assert_eq!(overview["projects"].as_array().unwrap().len(), 1);

    // The settings, read and written in place, comments kept.
    assert_eq!(
        ask(&window, "settings", json!({})).unwrap()["relay"],
        "disabled"
    );
    let values = json!({ "values": {
        "duration": "2h", "guests": 4, "domain": "local", "relay": null, "server": null, "join": null
    }});
    ask(&window, "save_settings", values).unwrap();
    let written = std::fs::read_to_string(&settings).unwrap();
    assert!(
        written.contains("# mine") && written.contains("duration = \"2h\""),
        "{written}"
    );
    assert!(!written.contains("relay"), "{written}");
    let overview = ask(&window, "overview", json!({})).unwrap();
    assert_eq!(overview["minutes"], 120);
    // No time limit, said as 0 minutes.
    ask(
        &window,
        "save_settings",
        json!({ "values": { "duration": "none" } }),
    )
    .unwrap();
    assert_eq!(ask(&window, "overview", json!({})).unwrap()["minutes"], 0);
    let refused = ask(
        &window,
        "save_settings",
        json!({ "values": { "duration": "soon" } }),
    )
    .unwrap_err();
    assert!(refused.as_str().unwrap().contains("duration"), "{refused}");

    // Nothing to start without a compose file; no action it does not know.
    let notes = home.join("Sites/notes").display().to_string();
    let error = ask(
        &window,
        "run_project",
        json!({ "path": notes, "action": "up" }),
    )
    .unwrap_err();
    assert!(
        error
            .as_str()
            .unwrap()
            .contains("does not know how to start"),
        "{error}"
    );
    let error = ask(
        &window,
        "run_project",
        json!({ "path": notes, "action": "explode" }),
    )
    .unwrap_err();
    assert!(
        error.as_str().unwrap().contains("no such action"),
        "{error}"
    );
    // A project's own command, in its folder: its failure is said.
    let made = home.join("Sites/made");
    std::fs::create_dir_all(&made).unwrap();
    std::fs::write(
        made.join("Makefile"),
        "up:\n\t@echo started > started\ndown:\n\t@echo broken >&2; exit 2\n",
    )
    .unwrap();
    let path = made.display().to_string();
    ask(
        &window,
        "run_project",
        json!({ "path": path, "action": "up" }),
    )
    .unwrap();
    assert!(
        made.join("started").is_file(),
        "make up ran in the project's folder"
    );
    let error = ask(
        &window,
        "run_project",
        json!({ "path": path, "action": "down" }),
    )
    .unwrap_err();
    assert!(
        error.as_str().unwrap().contains("make down failed")
            && error.as_str().unwrap().contains("broken"),
        "{error}"
    );
    let error = ask(&window, "authority", json!({ "action": "explode" })).unwrap_err();
    assert!(
        error.as_str().unwrap().contains("no such action"),
        "{error}"
    );
    // The computer's state is always said, whatever it is.
    let computer = ask(&window, "computer", json!({})).unwrap();
    assert!(computer["helper"].is_string(), "{computer}");

    std::fs::remove_dir_all(&home).ok();
}
