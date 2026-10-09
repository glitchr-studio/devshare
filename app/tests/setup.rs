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

    // A found project taken off the list stays off, said apart, until put
    // back; one with nothing to share is not listed in the first place.
    let empty = home.join("Sites/empty");
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::write(
        empty.join("compose.yaml"),
        "services:\n  worker:\n    image: busybox\n",
    )
    .unwrap();
    let overview = ask(&window, "overview", json!({})).unwrap();
    assert_eq!(
        overview["projects"].as_array().unwrap().len(),
        1,
        "{overview}"
    );
    assert_eq!(overview["hidden"][0]["why"], "nothing to share");
    let shop_folder = overview["projects"][0]["folder"].clone();
    ask(&window, "remove_project", json!({ "path": shop_folder })).unwrap();
    let overview = ask(&window, "overview", json!({})).unwrap();
    assert_eq!(overview["projects"], json!([]));
    let whys: Vec<&str> = overview["hidden"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hidden| hidden["why"].as_str().unwrap())
        .collect();
    assert_eq!(whys, ["nothing to share", "taken off the list"]);
    ask(&window, "restore_project", json!({ "path": shop_folder })).unwrap();
    assert_eq!(
        ask(&window, "overview", json!({})).unwrap()["projects"][0]["name"],
        "shop"
    );

    // A devshare.toml of any name, anywhere: a project of its own.
    let custom = home.join("configs/staging.toml");
    std::fs::create_dir_all(custom.parent().unwrap()).unwrap();
    std::fs::write(&custom, "[environments.staging]\nservices = [{ host = \"staging.local\", port = 8443, target = \"127.0.0.1:1\" }]\n").unwrap();
    assert_eq!(
        ask(&window, "add_project", json!({ "path": custom })).unwrap(),
        "staging.toml"
    );
    let overview = ask(&window, "overview", json!({})).unwrap();
    let staging = overview["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["name"] == "staging")
        .unwrap();
    assert_eq!(
        (&staging["hostname"], &staging["on"]),
        (&json!("staging.local"), &json!(true))
    );
    assert!(ask(
        &window,
        "add_project",
        json!({ "path": home.join("configs/missing.toml") })
    )
    .is_err());

    // A folder holding projects becomes a source; sources come and go.
    let work = home.join("Work/api");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("vite.config.ts"), "export default {}\n").unwrap();
    let said = ask(&window, "add_project", json!({ "path": home.join("Work") })).unwrap();
    assert!(said.as_str().unwrap().starts_with("projects in"), "{said}");
    let overview = ask(&window, "overview", json!({})).unwrap();
    assert_eq!(
        overview["folders"].as_array().unwrap().len(),
        2,
        "~/Sites kept, ~/Work added: {overview}"
    );
    assert!(overview["projects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|project| project["name"] == "api"));
    ask(
        &window,
        "remove_source",
        json!({ "path": home.join("Work") }),
    )
    .unwrap();
    let overview = ask(&window, "overview", json!({})).unwrap();
    assert!(!overview["projects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|project| project["name"] == "api"));
    let written = std::fs::read_to_string(&settings).unwrap();
    assert!(written.contains("folders = [\"~/Sites\"]"), "{written}");

    // Looked for where the settings say, and only there.
    let elsewhere = home.join("Elsewhere/blog");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("vite.config.ts"), "export default {}\n").unwrap();
    // The window sends every field: here the relay as it was.
    let values = json!({ "values": { "folders": ["~/Elsewhere"], "relay": "disabled" } });
    ask(&window, "save_settings", values).unwrap();
    let overview = ask(&window, "overview", json!({})).unwrap();
    // What was added by hand stays, whatever the sources.
    let names: Vec<&str> = overview["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|project| project["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["blog", "staging"], "{overview}");

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

    // How a project starts: the owner's own command for it, else the
    // defaults of the settings, else what it is.
    let shop_path = shop.canonicalize().unwrap().display().to_string();
    let values = json!({ "values": { "folders": ["~/Sites"], "up": "make start" } });
    ask(&window, "save_settings", values).unwrap();
    let find = |overview: &Value, name: &str| -> Value {
        overview["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|project| project["name"] == name)
            .cloned()
            .unwrap()
    };
    let overview = ask(&window, "overview", json!({})).unwrap();
    assert_eq!(
        find(&overview, "shop")["usual_up"],
        "make start",
        "{overview}"
    );
    assert_eq!(
        find(&overview, "shop")["usual_down"],
        "docker compose down",
        "no default to stop: Compose"
    );
    ask(
        &window,
        "set_commands",
        json!({ "path": shop_path, "up": "echo mine > mine", "down": null }),
    )
    .unwrap();
    let overview = ask(&window, "overview", json!({})).unwrap();
    assert_eq!(find(&overview, "shop")["local_up"], "echo mine > mine");
    ask(
        &window,
        "run_project",
        json!({ "path": shop_path, "action": "up" }),
    )
    .unwrap();
    assert!(
        shop.join("mine").is_file(),
        "the owner's own command ran in the project's folder"
    );
    // Emptied: the usual one again.
    ask(
        &window,
        "set_commands",
        json!({ "path": shop_path, "up": "  ", "down": null }),
    )
    .unwrap();
    assert_eq!(
        find(&ask(&window, "overview", json!({})).unwrap(), "shop")["local_up"],
        Value::Null
    );
    // Saving the settings without the folders leaves them as they are.
    ask(
        &window,
        "save_settings",
        json!({ "values": { "guests": 2 } }),
    )
    .unwrap();
    assert!(std::fs::read_to_string(&settings)
        .unwrap()
        .contains("folders = [\"~/Sites\"]"));

    std::fs::remove_dir_all(&home).ok();
}
