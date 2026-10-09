//! The showcase of `tests/showcase`, read from its compose file, and the
//! rules for writing a directory's `devshare.toml`.

use std::path::{Path, PathBuf};

use devshare_core::{
    discover::{discover, write, Options, FILE},
    environment::Config,
};

fn showcase() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/showcase")
}

/// A copy of the showcase's compose file and ports in a folder of its own.
fn project(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("devshare-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&folder).ok();
    std::fs::create_dir_all(&folder).unwrap();
    for file in ["docker-compose.yml", ".env"] {
        std::fs::copy(showcase().join(file), folder.join(file)).unwrap();
    }
    folder
}

#[test]
fn the_showcase_shares_its_three_web_ports_and_nothing_else() {
    let found = discover(&showcase(), &Options::default()).unwrap();

    assert_eq!(found.project, "showcase");
    assert_eq!(found.hostname, "showcase.test");
    assert_eq!(found.sources, ["docker-compose.yml", ".env"]);

    // The ports of .env, not the defaults written in the compose file.
    let shared: Vec<(String, Option<u16>)> = found
        .shared()
        .map(|port| (port.label(), port.host))
        .collect();
    assert_eq!(
        shared,
        [
            ("web → 80".to_string(), Some(8710)),
            ("api → 80".to_string(), Some(8711)),
            ("docs → 80".to_string(), Some(8712)),
        ]
    );
    let left_out: Vec<(String, &str)> = found
        .left_out()
        .map(|port| (port.label(), port.left_out.as_deref().unwrap()))
        .collect();
    assert_eq!(
        left_out,
        [
            ("api → 8125/udp".to_string(), "UDP is not shared yet"),
            (
                "cache → 6379".to_string(),
                "a cache is not shared by default"
            ),
        ]
    );
    assert_eq!(
        found.entrypoint().as_deref(),
        Some("http://showcase.test:8710")
    );

    // The file in the showcase's folder is exactly what discover writes...
    let written = std::fs::read_to_string(showcase().join(FILE)).unwrap();
    assert_eq!(found.to_toml(None), written);

    // ...and what a session reads from it is what was found: one hostname,
    // three ports, each reached on this machine where Docker publishes it.
    let read: Config = toml::from_str(&written).unwrap();
    let services: Vec<(String, u16, Option<String>)> = read.environments["showcase"]
        .services
        .iter()
        .map(|service| (service.host.clone(), service.port, service.target.clone()))
        .collect();
    assert_eq!(
        services,
        [
            (
                "showcase.test".to_string(),
                8710,
                Some("127.0.0.1:8710".to_string())
            ),
            (
                "showcase.test".to_string(),
                8711,
                Some("127.0.0.1:8711".to_string())
            ),
            (
                "showcase.test".to_string(),
                8712,
                Some("127.0.0.1:8712".to_string())
            ),
        ]
    );
    let selection = read.select(&[]).unwrap();
    assert_eq!(selection.routes.len(), 3);
    assert!(!selection
        .routes
        .contains_key(&("showcase.test".to_string(), 8713)));
}

#[test]
fn the_shell_wins_over_the_env_file_and_a_hostname_can_be_chosen() {
    let options = Options {
        hostname: Some("Demo.Example.test".into()),
        domain: None,
        environment: vec![
            ("SHOWCASE_WEB".into(), "9001".into()),
            // Pinned to 0: Docker would pick the port.
            ("SHOWCASE_API".into(), "0".into()),
        ],
        hosts: None,
    };
    let found = discover(&showcase(), &options).unwrap();

    assert_eq!(found.hostname, "demo.example.test");
    let shared: Vec<Option<u16>> = found.shared().map(|port| port.host).collect();
    assert_eq!(shared, [Some(9001), Some(8712)]);
    let api = found
        .left_out()
        .find(|port| port.label() == "api → 80")
        .unwrap();
    assert!(api
        .left_out
        .as_ref()
        .unwrap()
        .contains("Docker picks its port"));
    assert_eq!(
        found.entrypoint().as_deref(),
        Some("http://demo.example.test:9001")
    );
}

#[test]
fn an_override_file_adds_its_ports_and_profiles_decide_what_starts() {
    let folder = project("override");
    std::fs::write(
        folder.join("docker-compose.override.yml"),
        "services:\n  web:\n    ports:\n      - \"8443:443\"\n  admin:\n    image: nginx:alpine\n    profiles: [admin]\n    ports:\n      - \"8720:80\"\n  shell:\n    image: linuxserver/openssh-server\n    ports:\n      - \"2222:22\"\n",
    )
    .unwrap();

    let found = discover(&folder, &Options::default()).unwrap();
    assert_eq!(
        found.sources,
        ["docker-compose.yml", "docker-compose.override.yml", ".env"]
    );
    let shared: Vec<Option<u16>> = found.shared().map(|port| port.host).collect();
    assert_eq!(shared, [Some(8710), Some(8443), Some(8711), Some(8712)]);
    // A port 443 makes the entry point HTTPS.
    assert_eq!(
        found.entrypoint().as_deref(),
        Some("https://showcase.test:8443")
    );
    let reason = |label: &str| {
        let port = found.left_out().find(|port| port.label() == label).unwrap();
        port.left_out.clone().unwrap()
    };
    assert_eq!(reason("admin → 80"), "only started with the profile admin");
    assert_eq!(
        reason("shell → 22"),
        "a remote shell is not shared by default"
    );

    // Asked for, the profile's service is shared like the others.
    let options = Options {
        environment: vec![("COMPOSE_PROFILES".into(), "admin".into())],
        ..Options::default()
    };
    let found = discover(&folder, &options).unwrap();
    assert!(found.shared().any(|port| port.host == Some(8720)));

    std::fs::remove_dir_all(&folder).ok();
}

#[test]
fn a_file_written_by_hand_is_kept_and_a_generated_one_is_refreshed() {
    let folder = project("write");
    let found = discover(&folder, &Options::default()).unwrap();
    let path = folder.join(FILE);

    // Nothing there: written.
    assert_eq!(write(&folder, &found, false).unwrap(), path);
    let first = std::fs::read_to_string(&path).unwrap();
    assert!(first.starts_with("# Written by `devshare discover`"));

    // The developer names a control plane in it; the compose file changes;
    // discover writes the new ports and keeps the control plane.
    std::fs::write(
        &path,
        first.replacen("\n\n", "\n\nserver = \"https://join.example\"\n\n", 1),
    )
    .unwrap();
    std::fs::write(folder.join(".env"), "SHOWCASE_WEB=8730\n").unwrap();
    let changed = discover(&folder, &Options::default()).unwrap();
    write(&folder, &changed, false).unwrap();
    let second: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(second.server.as_deref(), Some("https://join.example"));
    assert_eq!(second.environments["showcase"].services[0].port, 8730);

    // Without the first line it is the developer's file: left alone.
    std::fs::write(
        &path,
        "[environments.mine]\nservices = [{ host = \"mine.test\", port = 1 }]\n",
    )
    .unwrap();
    let refused = write(&folder, &changed, false).unwrap_err().to_string();
    assert!(refused.contains("written by hand"), "{refused}");
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("mine.test"));
    write(&folder, &changed, true).unwrap();
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("showcase.test"));

    std::fs::remove_dir_all(&folder).ok();
}

