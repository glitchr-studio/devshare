//! Whose a port is. A port that answers is not always the project's: another
//! program may hold it (an editor's extension on 9000, a relay on 9001). For
//! a Docker Compose project, Docker says which ports its running containers
//! publish; for any port, the system says which program listens on it.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const COMPOSE_FILES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

/// The ports published by the running containers of a Docker Compose
/// project: the ones started from its folder, or a folder inside it. `None`
/// when the folder has no Compose file, or Docker cannot be asked at all
/// (the ports alone then tell).
pub fn published(folder: &Path) -> Option<Vec<u16>> {
    if !COMPOSE_FILES.iter().any(|file| folder.join(file).is_file()) {
        return None;
    }
    let docker = crate::system::program("docker");
    let mut command = Command::new(docker);
    command
        .args([
            "ps",
            "--format",
            "{{.Label \"com.docker.compose.project.working_dir\"}}\t{{.Ports}}",
        ])
        .env("PATH", crate::system::path());
    let (success, stdout, stderr) = run(command, Duration::from_secs(4))?;
    if !success {
        // Docker is not running: neither are the project's containers.
        let stopped = stderr.contains("Cannot connect") || stderr.contains("daemon");
        return stopped.then(Vec::new);
    }
    let folder = folder
        .canonicalize()
        .unwrap_or_else(|_| folder.to_path_buf());
    Some(ports_of(&stdout, &folder))
}

/// The published ports of the lines of `docker ps` whose Compose folder is
/// `folder` or inside it.
fn ports_of(listing: &str, folder: &Path) -> Vec<u16> {
    let mut ports = Vec::new();
    for line in listing.lines() {
        let Some((directory, published)) = line.split_once('\t') else {
            continue;
        };
        if directory.is_empty() || !Path::new(directory).starts_with(folder) {
            continue;
        }
        // 0.0.0.0:9000->9000/tcp, [::]:9000->9000/tcp, 443/udp
        for mapping in published.split(", ") {
            let Some((outside, inside)) = mapping.split_once("->") else {
                continue;
            };
            if !inside.ends_with("/tcp") {
                continue;
            }
            let Some((_, range)) = outside.rsplit_once(':') else {
                continue;
            };
            let (first, last) = range.split_once('-').unwrap_or((range, range));
            if let (Ok(first), Ok(last)) = (first.parse::<u16>(), last.parse::<u16>()) {
                for port in first..=last.min(first.saturating_add(100)) {
                    if !ports.contains(&port) {
                        ports.push(port);
                    }
                }
            }
        }
    }
    ports
}

/// The program listening on a port of this machine, by the name its user
/// knows: "Visual Studio Code" rather than "Code Helper (Plugin)".
pub fn holder(port: u16) -> Option<String> {
    let mut lsof = Command::new("lsof");
    lsof.args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"]);
    let (_, pids, _) = run(lsof, Duration::from_secs(2))?;
    let pid = pids.lines().next()?.trim().to_string();
    let mut ps = Command::new("ps");
    ps.args(["-o", "comm=", "-p", &pid]);
    let (_, program, _) = run(ps, Duration::from_secs(2))?;
    name_of(program.trim())
}

/// The outermost application of a program's path, else its file name.
fn name_of(program: &str) -> Option<String> {
    if program.is_empty() {
        return None;
    }
    let application = program
        .split('/')
        .find_map(|part| part.strip_suffix(".app"))
        .filter(|name| !name.is_empty());
    let name = match application {
        Some(name) => name,
        None => program.rsplit('/').next().unwrap_or(program),
    };
    Some(name.to_string())
}

/// Whether a program is Docker's: published ports are held by it.
pub fn is_docker(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "docker",
        "com.docke",
        "vpnkit",
        "orbstack",
        "limactl",
        "colima",
    ]
    .iter()
    .any(|known| name.contains(known))
}

/// Runs a short command, given up on after `limit`.
fn run(mut command: Command, limit: Duration) -> Option<(bool, String, String)> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < limit => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    Some((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ports_of_the_project_s_containers_only() {
        let listing = "/Users/me/Sites/shop\t0.0.0.0:80->80/tcp, [::]:80->80/tcp, 0.0.0.0:443->443/tcp, 443/udp\n\
                       /Users/me/Sites/shop/deployments\t127.0.0.1:9000-9001->9000-9001/tcp\n\
                       /Users/me/Sites/shopping\t0.0.0.0:8080->80/tcp\n\
                       \t0.0.0.0:5432->5432/tcp\n\
                       /Users/me/Sites/shop\t0.0.0.0:5353->53/udp";
        let mut ports = ports_of(listing, Path::new("/Users/me/Sites/shop"));
        ports.sort();
        assert_eq!(ports, vec![80, 443, 9000, 9001]);
        assert!(ports_of(listing, Path::new("/Users/me/Sites/other")).is_empty());
    }

    #[test]
    fn programs_by_the_names_their_users_know() {
        assert_eq!(
            name_of("/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper (Plugin).app/Contents/MacOS/Code Helper (Plugin)").as_deref(),
            Some("Visual Studio Code")
        );
        assert_eq!(name_of("/usr/local/bin/node").as_deref(), Some("node"));
        assert_eq!(name_of("php-fpm").as_deref(), Some("php-fpm"));
        assert_eq!(name_of(""), None);
        assert!(is_docker("com.docker.backend"));
        assert!(!is_docker("Visual Studio Code"));
    }
}
