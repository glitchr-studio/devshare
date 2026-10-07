//! The hostnames a project's reverse proxy routes, read from its
//! configuration: Traefik's labels and dynamic files, Caddy's Caddyfile and
//! labels (caddy-docker-proxy), nginx's `server_name`. A project behind a
//! proxy answers several names on the same port, and a guest needs every
//! one of them.
//!
//! Only what is read is reported: names with placeholders, wildcards or
//! regular expressions are skipped, since no guest can resolve them.

use std::path::{Path, PathBuf};

/// Configuration files larger than this are not read.
const LARGEST_FILE: u64 = 1024 * 1024;
/// Files read from one mounted folder, at most.
const MOST_FILES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Proxy {
    Traefik,
    Caddy,
    Nginx,
}

impl Proxy {
    /// What a service is, by its image, its build context or its name.
    pub(super) fn of(service: &str, image: Option<&str>, build: Option<&str>) -> Option<Self> {
        let image = image.map(|image| {
            let name = image.rsplit('/').next().unwrap_or(image);
            name.split([':', '@'])
                .next()
                .unwrap_or(name)
                .to_ascii_lowercase()
        });
        let hints = [image.as_deref(), build, Some(service)];
        for (word, proxy) in [
            ("traefik", Proxy::Traefik),
            ("caddy", Proxy::Caddy),
            ("nginx", Proxy::Nginx),
            ("openresty", Proxy::Nginx),
        ] {
            let named = |hint: &Option<&str>| {
                hint.is_some_and(|hint| hint.to_ascii_lowercase().contains(word))
            };
            // The image decides when there is one: `php` built from
            // ./docker/nginx-php is still nginx, a `traefik` service running
            // nginx is not Traefik.
            if image.is_some() {
                if named(&hints[0]) {
                    return Some(proxy);
                }
                continue;
            }
            if hints.iter().any(named) {
                return Some(proxy);
            }
        }
        None
    }

    /// Whether a file of a mounted folder may hold this proxy's routes.
    fn reads(self, path: &Path) -> bool {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match self {
            Proxy::Nginx => {
                name.ends_with(".conf")
                    || name == "nginx.conf.template"
                    || name.ends_with(".conf.template")
            }
            Proxy::Caddy => name.contains("caddyfile") || name.ends_with(".caddy"),
            Proxy::Traefik => [".yml", ".yaml", ".toml"]
                .iter()
                .any(|ext| name.ends_with(ext)),
        }
    }

    /// The names a file of this proxy routes.
    pub(super) fn names_in(self, text: &str) -> Vec<String> {
        match self {
            Proxy::Nginx => nginx_server_names(text),
            Proxy::Caddy => caddyfile_sites(text),
            Proxy::Traefik => traefik_hosts(text),
        }
    }
}

/// The files a proxy service reads its routes from: what is mounted into it
/// from the project, and its build context.
pub(super) fn config_files(
    directory: &Path,
    proxy: Proxy,
    mounted: &[String],
    build: Option<&str>,
) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let sources = mounted.iter().map(String::as_str).chain(build);
    for source in sources {
        let path = directory.join(source);
        if path.is_file() {
            // Mounted as it is, whatever its name: the target says what it is.
            files.push(path);
        } else if path.is_dir() {
            collect(&path, proxy, 2, &mut files);
        }
    }
    files.sort();
    files.dedup();
    files.retain(|file| std::fs::metadata(file).is_ok_and(|meta| meta.len() <= LARGEST_FILE));
    files
}

fn collect(folder: &Path, proxy: Proxy, depth: usize, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    entries.sort();
    for path in entries {
        if files.len() >= MOST_FILES {
            return;
        }
        if path.is_dir() && depth > 0 {
            collect(&path, proxy, depth - 1, files);
        } else if path.is_file() && proxy.reads(&path) {
            files.push(path);
        }
    }
}

/// The hosts of the labels of a service: Traefik's router rules (v2 and
/// v3, and v1's `frontend.rule`), and caddy-docker-proxy's site addresses.
pub(super) fn label_hosts(key: &str, value: &str) -> Option<(Proxy, Vec<String>)> {
    let key = key.trim();
    if key.starts_with("traefik.") && key.ends_with(".rule") {
        return Some((Proxy::Traefik, traefik_hosts(value)));
    }
    let caddy = key == "caddy"
        || key
            .strip_prefix("caddy_")
            .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()));
    if caddy {
        return Some((Proxy::Caddy, addresses(value)));
    }
    None
}

