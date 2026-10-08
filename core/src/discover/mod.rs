//! Reads the Docker Compose file of a directory and works out what a
//! session could share: the TCP ports the project publishes on the host.
//!
//! Nothing is asked of Docker. The compose file says which ports a project
//! opens when it runs, and that is what a guest will be given under one
//! hostname, the same port numbers as on the host.
//!
//! What is understood of Compose: the standard file names and their
//! override, `COMPOSE_FILE`, `.env` (then `.env.local`), `${VAR}` with its
//! defaults, the short and long forms of `ports`, port ranges, profiles.
//! Not understood: `extends`, `include`.
//!
//! A project behind a reverse proxy answers several names on the same
//! ports: they are read from the proxy's configuration (see [`routes`]),
//! and subdomains of them from `/etc/hosts`. A project with no compose file
//! may still run servers on the machine: Vite's, the Symfony CLI's (see
//! [`local`]).

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use serde_yaml_ng::Value;

use crate::environment::{Config, EnvironmentDef, ServiceDef, DEFAULT_DOMAIN};

mod local;
mod routes;

/// First line of a file this module wrote, and may therefore write again.
const SIGNATURE: &str = "# Written by `devshare discover`";

/// The file a directory's configuration lives in.
pub const FILE: &str = crate::environment::PROJECT_FILE;

const COMPOSE_FILES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];
const ENV_FILES: [&str; 2] = [".env", ".env.local"];
/// A range of ports wider than this is reported, not listed port by port.
const WIDEST_RANGE: u16 = 32;

/// What is never shared unless the developer says so, by the port a
/// container listens on.
const PRIVATE_PORTS: &[(u16, &str)] = &[
    (22, "a remote shell"),
    (25, "a mail server"),
    (465, "a mail server"),
    (587, "a mail server"),
    (1025, "a mail server"),
    (1433, "a database"),
    (1521, "a database"),
    (2375, "the Docker API"),
    (2376, "the Docker API"),
    (3306, "a database"),
    (33060, "a database"),
    (5432, "a database"),
    (5672, "a message broker"),
    (6379, "a cache"),
    (9042, "a database"),
    (9092, "a message broker"),
    (11211, "a cache"),
    (27017, "a database"),
];

/// The same, by what the image is.
const PRIVATE_IMAGES: &[(&str, &str)] = &[
    ("mysql", "a database"),
    ("mariadb", "a database"),
    ("postgres", "a database"),
    ("mongo", "a database"),
    ("cassandra", "a database"),
    ("clickhouse", "a database"),
    ("elasticsearch", "a search index"),
    ("opensearch", "a search index"),
    ("redis", "a cache"),
    ("valkey", "a cache"),
    ("memcached", "a cache"),
    ("rabbitmq", "a message broker"),
    ("kafka", "a message broker"),
];

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The hostname guests will use. Defaults to `<project>.<domain>`.
    pub hostname: Option<String>,
    /// What follows the project's name in that default. `test` when none.
    pub domain: Option<String>,
    /// Variables of the calling shell: they win over the env files, as they
    /// do for Compose.
    pub environment: Vec<(String, String)>,
    /// A hosts file to read subdomains of the project's names from:
    /// `/etc/hosts` for a developer's machine, nothing in tests.
    pub hosts: Option<PathBuf>,
}

/// One port a service publishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub service: String,
    /// The port inside the container.
    pub container: u16,
    /// The port on the host, unless Docker picks it when the service starts.
    pub host: Option<u16>,
    /// The address it is published on, when it is not every address.
    pub address: Option<String>,
    pub udp: bool,
    /// Why it is not shared, when it is not.
    pub left_out: Option<String>,
}

impl Published {
    /// `web → 80`.
    pub fn label(&self) -> String {
        let protocol = if self.udp { "/udp" } else { "" };
        format!("{} → {}{protocol}", self.service, self.container)
    }

    /// Where the host agent reaches it.
    fn target(&self) -> Option<String> {
        let port = self.host?;
        Some(match self.address.as_deref() {
            Some(address) if address.contains(':') => format!("[{address}]:{port}"),
            Some(address) => format!("{address}:{port}"),
            None => format!("127.0.0.1:{port}"),
        })
    }
}

/// A hostname a reverse proxy of the project answers, and where it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub name: String,
    /// The service whose ports carry it. `None`: wherever the project's
    /// own hostname is shared.
    pub service: Option<String>,
    /// Where it was read: a file of the project, a service's labels.
    pub source: String,
}

/// What a directory's compose file publishes.
#[derive(Debug, Clone)]
pub struct Discovery {
    pub project: String,
    /// The project's own hostname: for the ports no proxy routes names on.
    pub hostname: String,
    /// The names the project's proxies route, on their own ports.
    pub routes: Vec<Route>,
    /// The files that were read, by name.
    pub sources: Vec<String>,
    pub ports: Vec<Published>,
    /// What could not be read from the file and the developer should know.
    pub notes: Vec<String>,
}

impl Discovery {
    pub fn shared(&self) -> impl Iterator<Item = &Published> {
        self.ports.iter().filter(|port| port.left_out.is_none())
    }

    pub fn left_out(&self) -> impl Iterator<Item = &Published> {
        self.ports.iter().filter(|port| port.left_out.is_some())
    }