#[test]
fn a_folder_without_anything_to_share_says_so() {
    let folder = std::env::temp_dir().join(format!("devshare-empty-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    assert!(discover(&folder, &Options::default())
        .unwrap_err()
        .to_string()
        .contains("no compose file"));

    // Only a database: read, but nothing to write.
    std::fs::write(
        folder.join("compose.yaml"),
        "services:\n  database:\n    image: postgres:16\n    ports: [\"5432:5432\"]\n",
    )
    .unwrap();
    let found = discover(&folder, &Options::default()).unwrap();
    assert_eq!(found.shared().count(), 0);
    assert!(write(&folder, &found, false)
        .unwrap_err()
        .to_string()
        .contains("nothing to share"));
    assert!(!folder.join(FILE).exists());

    std::fs::remove_dir_all(&folder).ok();
}

#[test]
fn the_general_settings_choose_what_follows_the_projects_name() {
    let options = Options {
        domain: Some("lan".into()),
        ..Options::default()
    };
    let found = discover(&showcase(), &options).unwrap();
    assert_eq!(found.hostname, "showcase.lan");
    assert_eq!(
        found.entrypoint().as_deref(),
        Some("http://showcase.lan:8710")
    );
}

/// A project made of these files, in a folder of its own.
fn files(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("devshare-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&folder).ok();
    for (path, content) in files {
        let path = folder.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    folder
}

/// `host:port` of each service of the configuration a discovery gives.
fn shared(found: &devshare_core::discover::Discovery) -> Vec<String> {
    let config = found.config();
    let mut shared: Vec<String> = config
        .environments
        .values()
        .flat_map(|environment| &environment.services)
        .map(|service| format!("{}:{}", service.host, service.port))
        .collect();
    shared.sort();
    shared
}

#[test]
fn names_routed_by_traefik_labels_share_its_ports() {
    let folder = files(
        "traefik",
        &[(
            "compose.yaml",
            r#"
services:
  proxy:
    image: traefik:v3.1
    ports: ["80:80", "443:443"]
  app:
    image: my/app
    labels:
      traefik.http.routers.app.rule: "Host(`shop.test`) || Host(`api.shop.test`)"
      traefik.http.routers.app.entrypoints: websecure
  admin:
    image: my/admin
    labels:
      - "traefik.http.routers.admin.rule=Host(`admin.shop.test`) && PathPrefix(`/`)"
  hidden:
    image: my/hidden
    labels:
      traefik.enable: "false"
      traefik.http.routers.hidden.rule: "Host(`hidden.shop.test`)"
  vite:
    image: node:22
    ports: ["5173:5173"]
"#,
        )],
    );
    let found = discover(&folder, &Options::default()).unwrap();
    assert_eq!(found.hostname, "shop.test", "the first routed name");
    assert_eq!(
        shared(&found),
        [
            "admin.shop.test:443",
            "admin.shop.test:80",
            "api.shop.test:443",
            "api.shop.test:80",
            "shop.test:443",
            "shop.test:5173",
            "shop.test:80",
        ]
    );
    assert_eq!(found.entrypoint().as_deref(), Some("https://shop.test"));
    assert_eq!(found.routes[0].source, "the labels of app");

    // Written, then read back as a session reads it.
    let path = write(&folder, &found, false).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("#   api.shop.test  from the labels of app"),
        "{text}"
    );
    let config = Config::of(&folder).unwrap();
    assert_eq!(
        config.environments.values().next().unwrap().services.len(),
        7
    );
    std::fs::remove_dir_all(&folder).ok();
}

#[test]
fn names_of_a_mounted_nginx_configuration_and_what_guests_would_refuse() {
    let folder = files(
        "nginx",
        &[
            (
                "docker-compose.yml",
                "services:\n  web:\n    image: nginx:1.27\n    ports: [\"8443:443\"]\n    volumes:\n      - ./docker/nginx:/etc/nginx/conf.d:ro\n      - static:/srv\n  php:\n    image: php:8.3-fpm\nvolumes:\n  static:\n",
            ),
            (
                "docker/nginx/default.conf",
                "server {\n  listen 443 ssl;\n  server_name shop.test admin.shop.test shop.com localhost;\n}\n",
            ),
            ("docker/nginx/README.md", "server_name ignored.test;"),
        ],
    );
    let found = discover(&folder, &Options::default()).unwrap();
    assert_eq!(shared(&found), ["admin.shop.test:8443", "shop.test:8443"]);
    assert_eq!(found.routes[0].source, "docker/nginx/default.conf");
    assert!(
        found
            .notes
            .iter()
            .any(|note| note.contains("would refuse them: shop.com.")),
        "{:?}",
        found.notes
    );
    assert_eq!(
        found.entrypoint().as_deref(),
        Some("https://shop.test:8443")
    );
    std::fs::remove_dir_all(&folder).ok();
}

#[test]
fn a_caddy_built_from_the_project_and_a_traefik_of_the_machine() {
    let folder = files(
        "caddy",
        &[
            (
                "compose.yaml",
                "services:\n  caddy:\n    build: ./docker/caddy\n    ports: [\"8080:80\"]\n",
            ),
            (
                "docker/caddy/Caddyfile",
                "{\n  auto_https off\n}\nhttp://shop.test, http://www.shop.test {\n  reverse_proxy app:8000\n}\n",
            ),
        ],
    );
    let found = discover(&folder, &Options::default()).unwrap();
    assert_eq!(shared(&found), ["shop.test:8080", "www.shop.test:8080"]);
    std::fs::remove_dir_all(&folder).ok();

    // Labels for a Traefik the project does not run: the machine's own,
    // on the usual ports.
    let folder = files(
        "outside",
        &[(
            "compose.yaml",
            "services:\n  app:\n    image: my/app\n    labels: [\"traefik.http.routers.app.rule=Host(`shop.test`)\"]\n",
        )],
    );
    let found = discover(&folder, &Options::default()).unwrap();
    assert_eq!(shared(&found), ["shop.test:443", "shop.test:80"]);
    assert!(found
        .notes
        .iter()
        .any(|note| note.contains("not part of this project")));
    std::fs::remove_dir_all(&folder).ok();
}

#[test]
fn subdomains_in_the_hosts_file_follow_their_parent_and_a_chosen_name_is_kept() {
    let folder = files(
        "hosts",
        &[
            (
                "compose.yaml",
                "services:\n  web:\n    image: nginx\n    ports: [\"80:80\"]\n    volumes: [\"./site.conf:/etc/nginx/conf.d/site.conf\"]\n  api:\n    image: my/api\n    ports: [\"8080:8080\"]\n",
            ),
            ("site.conf", "server { server_name shop.test; }"),
            (
                "hosts",
                "127.0.0.1 localhost cdn.shop.test other.test\n10.0.0.2 lan.shop.test\n",
            ),
        ],
    );
    let options = Options {
        hosts: Some(folder.join("hosts")),
        ..Options::default()
    };
    let found = discover(&folder, &options).unwrap();
    assert_eq!(
        shared(&found),
        ["cdn.shop.test:80", "shop.test:80", "shop.test:8080"]
    );
    assert!(found
        .routes
        .iter()
        .any(|route| route.source == "/etc/hosts"));

    // A hostname chosen by the developer is the project's own: the proxy
    // still answers its names on its port.
    let options = Options {
        hostname: Some("mine.test".into()),
        ..Options::default()
    };
    let found = discover(&folder, &options).unwrap();
    assert_eq!(shared(&found), ["mine.test:8080", "shop.test:80"]);
    std::fs::remove_dir_all(&folder).ok();
}

#[test]
fn a_vite_project_without_docker_and_what_vite_needs_to_be_told() {
    let folder = files(
        "vite",
        &[
            (
                "vite.config.ts",
                "export default defineConfig({ server: { port: 3000, strictPort: true } })\n",
            ),
            ("package.json", r#"{"scripts": {"dev": "vite"}}"#),
        ],
    );
    let found = discover(&folder, &Options::default()).unwrap();
    let name = found.hostname.clone();
    assert_eq!(shared(&found), [format!("{name}:3000")]);
    assert_eq!(found.sources, ["vite.config.ts"]);
    let config = found.config();
    let service = &config.environments.values().next().unwrap().services[0];
    assert_eq!(service.target.as_deref(), Some("localhost:3000"));
    assert!(
        found.notes.iter().any(|note| note.contains("allowedHosts")),
        "{:?}",
        found.notes
    );
    assert!(!found.notes.iter().any(|note| note.contains("strictPort")));

    // Told about the name: nothing to say.
    std::fs::write(
        folder.join("vite.config.ts"),
        format!("export default {{ server: {{ strictPort: true, allowedHosts: ['{name}'] }} }}\n"),
    )
    .unwrap();
    let found = discover(&folder, &Options::default()).unwrap();
    assert_eq!(shared(&found), [format!("{name}:5173")]);
    assert!(found.notes.is_empty(), "{:?}", found.notes);
    std::fs::remove_dir_all(&folder).ok();
}

#[test]
fn a_symfony_project_served_by_the_symfony_cli() {
    let folder = files(
        "symfony",
        &[
            ("symfony.lock", "{}"),
            (".symfony.local.yaml", "http:\n  port: 8010\n"),
        ],
    );
    let found = discover(&folder, &Options::default()).unwrap();
    assert_eq!(shared(&found), [format!("{}:8010", found.hostname)]);
    assert_eq!(found.entrypoint(), None, "8010 is no usual web port");
    std::fs::remove_dir_all(&folder).ok();
}

#[test]
fn vite_on_the_machine_next_to_containers_and_not_twice() {
    let folder = files(
        "beside",
        &[
            (
                "compose.yaml",
                "services:\n  web:\n    image: nginx\n    ports: [\"8080:80\"]\n",
            ),
            ("vite.config.js", "export default {}\n"),
        ],
    );
    let found = discover(&folder, &Options::default()).unwrap();
    let name = found.hostname.clone();
    assert_eq!(
        shared(&found),
        [format!("{name}:5173"), format!("{name}:8080")]
    );

    // Vite in a container: its port is the container's, not added again.
    std::fs::write(
        folder.join("compose.yaml"),
        "services:\n  node:\n    image: node:22\n    ports: [\"5174:5173\"]\n",
    )
    .unwrap();
    let found = discover(&folder, &Options::default()).unwrap();
    assert_eq!(shared(&found), [format!("{name}:5174")]);
    std::fs::remove_dir_all(&folder).ok();
}

#[test]
fn the_project_names_itself_in_its_environment() {
    let folder = files(
        "named",
        &[
            (
                "docker-compose.yml",
                "services:\n  app:\n    image: my/app\n    ports: [\"${APP_HTTPS:-443}:443\", \"${APP_HTTP:-80}:80\"]\n",
            ),
            (
                ".env",
                "APP_HTTP=8124\nAPP_HTTPS=8633\nSSL_CERT_DOMAINS=localhost,shop.local,127.0.0.1\nDEFAULT_URI=https://localhost:8633\n",
            ),
            // The developer's own file wins, as it does for the project.
            (".env.local", "SSL_CERT_DOMAINS=localhost,shop.local,admin.shop.local,shop.com\n"),
        ],
    );
    let found = discover(&folder, &Options::default()).unwrap();
    assert_eq!(found.hostname, "shop.local");
    assert_eq!(
        shared(&found),
        [
            "admin.shop.local:8124",
            "admin.shop.local:8633",
            "shop.local:8124",
            "shop.local:8633",
        ]
    );
    assert_eq!(
        found.entrypoint().as_deref(),
        Some("https://shop.local:8633")
    );
    assert_eq!(
        found.routes[0].source,
        "SSL_CERT_DOMAINS in the environment"
    );
    assert!(
        found.notes.iter().any(|note| note.contains("shop.com")),
        "{:?}",
        found.notes
    );

    // An address names its host too; a name chosen by hand still wins.
    std::fs::write(
        folder.join(".env.local"),
        "SSL_CERT_DOMAINS=localhost\nDEFAULT_URI=https://shop.local:8633/\n",
    )
    .unwrap();
    assert_eq!(
        discover(&folder, &Options::default()).unwrap().hostname,
        "shop.local"
    );
    let chosen = Options {
        hostname: Some("mine.test".into()),
        ..Options::default()
    };
    assert_eq!(discover(&folder, &chosen).unwrap().hostname, "mine.test");
    std::fs::remove_dir_all(&folder).ok();
}

#[test]
fn projects_are_found_where_the_known_ones_are_and_in_the_usual_folders() {
    let home = files(
        "home",
        &[
            ("Sites/shop/compose.yaml", "services: {}\n"),
            ("Sites/blog/vite.config.ts", "export default {}\n"),
            ("Sites/notes/README.md", "not a project\n"),
            ("Sites/.hidden/compose.yaml", "services: {}\n"),
            ("work/api/symfony.lock", "{}"),
            ("work/known/compose.yaml", "services: {}\n"),
            ("elsewhere/lost/compose.yaml", "services: {}\n"),
        ],
    );
    // work/ because a known project sits there, Sites/ as a usual folder.
    let mut roots = devshare_core::discover::usual_folders(&home);
    roots.push(home.join("work"));
    let found = devshare_core::discover::candidates(&roots, &[home.join("work/known")]);
    let names: Vec<String> = found
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    // Known ones, hidden ones and folders that are no project are left out.
    assert_eq!(names, ["api", "blog", "shop"]);
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn a_project_is_started_its_own_way() {
    use devshare_core::discover::Commands;
    let folder = files(
        "commands",
        &[
            ("compose.yaml", "services: {}\n"),
            ("Makefile", "APP := x\nup: env\n\t@docker compose up -d\n\ndown:\n\t@docker compose down\nupdate:\n"),
        ],
    );
    let found = Commands::of(&folder);
    assert_eq!(found.up.as_deref(), Some("make up"));
    assert_eq!(found.down.as_deref(), Some("make down"));

    // Without the targets: Compose itself.
    std::fs::write(folder.join("Makefile"), "upgrade:\n\ttrue\nUP := 1\n").unwrap();
    assert_eq!(
        Commands::of(&folder).up.as_deref(),
        Some("docker compose up -d")
    );
    // Said in devshare.toml: that, whatever else there is.
    std::fs::write(
        folder.join(FILE),
        "up = \"make up ENV=prod\"\n[environments]\n",
    )
    .unwrap();
    assert_eq!(
        Commands::of(&folder).up.as_deref(),
        Some("make up ENV=prod")
    );
    assert_eq!(
        Commands::of(&folder).down.as_deref(),
        Some("docker compose down")
    );
    // Nothing to run a project with.
    let bare = files("bare", &[("vite.config.ts", "export default {}\n")]);
    assert_eq!(Commands::of(&bare), Commands::default());
    std::fs::remove_dir_all(&folder).ok();
    std::fs::remove_dir_all(&bare).ok();
}

#[test]
fn a_react_native_app_is_shared_through_its_metro_server_with_a_way_to_open_it() {
    let demo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/react-native");
    let found = discover(&demo, &Options::default()).unwrap();
    assert_eq!(found.hostname, "react-native.test");
    assert_eq!(shared(&found), ["react-native.test:8081"]);
    assert_eq!(found.sources, ["package.json"]);
    let config = found.config();
    let environment = config.environments.values().next().unwrap();
    assert_eq!(environment.services[0].kind.as_deref(), Some("metro"));
    assert_eq!(
        environment.services[0].target.as_deref(),
        Some("localhost:8081")
    );
    assert_eq!(
        (
            environment.launch[0].kind.as_str(),
            environment.launch[0].url.as_str()
        ),
        ("expo", "exp://react-native.test:8081")
    );
    assert!(
        found.notes.iter().any(|note| note.contains("tunnel")),
        "{:?}",
        found.notes
    );

    // Written and read back: the kind and the launch travel to the session.
    let toml = found.to_toml(None);
    assert!(toml.contains("kind = \"metro\""), "{toml}");
    assert!(
        toml.contains(
            "launch = [\n  { kind = \"expo\", url = \"exp://react-native.test:8081\" },\n]"
        ),
        "{toml}"
    );
    let folder = files("metro-written", &[("devshare.toml", &toml)]);
    let read = Config::of(&folder).unwrap();
    let selection = read.select(&[]).unwrap();
    let sent = selection.environments.values().next().unwrap();
    assert_eq!(sent.services[0].kind.as_deref(), Some("metro"));
    assert_eq!(sent.launches[0].url, "exp://react-native.test:8081");
    std::fs::remove_dir_all(&folder).ok();

    // Beside containers too: a compose project with a React Native app.
    let both = files(
        "metro-beside",
        &[
            (
                "compose.yaml",
                "services:\n  api:\n    image: my/api\n    ports: [\"8080:8080\"]\n",
            ),
            (
                "package.json",
                r#"{"dependencies": {"react-native": "0.76.0"}}"#,
            ),
        ],
    );
    let found = discover(&both, &Options::default()).unwrap();
    assert_eq!(
        shared(&found),
        [
            format!("{}:8080", found.hostname),
            format!("{}:8081", found.hostname)
        ]
    );
    assert!(
        found.launches.is_empty(),
        "no Expo: nothing to open it with"
    );
    std::fs::remove_dir_all(&both).ok();
}