/// The hostnames of `Host(…)` and `HostSNI(…)` in Traefik rules, and of a
/// v1 `Host:a,b`.
pub(super) fn traefik_hosts(text: &str) -> Vec<String> {
    let mut hosts = Vec::new();
    for marker in ["Host(", "HostSNI("] {
        let mut rest = text;
        while let Some(start) = rest.find(marker) {
            // `HostRegexp(` and the like are other matchers.
            let before = &rest[..start];
            rest = &rest[start + marker.len()..];
            if before.ends_with(|c: char| c.is_ascii_alphanumeric()) {
                continue;
            }
            let Some(end) = rest.find(')') else { break };
            for host in rest[..end].split(',') {
                hosts.push(host.trim().trim_matches(['`', '"', '\'']).to_string());
            }
            rest = &rest[end..];
        }
    }
    if let Some(v1) = text.trim().strip_prefix("Host:") {
        hosts.extend(v1.split([',', ';']).map(|host| host.trim().to_string()));
    }
    usable(hosts)
}

/// The site addresses of a Caddyfile: what comes before each top-level
/// block, the global options and snippets aside.
pub(super) fn caddyfile_sites(text: &str) -> Vec<String> {
    let mut sites = Vec::new();
    let mut depth = 0usize;
    let mut header = String::new();
    let mut first_line = None;
    for line in text.lines() {
        let line: Vec<char> = line.split('#').next().unwrap_or_default().chars().collect();
        let mut placeholder = false;
        for (index, character) in line.iter().copied().enumerate() {
            // A block's brace is followed by a space or the end of the
            // line; a placeholder's, `{$DOMAIN}` or `{host}`, is not.
            let opens =
                character == '{' && line.get(index + 1).is_none_or(|next| next.is_whitespace());
            match character {
                '{' if !opens => placeholder = true,
                '}' if placeholder => placeholder = false,
                '{' if depth == 0 => {
                    let words = std::mem::take(&mut header);
                    let words = words.trim();
                    // `{` alone opens the global options, `(name)` a snippet.
                    if !words.is_empty() && !words.starts_with('(') {
                        sites.extend(addresses(words));
                    }
                    depth = 1;
                    continue;
                }
                '{' => depth += 1,
                '}' if depth > 0 => {
                    depth -= 1;
                    continue;
                }
                _ => {}
            }
            if depth == 0 {
                header.push(character);
            }
        }
        if depth == 0 {
            // A Caddyfile of one site may have no block: its first line is
            // the address, the rest its directives.
            if first_line.is_none() && !header.trim().is_empty() {
                first_line = Some(header.trim().to_string());
            }
            header.push(' ');
        }
    }
    if sites.is_empty() {
        if let Some(line) = first_line {
            sites.extend(addresses(&line));
        }
    }
    usable(sites)
}

/// The names of nginx's `server_name` directives.
pub(super) fn nginx_server_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let text: String = text
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    for statement in text.split([';', '{', '}']) {
        let mut words = statement.split_whitespace();
        if words.next() != Some("server_name") {
            continue;
        }
        for name in words {
            // `.shop.test` stands for shop.test and its subdomains.
            names.push(name.trim_start_matches('.').to_string());
        }
    }
    usable(names)
}

/// Site addresses as Caddy writes them: `https://shop.test:443, api.test`.
fn addresses(text: &str) -> Vec<String> {
    text.split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|address| !address.is_empty())
        .map(|address| {
            let address = address
                .strip_prefix("https://")
                .or_else(|| address.strip_prefix("http://"))
                .unwrap_or(address);
            let address = address.split('/').next().unwrap_or_default();
            match address.rsplit_once(':') {
                Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host.to_string(),
                _ => address.to_string(),
            }
        })
        .collect()
}

/// What a guest could resolve: hostnames, without placeholders, wildcards
/// or patterns; lowercased, each once.
fn usable(names: Vec<String>) -> Vec<String> {
    let mut usable: Vec<String> = Vec::new();
    for name in names {
        let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
        if name.is_empty() || !devshare_protocol::names::is_hostname(&name) {
            continue;
        }
        if !usable.contains(&name) {
            usable.push(name);
        }
    }
    usable
}