    /// The hostnames a guest reaches `port` under: those its proxy routes,
    /// else the project's own and the names that go with it.
    pub fn names_for(&self, port: &Published) -> Vec<String> {
        let routed = |service: Option<&str>| -> Vec<String> {
            self.routes
                .iter()
                .filter(|route| route.service.as_deref() == service)
                .map(|route| route.name.clone())
                .collect()
        };
        let mut names = routed(Some(&port.service));
        if names.is_empty() {
            names.push(self.hostname.clone());
            names.extend(routed(None));
        }
        let mut seen = Vec::new();
        names.retain(|name| {
            let new = !seen.contains(name);
            seen.push(name.clone());
            new
        });
        names
    }

    /// The address guests start from: the web port, when there is one.
    pub fn entrypoint(&self) -> Option<String> {
        let by_container = |wanted: &[u16]| {
            wanted
                .iter()
                .find_map(|port| self.shared().find(|shared| shared.container == *port))
        };
        let (scheme, default, found) = match by_container(&[443, 8443]) {
            Some(found) => ("https", 443, found),
            None => (
                "http",
                80,
                by_container(&[80, 8080, 8000, 3000, 5173, 4200])?,
            ),
        };
        let name = self.names_for(found).swap_remove(0);
        Some(match found.host? {
            port if port == default => format!("{scheme}://{name}"),
            port => format!("{scheme}://{name}:{port}"),
        })
    }

    /// The configuration a session is started from.
    pub fn config(&self) -> Config {
        let services = self
            .shared()
            .filter(|port| port.host.is_some())
            .flat_map(|port| {
                self.names_for(port).into_iter().map(|host| ServiceDef {
                    host,
                    port: port.host.unwrap_or_default(),
                    target: port.target(),
                })
            })
            .collect();
        Config {
            server: None,
            environments: BTreeMap::from([(
                self.project.clone(),
                EnvironmentDef {
                    entrypoint: self.entrypoint(),
                    services,
                },
            )]),
        }
    }

    /// The configuration as the file a developer will read and edit: what
    /// was left out is there too, commented, one line away from shared.
    pub fn to_toml(&self, server: Option<&str>) -> String {
        let mut file = format!(
            "{SIGNATURE} from {}.\n\
             # Run it again when the compose file changes. Remove this line to keep the\n\
             # file as you edit it: discover will then leave it alone.\n\n",
            self.sources.join(", ")
        );
        if let Some(server) = server {
            file.push_str(&format!("server = {}\n\n", quoted(server)));
        }

        if !self.routes.is_empty() {
            file.push_str("# Names the project's proxies answer:\n");
            for route in &self.routes {
                file.push_str(&format!("#   {}  from {}\n", route.name, route.source));
            }
            file.push('\n');
        }
        file.push_str(&format!("[environments.{}]\n", key(&self.project)));
        if let Some(entrypoint) = self.entrypoint() {
            file.push_str(&format!("entrypoint = {}\n", quoted(&entrypoint)));
        }
        file.push_str("services = [\n");
        for port in &self.ports {
            // A UDP port gets no line to uncomment: a session carries TCP.
            let lines = port
                .host
                .zip(port.target())
                .filter(|_| !port.udp)
                .map(|(host, target)| {
                    self.names_for(port)
                        .iter()
                        .map(|name| {
                            format!(
                                "{{ host = {}, port = {host}, target = {} }},",
                                quoted(name),
                                quoted(&target)
                            )
                        })
                        .collect::<Vec<_>>()
                });
            match (&port.left_out, lines) {
                (None, Some(lines)) => {
                    file.push_str(&format!("  # {}\n", port.label()));
                    for line in lines {
                        file.push_str(&format!("  {line}\n"));
                    }
                }
                (Some(reason), Some(lines)) => {
                    file.push_str(&format!("  # {}: {reason}\n", port.label()));
                    for line in lines {
                        file.push_str(&format!("  # {line}\n"));
                    }
                }
                (reason, None) => {
                    let reason = reason.as_deref().unwrap_or("no port on the host");
                    file.push_str(&format!("  # {}: {reason}\n", port.label()));
                }
            }
        }
        file.push_str("]\n");
        for note in &self.notes {
            file.push_str(&format!("\n# {note}\n"));
        }
        file
    }
}

