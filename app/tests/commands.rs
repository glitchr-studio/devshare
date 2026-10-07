//! The requests of the window, sent the way the window sends them, to the
//! app on Tauri's test runtime: no web view, but the real commands, the real
//! events and a real session.

use std::{
    sync::mpsc::{channel, Receiver},
    time::Duration,
};

use devshare_app::commands;
use devshare_core::{
    guest::{End, GuestLink},
    protocol::{Device, EndReason},
};
use serde_json::{json, Value};
use tauri::{
    ipc::{CallbackFn, InvokeBody},
    test::{get_ipc_response, mock_builder, MockRuntime, INVOKE_KEY},
    webview::InvokeRequest,
    Listener, WebviewWindow, WebviewWindowBuilder,
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

/// The next `session` event for which `wanted` holds.
fn until(events: &Receiver<Value>, wanted: impl Fn(&Value) -> bool) -> Value {
    loop {
        let session = events
            .recv_timeout(Duration::from_secs(8))
            .expect("the window was never sent what was expected");
        if wanted(&session) {
            return session;
        }
    }
}

#[test]
fn the_window_shares_sees_a_login_and_disconnects_it() {
    let folder = std::env::temp_dir().join(format!("devshare-app-test-{}", std::process::id()));
    std::fs::remove_dir_all(&folder).ok();
    // A project whose devshare.toml its owner wrote, without a compose file.
    let site = folder.join("site");
    std::fs::create_dir_all(&site).unwrap();
    let general = folder.join("settings.toml");

    // A control plane and one local service, on the app's own runtime.
    let (server, service) = tauri::async_runtime::block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(devshare_server::serve(listener));
        let service = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = service.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                service.accept().await.ok();
            }
        });
        (server, address)
    });
    std::fs::write(
        site.join("devshare.toml"),
        format!(
            "[environments.shop]\nservices = [{{ host = \"shop.test\", port = 80, target = \"{service}\" }}]\n\n[environments.admin]\nservices = [{{ host = \"admin.test\", port = 80, target = \"{service}\" }}]\n"
        ),
    )
    .unwrap();
    // The general settings: the control plane, and other defaults than the app's.
    std::fs::write(
        &general,
        format!("server = \"{server}\"\nduration = \"10m\"\nguests = 2\ndomain = \"lan\"\n"),
    )
    .unwrap();
    std::env::set_var("DEVSHARE_SETTINGS", &general);
    std::env::set_var("DEVSHARE_APP_DATA", folder.join("app-data"));
    std::env::set_var("DEVSHARE_RELAY", "disabled");
    std::env::remove_var("DEVSHARE_SERVER");

    let app = commands::create(mock_builder());
    let window = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let (sessions, session_events) = channel();
    app.listen("session", move |event| {
        sessions
            .send(serde_json::from_str(event.payload()).unwrap())
            .ok();
    });
    let (endings, ending_events) = channel();
    app.listen("ended", move |event| {
        endings
            .send(serde_json::from_str::<Value>(event.payload()).unwrap())
            .ok();
    });

    // Nothing at first, and the defaults of the general settings.
    let declared = ask(&window, "declared", json!({})).unwrap();
    assert_eq!(declared["environments"], json!([]));
    assert_eq!(
        (&declared["minutes"], &declared["guests"]),
        (&json!(10), &json!(2))
    );
    assert_eq!(declared["settings"], general.display().to_string());

    // The owner adds the project by its folder.
    assert_eq!(
        ask(&window, "add_project", json!({ "path": site })),
        Ok(json!("site"))
    );
    let declared = ask(&window, "declared", json!({})).unwrap();
    assert_eq!(declared["environments"][0]["name"], "admin");
    assert_eq!(declared["environments"][1]["services"][0], "shop.test:80");
    assert_eq!(declared["problems"], json!([]));

    // Nothing chosen is refused; then the Share button.
    assert!(ask(
        &window,
        "share",
        json!({ "environments": [], "minutes": 5, "guests": 2 })
    )
    .is_err());
    let share = json!({ "environments": ["shop"], "minutes": 5, "guests": 2 });
    assert_eq!(ask(&window, "share", share.clone()), Ok(Value::Null));
    assert!(
        ask(&window, "share", share).is_err(),
        "one session at a time"
    );

    let started = until(&session_events, |_| true);
    let code = started["code"].as_str().unwrap().to_string();
    assert_eq!(started["environments"].as_array().unwrap().len(), 1);
    assert_eq!(started["remaining"].as_u64().unwrap() / 60, 4);

    // Someone joins: the window is sent their login.
    let alice = Device {
        name: "Alices-MacBook".into(),
        platform: "macos".into(),
        user: Some("alice".into()),
    };
    let mut link = tauri::async_runtime::block_on(GuestLink::join(&code, &server, alice)).unwrap();
    let joined = until(&session_events, |session| {
        session["guests"][0]["user"] == "alice"
    });
    assert_eq!(joined["guests"][0]["computer"], "Alices-MacBook");
    let guest = joined["guests"][0]["id"].clone();

    // The Disconnect button.
    assert_eq!(
        ask(&window, "disconnect", json!({ "guest": guest })),
        Ok(json!(true))
    );
    let end = tauri::async_runtime::block_on(async {
        tokio::time::timeout(Duration::from_secs(5), link.ended())
            .await
            .unwrap()
    });
    assert_eq!(end, End::Host(EndReason::Revoked));
    // The invitation stays open: only that device is turned away.
    let after = until(&session_events, |session| {
        session["guests"].as_array().unwrap().is_empty()
    });
    assert_eq!(after["invitation_open"], true);

    // The New invitation button.
    assert_eq!(ask(&window, "invite", json!({})), Ok(Value::Null));
    let invited = until(&session_events, |session| {
        session["code"] != started["code"]
    });
    assert_eq!(invited["invitation_open"], true);
    assert_ne!(invited["code"], started["code"]);

    // The Stop sharing button.
    assert_eq!(ask(&window, "stop", json!({})), Ok(Value::Null));
    let reason = ending_events.recv_timeout(Duration::from_secs(8)).unwrap();
    assert_eq!(reason, "you stopped sharing");
    // The app is ready for another session.
    assert!(ask(&window, "invite", json!({})).is_err());

    // A project with a compose file: its devshare.toml is written in its
    // own folder, under the domain of the general settings.
    let showcase = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/showcase");
    let project = folder.join("project");
    std::fs::create_dir_all(&project).unwrap();
    for file in ["docker-compose.yml", ".env"] {
        std::fs::copy(showcase.join(file), project.join(file)).unwrap();
    }
    assert!(ask(
        &window,
        "add_project",
        json!({ "path": folder.join("nowhere") })
    )
    .is_err());
    assert_eq!(
        ask(&window, "add_project", json!({ "path": project })),
        Ok(json!("showcase"))
    );
    assert!(project.join("devshare.toml").is_file());

    let names = |declared: &Value| -> Vec<String> {
        let environments = declared["environments"].as_array().unwrap();
        environments
            .iter()
            .map(|environment| environment["name"].as_str().unwrap().to_string())
            .collect()
    };
    let declared = ask(&window, "declared", json!({})).unwrap();
    assert_eq!(names(&declared), ["admin", "shop", "showcase"]);
    assert_eq!(
        declared["environments"][2]["services"],
        json!([
            "showcase.lan:8710",
            "showcase.lan:8711",
            "showcase.lan:8712"
        ])
    );

    // Nothing was written in the general settings, nor in the owner's project.
    let untouched = std::fs::read_to_string(&general).unwrap();
    assert!(
        !untouched.contains("project") && untouched.starts_with("server = "),
        "{untouched}"
    );
    assert!(std::fs::read_to_string(site.join("devshare.toml"))
        .unwrap()
        .starts_with("[environments.shop]"));

    // A project whose folder goes away is said; removing it clears the list
    // and touches no file.
    std::fs::remove_file(project.join("devshare.toml")).unwrap();
    let declared = ask(&window, "declared", json!({})).unwrap();
    assert_eq!(names(&declared), ["admin", "shop"]);
    let broken = declared["problems"][0]["folder"].clone();
    assert_eq!(
        ask(&window, "remove_project", json!({ "path": broken })),
        Ok(Value::Null)
    );
    let declared = ask(&window, "declared", json!({})).unwrap();
    assert_eq!(declared["problems"], json!([]));
    assert!(project.join("docker-compose.yml").is_file());

    std::fs::remove_dir_all(&folder).ok();
}