/// The names `/etc/hosts` gives to this machine's loopback addresses.
pub(super) fn loopback_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default();
        let mut words = line.split_whitespace();
        let Some(address) = words
            .next()
            .and_then(|a| a.parse::<std::net::IpAddr>().ok())
        else {
            continue;
        };
        if address.is_loopback() {
            names.extend(words.map(str::to_string));
        }
    }
    usable(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traefik_rules_give_their_hosts_and_nothing_else() {
        assert_eq!(
            traefik_hosts(
                "Host(`shop.test`) || Host(`www.shop.test`, `api.shop.test`) && PathPrefix(`/v1`)"
            ),
            ["shop.test", "www.shop.test", "api.shop.test"]
        );
        assert_eq!(traefik_hosts("HostSNI(`db.shop.test`)"), ["db.shop.test"]);
        assert_eq!(traefik_hosts("Host(\"quoted.test\")"), ["quoted.test"]);
        assert_eq!(
            traefik_hosts("Host:old.test,older.test"),
            ["old.test", "older.test"]
        );
        assert!(traefik_hosts("HostRegexp(`{sub:[a-z]+}.shop.test`)").is_empty());
        assert!(traefik_hosts("Host(`${DOMAIN}`)").is_empty());
        assert!(traefik_hosts("HostSNI(`*`)").is_empty());
        // A dynamic file: rules wherever they are.
        let file = "http:\n  routers:\n    shop:\n      rule: \"Host(`shop.test`)\"\n    admin:\n      rule: Host(`admin.shop.test`)\n";
        assert_eq!(traefik_hosts(file), ["shop.test", "admin.shop.test"]);
    }

    #[test]
    fn labels_of_traefik_and_caddy_docker_proxy_are_read() {
        let (proxy, hosts) =
            label_hosts("traefik.http.routers.shop.rule", "Host(`shop.test`)").unwrap();
        assert_eq!(
            (proxy, hosts),
            (Proxy::Traefik, vec!["shop.test".to_string()])
        );
        let (proxy, hosts) = label_hosts("caddy", "shop.test, https://www.shop.test:443").unwrap();
        assert_eq!(proxy, Proxy::Caddy);
        assert_eq!(hosts, ["shop.test", "www.shop.test"]);
        assert!(label_hosts("caddy_1", "api.shop.test").is_some());
        assert!(label_hosts("caddy.reverse_proxy", "{{upstreams 80}}").is_none());
        assert!(label_hosts("traefik.http.routers.shop.entrypoints", "websecure").is_none());
        assert!(label_hosts("com.example.rule", "Host(`x.test`)").is_none());
    }

    #[test]
    fn a_caddyfile_gives_its_sites() {
        let caddyfile = "\
{
    local_certs
}

(common) {
    encode gzip
}

shop.test, https://www.shop.test:8443 {
    import common
    reverse_proxy app:8000 {
        header_up Host {host}
    }
}

# api.shop.test is commented out
http://admin.shop.test {
    respond \"admin\"
}

:2019 {
    metrics
}

{$DOMAIN} {
    respond hi
}

*.shop.test {
    respond wildcard
}
";
        assert_eq!(
            caddyfile_sites(caddyfile),
            ["shop.test", "www.shop.test", "admin.shop.test"]
        );
        // One site, no block.
        assert_eq!(
            caddyfile_sites("solo.test\n\nreverse_proxy app:8000\n"),
            ["solo.test"]
        );
    }

    #[test]
    fn nginx_gives_its_server_names() {
        let conf = "\
server {
    listen 80;
    server_name shop.test www.shop.test; # the shop
    location / { proxy_pass http://app; }
}
server {
    listen 80 default_server;
    server_name _;
}
server {
    server_name .api.shop.test ~^(?<sub>.+)\\.shop\\.test$ *.cdn.test localhost;
}
# server_name commented.test;
";
        assert_eq!(
            nginx_server_names(conf),
            ["shop.test", "www.shop.test", "api.shop.test", "localhost"]
        );
    }

    #[test]
    fn a_service_is_known_by_its_image_first() {
        assert_eq!(
            Proxy::of("proxy", Some("traefik:v3.1"), None),
            Some(Proxy::Traefik)
        );
        assert_eq!(
            Proxy::of("web", Some("nginx:alpine"), None),
            Some(Proxy::Nginx)
        );
        assert_eq!(
            Proxy::of("web", Some("lucaslorentz/caddy-docker-proxy"), None),
            Some(Proxy::Caddy)
        );
        assert_eq!(Proxy::of("nginx", Some("php:8.3-fpm"), None), None);
        assert_eq!(
            Proxy::of("web", None, Some("./docker/nginx")),
            Some(Proxy::Nginx)
        );
        assert_eq!(Proxy::of("caddy", None, Some(".")), Some(Proxy::Caddy));
        assert_eq!(Proxy::of("app", None, Some(".")), None);
    }

    #[test]
    fn hosts_file_names_of_loopback_only() {
        let hosts = "\
127.0.0.1\tlocalhost shop.test
::1 localhost api.shop.test # ipv6
192.168.1.10 nas.test
# 127.0.0.1 old.test
255.255.255.255 broadcasthost
";
        assert_eq!(
            loopback_names(hosts),
            ["localhost", "shop.test", "api.shop.test"]
        );
    }
}