/// Reads the compose file of `directory`.
pub fn discover(directory: &Path, options: &Options) -> Result<Discovery> {
    let mut sources = Vec::new();

    // The shell's variables win over the files, a later file over an earlier one.
    let mut variables = HashMap::new();
    for name in ENV_FILES {
        if let Ok(content) = std::fs::read_to_string(directory.join(name)) {
            variables.extend(env_file(&content));
            sources.push(name.to_string());
        }
    }
    variables.extend(options.environment.iter().cloned());

    let composed = variables
        .get("COMPOSE_FILE")
        .is_some_and(|files| !files.is_empty())
        || has_compose_file(directory);
    if !composed {
        return on_the_machine(directory, options, &variables, sources);
    }

    let files = compose_files(directory, &variables)?;
    let mut documents = Vec::new();
    for file in &files {
        let content = std::fs::read_to_string(directory.join(file))
            .with_context(|| format!("reading {}", directory.join(file).display()))?;
        let mut document: Value = serde_yaml_ng::from_str(&content)
            .with_context(|| format!("{} is not valid YAML", directory.join(file).display()))?;
        document.apply_merge().ok();
        documents.push(document);
    }
    sources.splice(0..0, files);

    let project = project_name(directory, &documents, &variables)?;
    variables
        .entry("COMPOSE_PROJECT_NAME".to_string())
        .or_insert_with(|| project.clone());
    let profiles: Vec<String> = variables
        .get("COMPOSE_PROFILES")
        .map(|profiles| profiles.split(',').map(|p| p.trim().to_string()).collect())
        .unwrap_or_default();

    // A later file adds its ports to those of an earlier one and replaces
    // the rest, as Compose merges them.
    let mut services: Vec<(String, Service)> = Vec::new();
    for document in &documents {
        let Some(declared) = document.get("services").and_then(Value::as_mapping) else {
            continue;
        };
        for (name, definition) in declared {
            let Some(name) = name.as_str() else { continue };
            let definition = interpolated(definition, &variables);
            let index = match services.iter().position(|(known, _)| known == name) {
                Some(index) => index,
                None => {
                    services.push((name.to_string(), Service::default()));
                    services.len() - 1
                }
            };
            services[index].1.merge(&definition);
        }
    }
    if services.is_empty() {
        bail!("{} declares no service", sources[0]);
    }

    let hostname = match &options.hostname {
        Some(hostname) => hostname.trim().to_ascii_lowercase(),
        None => format!(
            "{}.{}",
            label(&project),
            options.domain.as_deref().unwrap_or(DEFAULT_DOMAIN)
        ),
    };
    let mut discovery = Discovery {
        project,
        hostname,
        routes: Vec::new(),
        sources,
        ports: Vec::new(),
        notes: Vec::new(),
    };

    for (name, service) in &services {
        if service.host_network {
            discovery.notes.push(format!(
                "{name} uses the host's network: the ports it opens are not in the compose file."
            ));
        }
        // A service with profiles only starts when one of them is asked for.
        let asked = service.profiles.iter().any(|p| profiles.contains(p));
        let inactive =
            (!service.profiles.is_empty() && !asked).then(|| service.profiles.join(", "));
        for declared in &service.ports {
            for mut port in
                ports(name, declared).with_context(|| format!("the ports of the service {name}"))?
            {
                port.left_out = match (&inactive, &port) {
                    (_, Published { left_out: Some(reason), .. }) => Some(reason.clone()),
                    (Some(profile), _) => Some(format!("only started with the profile {profile}")),
                    (_, Published { udp: true, .. }) => Some("UDP is not shared yet".into()),
                    (_, Published { host: None, .. }) => Some(
                        "Docker picks its port on the host when the service starts: pin one to share it"
                            .into(),
                    ),
                    _ => private(service.image.as_deref(), port.container)
                        .map(|kind| format!("{kind} is not shared by default")),
                };
                // Published on IPv4 and IPv6 alike: one service for a guest.
                let twice = discovery.ports.iter().any(|known: &Published| {
                    known.host.is_some() && known.host == port.host && known.udp == port.udp
                });
                if !twice {
                    discovery.ports.push(port);
                }
            }
        }
    }
    find_routes(directory, &services, options, &mut discovery);
    named_by_env(&variables, options, &mut discovery);

    // Vite often runs on the machine next to the containers: its port is
    // then published by none of them.
    if let Some(vite) = local::vite(directory, &discovery.hostname) {
        let port = vite.port.container;
        let published = discovery
            .ports
            .iter()
            .any(|known| known.container == port || known.host == Some(port));
        if !published {
            discovery.sources.push(vite.source);
            discovery.ports.push(vite.port);
            discovery.notes.extend(vite.notes);
        }
    }
    Ok(discovery)
}

/// A project without a compose file: the servers it runs on the machine.
fn on_the_machine(
    directory: &Path,
    options: &Options,
    variables: &HashMap<String, String>,
    mut sources: Vec<String>,
) -> Result<Discovery> {
    let project = project_name(directory, &[], variables)?;
    let hostname = match &options.hostname {
        Some(hostname) => hostname.trim().to_ascii_lowercase(),
        None => format!(
            "{}.{}",
            label(&project),
            options.domain.as_deref().unwrap_or(DEFAULT_DOMAIN)
        ),
    };
    let found: Vec<local::Local> = [local::symfony(directory), local::vite(directory, &hostname)]
        .into_iter()
        .flatten()
        .collect();
    if found.is_empty() {
        bail!(
            "no compose file in {}, nor a Vite or Symfony project",
            directory.display()
        );
    }
    let mut discovery = Discovery {
        project,
        hostname,
        routes: Vec::new(),
        sources: Vec::new(),
        ports: Vec::new(),
        notes: Vec::new(),
    };
    for local in found {
        sources.insert(0, local.source);
        discovery.ports.push(local.port);
        discovery.notes.extend(local.notes);
    }
    discovery.sources = sources;
    named_by_env(variables, options, &mut discovery);
    Ok(discovery)
}

/// The variables a project names itself in: its certificate's names, the
/// address its framework builds links with.
const NAME_VARIABLES: [&str; 6] = [
    "SSL_CERT_DOMAINS",
    "SERVER_NAME",
    "VIRTUAL_HOST",
    "APP_DOMAIN",
    "DEFAULT_URI",
    "APP_URL",
];