#[test]
fn the_window_cannot_open_addresses_outside_a_joined_session_and_a_bad_invitation_is_said() {
    let app = commands::create(mock_builder());
    let window = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();

    let refused = ask(&window, "open", json!({ "url": "https://glitchr.dev" })).unwrap_err();
    assert!(refused
        .as_str()
        .unwrap()
        .contains("not an address of the session"));
    // Nothing handed over by a link yet.
    assert_eq!(ask(&window, "handed", json!({})).unwrap(), Value::Null);
    // Leaving when in no session is no error.
    assert!(ask(&window, "leave", json!({})).is_ok());

    // Not an invitation: refused, whatever this computer's rights.
    let error = ask(
        &window,
        "join",
        json!({ "invitation": "not an invitation" }),
    )
    .unwrap_err();
    assert!(!error.as_str().unwrap().is_empty());
}

#[test]
fn a_link_hands_its_invitation_to_the_window_which_joins_nothing_by_itself() {
    let app = commands::create(mock_builder());
    let window = WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let (heard, events) = channel();
    window.listen("invitation", move |event| {
        heard.send(event.payload().to_string()).ok();
    });

    let page = "https://join.glitchr.dev/#gVOxtm5P6ALN7P-x_yQk3";
    let link = format!("devshare://open?link={}", page.replace('#', "%23"));
    commands::hand_over(
        app.handle(),
        vec![
            "https://elsewhere.example/".parse().unwrap(),
            link.parse().unwrap(),
        ],
    );

    let said = events.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(serde_json::from_str::<String>(&said).unwrap(), page);
    // Kept for a window that was not there yet, once.
    assert_eq!(ask(&window, "handed", json!({})).unwrap(), json!(page));
    assert_eq!(ask(&window, "handed", json!({})).unwrap(), Value::Null);
    // Shown, not joined: nothing is open to leave, no address to open.
    let refused = ask(&window, "open", json!({ "url": "https://shop.test" })).unwrap_err();
    assert!(refused
        .as_str()
        .unwrap()
        .contains("not an address of the session"));
}