/// The names the project gives itself in its environment (`.env`, then
/// `.env.local`, then the shell): the first becomes its hostname unless one
/// was chosen, the others go with it. `SSL_CERT_DOMAINS=localhost,shop.local`
/// makes the project `shop.local` for its guests.
fn named_by_env(variables: &HashMap<String, String>, options: &Options, discovery: &mut Discovery) {
    let policy = devshare_protocol::names::NamePolicy::with(options.domain.clone());
    let mut names: Vec<(String, &str)> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    for variable in NAME_VARIABLES {
        let Some(value) = variables.get(variable) else {
            continue;
        };
        for word in value
            .split([',', ' ', '\t'])
            .map(str::trim)
            .filter(|word| !word.is_empty())
        {
            // An address (`https://shop.local:8443/`) stands for its host.
            let name = match url::Url::parse(word) {
                Ok(address) if address.has_host() => {
                    address.host_str().unwrap_or_default().to_string()
                }
                _ => word.to_string(),
            };
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            // This machine keeps `localhost`; addresses are not names.
            if name == "localhost"
                || name.parse::<std::net::IpAddr>().is_ok()
                || !devshare_protocol::names::is_hostname(&name)
            {
                continue;
            }
            if !policy.accepts(&name) {
                if !refused.contains(&name) {
                    refused.push(name);
                }
                continue;
            }
            if !names.iter().any(|(known, _)| *known == name) {
                names.push((name, variable));
            }
        }
    }
    if !refused.is_empty() {
        discovery.notes.push(format!(
            "Named in the project's environment but left out, as guests would refuse them: {}.",
            refused.join(", ")
        ));
    }
    let Some((first, _)) = names.first() else {
        return;
    };
    if options.hostname.is_none() {
        discovery.hostname = first.clone();
    }
    for (name, variable) in names {
        if name != discovery.hostname && !discovery.routes.iter().any(|route| route.name == name) {
            discovery.routes.push(Route {
                name,
                service: None,
                source: format!("{variable} in the environment"),
            });
        }
    }
}

/// The names the project's proxies answer, on the ports of the proxy that
/// answers them. The first becomes the project's hostname unless one was
/// chosen.
fn find_routes(
    directory: &Path,
    services: &[(String, Service)],
    options: &Options,
    discovery: &mut Discovery,
) {
    use routes::Proxy;

    let proxy_of = |name: &str, service: &Service| {
        Proxy::of(name, service.image.as_deref(), service.build.as_deref())
    };
    // The service that carries what a proxy's labels route: the proxy
    // itself when the project runs it.
    let carrier = |wanted: Proxy| {
        services
            .iter()
            .find(|(name, service)| proxy_of(name, service) == Some(wanted))
            .map(|(name, _)| name.clone())
    };

    let mut found: Vec<(String, Option<String>, String)> = Vec::new();
    let mut outside = Vec::new();
    for (name, service) in services {
        let disabled = service
            .labels
            .iter()
            .any(|(key, value)| key == "traefik.enable" && value.eq_ignore_ascii_case("false"));
        for (key, value) in &service.labels {
            let Some((proxy, hosts)) = routes::label_hosts(key, value) else {
                continue;
            };
            if proxy == Proxy::Traefik && disabled {
                continue;
            }
            let by = carrier(proxy).unwrap_or_else(|| {
                let label = match proxy {
                    Proxy::Traefik => "traefik",
                    _ => "caddy",
                };
                if !outside.contains(&label) {
                    outside.push(label);
                }
                format!("{label} (outside this project)")
            });
            for host in hosts {
                found.push((host, Some(by.clone()), format!("the labels of {name}")));
            }
        }
        if let Some(proxy) = proxy_of(name, service) {
            for file in
                routes::config_files(directory, proxy, &service.mounted, service.build.as_deref())
            {
                let Ok(text) = std::fs::read_to_string(&file) else {
                    continue;
                };
                let source = file
                    .strip_prefix(directory)
                    .unwrap_or(&file)
                    .display()
                    .to_string();
                for host in proxy.names_in(&text) {
                    found.push((host, Some(name.clone()), source.clone()));
                }
            }
        }
    }

    // A proxy of the machine, not of the project: it listens on the usual
    // ports, which the compose file therefore does not publish.
    for label in outside {
        let service = format!("{label} (outside this project)");
        for port in [80, 443] {
            if discovery.ports.iter().any(|known| known.host == Some(port)) {
                continue;
            }
            discovery.ports.push(Published {
                service: service.clone(),
                container: port,
                host: Some(port),
                address: None,
                udp: false,
                left_out: None,
            });
        }
        discovery.notes.push(format!(
            "Names are routed by a {label} that is not part of this project: it is assumed \
             on this machine's ports 80 and 443."
        ));
    }

    // A guest refuses names outside the development domains, and this
    // machine keeps `localhost` for itself.
    let policy = devshare_protocol::names::NamePolicy::with(options.domain.clone());
    let mut refused = Vec::new();
    for (name, service, source) in found {
        if name == "localhost" {
            continue;
        }
        if !policy.accepts(&name) {
            if !refused.contains(&name) {
                refused.push(name);
            }
            continue;
        }
        let known = discovery
            .routes
            .iter()
            .any(|route| route.name == name && route.service == service);
        if !known {
            discovery.routes.push(Route {
                name,
                service,
                source,
            });
        }
    }
    if !refused.is_empty() {
        discovery.notes.push(format!(
            "Routed by the project but left out, as guests would refuse them: {}. Only names \
             under .test, .localhost and the other development domains are shared.",
            refused.join(", ")
        ));
    }
    if options.hostname.is_none() {
        if let Some(first) = discovery.routes.first() {
            discovery.hostname = first.name.clone();
        }
    }

    // Subdomains the developer gave this machine for the project's names.
    let Some(text) = options
        .hosts
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok())
    else {
        return;
    };
    for name in routes::loopback_names(&text) {
        if !policy.accepts(&name) || discovery.routes.iter().any(|route| route.name == name) {
            continue;
        }
        let parent = discovery
            .routes
            .iter()
            .find(|route| name.ends_with(&format!(".{}", route.name)))
            .map(|route| route.service.clone());
        let service = match parent {
            Some(service) => service,
            None if name.ends_with(&format!(".{}", discovery.hostname)) => None,
            None => continue,
        };
        discovery.routes.push(Route {
            name,
            service,
            source: "/etc/hosts".into(),
        });
    }
}

/// Writes the configuration of `directory`, replacing one this module wrote
/// before and nothing else unless `force` is set. The control plane named in
/// the file it replaces is kept.
pub fn write(directory: &Path, discovery: &Discovery, force: bool) -> Result<PathBuf> {
    if discovery.shared().next().is_none() {
        bail!(
            "nothing to share: the compose file publishes no TCP port on a fixed port of the host"
        );
    }
    let path = directory.join(FILE);
    let existing = std::fs::read_to_string(&path).ok();
    if let Some(existing) = &existing {
        if !existing.starts_with(SIGNATURE) && !force {
            bail!(
                "{} was written by hand: it is kept as it is (--force replaces it)",
                path.display()
            );
        }
    }
    let server = existing
        .and_then(|existing| toml::from_str::<Config>(&existing).ok())
        .and_then(|config| config.server);
    std::fs::write(&path, discovery.to_toml(server.as_deref()))
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// The configuration of a project's folder. A folder that has none yet gets
/// it from its compose file, written there: the discovery is returned then,
/// for whoever wants to say what was found.
///
/// A file that exists is read as it is, never recomputed: its owner may
/// have edited it, and only an explicit `discover` replaces it.
pub fn project(folder: &Path, options: &Options) -> Result<(Config, Option<Discovery>)> {
    if folder.join(FILE).is_file() {
        return Ok((Config::of(folder)?, None));
    }
    let found = discover(folder, options).with_context(|| {
        format!(
            "{} has no {FILE}, and none can be written for it",
            folder.display()
        )
    })?;
    write(folder, &found, false)?;
    Ok((Config::of(folder)?, Some(found)))
}

#[derive(Debug, Default)]
struct Service {
    image: Option<String>,
    ports: Vec<Value>,
    profiles: Vec<String>,
    host_network: bool,
    labels: Vec<(String, String)>,
    /// The files and folders of the machine mounted into it.
    mounted: Vec<String>,
    /// Its build context, when it is built from the project.
    build: Option<String>,
}

impl Service {
    fn merge(&mut self, definition: &Value) {
        if let Some(image) = definition.get("image").and_then(Value::as_str) {
            self.image = Some(image.to_string());
        }
        if let Some(ports) = definition.get("ports").and_then(Value::as_sequence) {
            for port in ports {
                if !self.ports.contains(port) {
                    self.ports.push(port.clone());
                }
            }
        }
        if let Some(profiles) = definition.get("profiles").and_then(Value::as_sequence) {
            self.profiles = profiles
                .iter()
                .filter_map(|profile| profile.as_str().map(str::to_string))
                .collect();
        }
        if let Some(mode) = definition.get("network_mode").and_then(Value::as_str) {
            self.host_network = mode == "host";
        }
        match definition.get("labels") {
            Some(Value::Mapping(labels)) => {
                for (key, value) in labels {
                    if let (Some(key), Some(value)) = (key.as_str(), scalar(value)) {
                        self.label(key, &value);
                    }
                }
            }
            Some(Value::Sequence(labels)) => {
                for label in labels.iter().filter_map(Value::as_str) {
                    let (key, value) = label.split_once('=').unwrap_or((label, ""));
                    self.label(key, value);
                }
            }
            _ => {}
        }
        if let Some(volumes) = definition.get("volumes").and_then(Value::as_sequence) {
            for volume in volumes {
                let source = match volume {
                    Value::String(short) => short.split(':').next().map(str::to_string),
                    long => long
                        .get("type")
                        .and_then(Value::as_str)
                        .filter(|kind| *kind == "bind")
                        .and(long.get("source").and_then(Value::as_str))
                        .map(str::to_string),
                };
                // A named volume is Docker's, not a path of the machine.
                if let Some(source) = source.filter(|source| source.starts_with(['.', '/'])) {
                    if !self.mounted.contains(&source) {
                        self.mounted.push(source);
                    }
                }
            }
        }
        match definition.get("build") {
            Some(Value::String(context)) => self.build = Some(context.clone()),
            Some(build) => {
                if let Some(context) = build.get("context").and_then(Value::as_str) {
                    self.build = Some(context.to_string());
                }
            }
            None => {}
        }
    }

    fn label(&mut self, key: &str, value: &str) {
        let key = key.trim().to_string();
        self.labels.retain(|(known, _)| *known != key);
        self.labels.push((key, value.trim().to_string()));
    }
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Whether a folder is a project discovery can read: a compose file, a Vite
/// configuration, or a Symfony application.
pub fn is_project(directory: &Path) -> bool {
    has_compose_file(directory)
        || local::vite(directory, "").is_some()
        || local::symfony(directory).is_some()
}

/// Where a developer usually keeps projects, under their home folder: the
/// ones that exist.
pub fn usual_folders(home: &Path) -> Vec<PathBuf> {
    const USUAL: [&str; 9] = [
        "Sites",
        "Projects",
        "projects",
        "Developer",
        "Code",
        "code",
        "src",
        "dev",
        "workspace",
    ];
    USUAL
        .iter()
        .map(|name| home.join(name))
        .filter(|folder| folder.is_dir())
        .collect()
}

/// The projects in `roots`, one level down, `known` ones left out, sorted
/// by name.
pub fn candidates(roots: &[PathBuf], known: &[PathBuf]) -> Vec<PathBuf> {
    /// Enough for any developer's folder, few enough to read in a moment.
    const MOST: usize = 300;

    let known: Vec<PathBuf> = known
        .iter()
        .map(|folder| folder.canonicalize().unwrap_or_else(|_| folder.clone()))
        .collect();
    let mut found = Vec::new();
    let mut seen = Vec::new();
    for root in roots {
        let Ok(root) = root.canonicalize() else {
            continue;
        };
        if seen.contains(&root) {
            continue;
        }
        seen.push(root.clone());
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let hidden = entry.file_name().to_string_lossy().starts_with('.');
            if hidden || !path.is_dir() || known.contains(&path) || found.contains(&path) {
                continue;
            }
            if is_project(&path) {
                found.push(path);
            }
            if found.len() >= MOST {
                break;
            }
        }
    }
    found.sort_by_key(|path| path.file_name().map(|name| name.to_ascii_lowercase()));
    found
}

/// How a project is started and stopped.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct Commands {
    pub up: Option<String>,
    pub down: Option<String>,
}

impl Commands {
    /// What `devshare.toml` says (`up = "…"`, `down = "…"`), else the
    /// project's Makefile when it has `up` and `down` targets (`make up`
    /// reads the project's `.env` files and picks its environment itself),
    /// else `docker compose up -d` and `docker compose down` when it has a
    /// compose file. Nothing for a project started some other way.
    pub fn of(folder: &Path) -> Self {
        let written: Commands = std::fs::read_to_string(folder.join(FILE))
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default();
        let makefile = ["Makefile", "makefile", "GNUmakefile"]
            .iter()
            .find_map(|name| std::fs::read_to_string(folder.join(name)).ok())
            .unwrap_or_default();
        let target = |name: &str| {
            makefile.lines().any(|line| {
                line.strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with(':') && !rest.starts_with(":="))
            })
        };
        let compose = has_compose_file(folder);
        let fallback = |make: &str, docker: &str| {
            if target(make) {
                Some(format!("make {make}"))
            } else if compose {
                Some(docker.to_string())
            } else {
                None
            }
        };
        Self {
            up: written
                .up
                .or_else(|| fallback("up", "docker compose up -d")),
            down: written
                .down
                .or_else(|| fallback("down", "docker compose down")),
        }
    }
}

/// Whether a folder holds a compose file under one of its standard names.
pub fn has_compose_file(directory: &Path) -> bool {
    COMPOSE_FILES
        .iter()
        .any(|name| directory.join(name).is_file())
}

/// The compose files of a directory, in the order Compose reads them.
fn compose_files(directory: &Path, variables: &HashMap<String, String>) -> Result<Vec<String>> {
    if let Some(listed) = variables
        .get("COMPOSE_FILE")
        .filter(|listed| !listed.is_empty())
    {
        let separator = variables
            .get("COMPOSE_PATH_SEPARATOR")
            .map(String::as_str)
            .unwrap_or(":");
        return Ok(listed.split(separator).map(str::to_string).collect());
    }
    let Some(main) = COMPOSE_FILES
        .iter()
        .find(|name| directory.join(name).is_file())
    else {
        bail!("no compose file in {}", directory.display());
    };
    let mut files = vec![main.to_string()];
    let (stem, extension) = main.rsplit_once('.').unwrap_or((main, "yml"));
    for extension in [extension, "yaml", "yml"] {
        let candidate = format!("{stem}.override.{extension}");
        if directory.join(&candidate).is_file() && !files.contains(&candidate) {
            files.push(candidate);
            break;
        }
    }
    Ok(files)
}

/// The project's name as Compose decides it: the file's `name`, else
/// `COMPOSE_PROJECT_NAME`, else the directory.
fn project_name(
    directory: &Path,
    documents: &[Value],
    variables: &HashMap<String, String>,
) -> Result<String> {
    let named = documents
        .iter()
        .rev()
        .find_map(|document| document.get("name").and_then(Value::as_str))
        .map(|name| interpolate(name, variables))
        .filter(|name| !name.is_empty());
    let name = match named.or_else(|| variables.get("COMPOSE_PROJECT_NAME").cloned()) {
        Some(name) => name,
        None => directory
            .canonicalize()
            .ok()
            .as_deref()
            .unwrap_or(directory)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| anyhow!("{} has no name", directory.display()))?,
    };
    let name: String = name
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    if name.is_empty() {
        bail!("the project has no usable name");
    }
    Ok(name)
}

/// A project name as a DNS label.
fn label(project: &str) -> String {
    let label = project.replace('_', "-");
    label.trim_matches('-').to_string()
}

fn private(image: Option<&str>, container: u16) -> Option<&'static str> {
    // `registry/team/mysql:8` is judged on `mysql`.
    let image = image.map(|image| {
        let name = image.rsplit('/').next().unwrap_or(image);
        name.split([':', '@'])
            .next()
            .unwrap_or(name)
            .to_ascii_lowercase()
    });
    let by_image = PRIVATE_IMAGES
        .iter()
        .find(|(name, _)| image.as_deref().is_some_and(|image| image.contains(name)));
    let by_port = PRIVATE_PORTS.iter().find(|(port, _)| *port == container);
    by_image
        .map(|(_, kind)| *kind)
        .or(by_port.map(|(_, kind)| *kind))
}

/// One entry of `ports`, in either form, as the ports it stands for.
fn ports(service: &str, declared: &Value) -> Result<Vec<Published>> {
    let (address, host, container, udp) = match declared {
        Value::Number(port) => (None, None, port.to_string(), false),
        Value::String(short) => {
            let (mapping, protocol) = short.split_once('/').unwrap_or((short, "tcp"));
            // `[address:][host:]container`, the address possibly an IPv6 one.
            let mut parts = mapping.rsplitn(3, ':');
            let container = parts.next().unwrap_or_default().to_string();
            let host = parts.next().map(str::to_string);
            let address = parts
                .next()
                .map(|address| address.trim_matches(['[', ']']).to_string());
            (
                address,
                host,
                container,
                protocol.eq_ignore_ascii_case("udp"),
            )
        }
        Value::Mapping(_) => {
            let text = |key: &str| match declared.get(key) {
                Some(Value::String(text)) => Some(text.clone()),
                Some(Value::Number(number)) => Some(number.to_string()),
                _ => None,
            };
            let container = text("target").ok_or_else(|| anyhow!("a port has no target"))?;
            let udp = text("protocol").is_some_and(|protocol| protocol.eq_ignore_ascii_case("udp"));
            (text("host_ip"), text("published"), container, udp)
        }
        other => bail!("{other:?} is not a port"),
    };

    let address = address.filter(|address| !matches!(address.as_str(), "" | "0.0.0.0" | "::"));
    let container = range(&container)?;
    // No host port, or 0: Docker picks one.
    let host = match host.as_deref() {
        None | Some("") | Some("0") => None,
        Some(host) => Some(range(host)?),
    };
    let count = container.1 - container.0 + 1;
    let paired = host.filter(|host| host.1 - host.0 + 1 == count);

    if count > WIDEST_RANGE {
        return Ok(vec![Published {
            service: service.to_string(),
            container: container.0,
            host: None,
            address,
            udp,
            left_out: Some(format!("a range of {count} ports: list the ones to share")),
        }]);
    }
    Ok((0..count)
        .map(|offset| Published {
            service: service.to_string(),
            container: container.0 + offset,
            host: paired.map(|host| host.0 + offset),
            address: address.clone(),
            udp,
            left_out: None,
        })
        .collect())
}

/// `8080` or `8000-8010`, as its two ends.
fn range(text: &str) -> Result<(u16, u16)> {
    let (first, last) = text.split_once('-').unwrap_or((text, text));
    let port = |text: &str| {
        text.trim()
            .parse::<u16>()
            .map_err(|_| anyhow!("\"{text}\" is not a port"))
    };
    let (first, last) = (port(first)?, port(last)?);
    if last < first {
        bail!("\"{text}\" is not a range of ports");
    }
    Ok((first, last))
}

/// The variables of an env file.
fn env_file(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line);
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (name, value) = line.split_once('=')?;
            let value = value.trim();
            let value = match value.chars().next() {
                Some(quote @ ('"' | '\'')) => value[1..].split(quote).next().unwrap_or_default(),
                // Unquoted: a comment may follow.
                _ => value.split(" #").next().unwrap_or_default().trim_end(),
            };
            Some((name.trim().to_string(), value.to_string()))
        })
        .collect()
}

/// A copy of `value` with the variables of every string replaced.
fn interpolated(value: &Value, variables: &HashMap<String, String>) -> Value {
    match value {
        Value::String(text) => Value::String(interpolate(text, variables)),
        Value::Sequence(items) => Value::Sequence(
            items
                .iter()
                .map(|item| interpolated(item, variables))
                .collect(),
        ),
        Value::Mapping(entries) => Value::Mapping(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), interpolated(value, variables)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Replaces `$VAR`, `${VAR}`, `${VAR:-default}`, `${VAR-default}`,
/// `${VAR:+other}`, `${VAR+other}` and `${VAR:?message}`; `$$` is a dollar.
/// A default may itself hold variables.
fn interpolate(text: &str, variables: &HashMap<String, String>) -> String {
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    let is_name = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';

    while index < bytes.len() {
        if bytes[index] != b'$' {
            let next = text[index..]
                .find('$')
                .map_or(bytes.len(), |found| index + found);
            output.push_str(&text[index..next]);
            index = next;
            continue;
        }
        match bytes.get(index + 1) {
            Some(b'$') => {
                output.push('$');
                index += 2;
            }
            Some(b'{') => {
                // The matching brace, whatever is nested in between.
                let start = index + 2;
                let mut depth = 1;
                let mut end = start;
                while end < bytes.len() && depth > 0 {
                    match bytes[end] {
                        b'{' if bytes[end - 1] == b'$' => depth += 1,
                        b'}' => depth -= 1,
                        _ => {}
                    }
                    end += 1;
                }
                if depth > 0 {
                    output.push_str(&text[index..]);
                    break;
                }
                let inside = &text[start..end - 1];
                let name_end = inside
                    .bytes()
                    .position(|byte| !is_name(byte))
                    .unwrap_or(inside.len());
                let (name, rest) = inside.split_at(name_end);
                let value = variables.get(name);
                let word = |skip: usize| interpolate(&rest[skip..], variables);
                let set = value.is_some();
                let filled = value.is_some_and(|value| !value.is_empty());
                let current = || value.cloned().unwrap_or_default();
                output.push_str(&match rest.as_bytes() {
                    [b':', b'-', ..] => {
                        if filled {
                            current()
                        } else {
                            word(2)
                        }
                    }
                    [b'-', ..] => {
                        if set {
                            current()
                        } else {
                            word(1)
                        }
                    }
                    [b':', b'+', ..] => {
                        if filled {
                            word(2)
                        } else {
                            String::new()
                        }
                    }
                    [b'+', ..] => {
                        if set {
                            word(1)
                        } else {
                            String::new()
                        }
                    }
                    _ => current(),
                });
                index = end;
            }
            Some(&byte) if is_name(byte) && !byte.is_ascii_digit() => {
                let start = index + 1;
                let end = start
                    + bytes[start..]
                        .iter()
                        .position(|byte| !is_name(*byte))
                        .unwrap_or(bytes.len() - start);
                output.push_str(variables.get(&text[start..end]).map_or("", String::as_str));
                index = end;
            }
            _ => {
                output.push('$');
                index += 1;
            }
        }
    }
    output
}

/// A string as TOML writes it.
fn quoted(text: &str) -> String {
    toml::Value::String(text.to_string()).to_string()
}

/// A table key as TOML writes it: bare when it can be.
fn key(name: &str) -> String {
    let bare = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    if bare {
        name.to_string()
    } else {
        quoted(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variables(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn variables_are_replaced_as_compose_replaces_them() {
        let known = variables(&[("HTTP", "8080"), ("EMPTY", ""), ("NAME", "shop")]);
        for (text, wanted) in [
            ("${HTTP}:80", "8080:80"),
            ("$HTTP:80", "8080:80"),
            ("${HTTPS:-0}:443", "0:443"),
            ("${EMPTY:-1}/${EMPTY-2}", "1/"),
            ("${MISSING-2}", "2"),
            ("${APP:-${NAME}}-proxy", "shop-proxy"),
            ("${APP:-${OTHER:-x}}", "x"),
            ("${NAME:+set}${MISSING:+set}", "set"),
            ("price: $$5 ${MISSING}", "price: $5 "),
            ("no variable", "no variable"),
            ("${unterminated", "${unterminated"),
        ] {
            assert_eq!(interpolate(text, &known), wanted, "{text}");
        }
    }

    #[test]
    fn env_files_are_read_with_their_quotes_and_comments() {
        let read = env_file(
            "# ports\nAPP_HTTP=8098\nexport APP_HTTPS=\"8493\"  # tls\nNAME='my shop'\nPLAIN=a b # c\n\nnot a line\n",
        );
        assert_eq!(
            read,
            [
                ("APP_HTTP".to_string(), "8098".to_string()),
                ("APP_HTTPS".to_string(), "8493".to_string()),
                ("NAME".to_string(), "my shop".to_string()),
                ("PLAIN".to_string(), "a b".to_string()),
            ]
        );
    }

    /// `(container, host, address, udp)` for each port an entry stands for.
    fn read(entry: &str) -> Vec<(u16, Option<u16>, Option<String>, bool)> {
        let declared: Value = serde_yaml_ng::from_str(entry).unwrap();
        ports("web", &declared)
            .unwrap()
            .into_iter()
            .map(|port| (port.container, port.host, port.address, port.udp))
            .collect()
    }

    #[test]
    fn every_form_of_a_port_is_read() {
        assert_eq!(read("\"8080:80\""), [(80, Some(8080), None, false)]);
        assert_eq!(read("\"0.0.0.0:8080:80\""), [(80, Some(8080), None, false)]);
        assert_eq!(
            read("\"127.0.0.1:8080:80\""),
            [(80, Some(8080), Some("127.0.0.1".into()), false)]
        );
        assert_eq!(
            read("\"[::1]:8080:80\""),
            [(80, Some(8080), Some("::1".into()), false)]
        );
        assert_eq!(read("\"8443:443/udp\""), [(443, Some(8443), None, true)]);
        // No port on the host, or 0: Docker picks one.
        assert_eq!(read("3000"), [(3000, None, None, false)]);
        assert_eq!(read("\"3000\""), [(3000, None, None, false)]);
        assert_eq!(read("\"0:80\""), [(80, None, None, false)]);
        assert_eq!(
            read("\"9000-9002:7000-7002\""),
            [
                (7000, Some(9000), None, false),
                (7001, Some(9001), None, false),
                (7002, Some(9002), None, false),
            ]
        );
        assert_eq!(
            read("{ target: 80, published: \"8080\", protocol: tcp, host_ip: 127.0.0.1 }"),
            [(80, Some(8080), Some("127.0.0.1".into()), false)]
        );
        assert_eq!(
            read("{ target: 53, published: 5353, protocol: udp }"),
            [(53, Some(5353), None, true)]
        );
        assert_eq!(read("{ target: 80 }"), [(80, None, None, false)]);

        let wide: Value = serde_yaml_ng::from_str("\"49160-49200:49160-49200\"").unwrap();
        let wide = ports("turn", &wide).unwrap();
        assert_eq!(wide.len(), 1);
        assert!(wide[0].left_out.as_ref().unwrap().contains("41 ports"));

        let nonsense: Value = serde_yaml_ng::from_str("\"http:80\"").unwrap();
        assert!(ports("web", &nonsense).is_err());
    }

    #[test]
    fn databases_caches_and_shells_stay_private() {
        assert_eq!(private(Some("mysql:8"), 3306), Some("a database"));
        assert_eq!(
            private(Some("registry.example/team/postgres@sha256:abc"), 9999),
            Some("a database")
        );
        assert_eq!(private(Some("redis:alpine"), 6379), Some("a cache"));
        // An image that says nothing, a port that says it all.
        assert_eq!(private(Some("my-company/thing"), 5432), Some("a database"));
        assert_eq!(private(None, 22), Some("a remote shell"));
        assert_eq!(private(Some("nginx:alpine"), 80), None);
        assert_eq!(private(Some("maildev/maildev"), 1080), None);
        assert_eq!(
            private(Some("maildev/maildev"), 1025),
            Some("a mail server")
        );
    }
}
