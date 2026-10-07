//! Starting the control plane for real: on a port of its own, twice on the
//! same port, and on a port another program holds.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Server {
    process: Child,
    /// What it printed until it said where it listens.
    said: Vec<String>,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.process.kill().ok();
        self.process.wait().ok();
    }
}

fn command(listen: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_devshare-server"));
    command
        .env("DEVSHARE_LISTEN", listen)
        .env("RUST_LOG", "off")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Starts a control plane and waits until it says where it listens.
fn start(listen: &str) -> Server {
    // Owned at once: whatever happens below, it is stopped and waited for.
    let mut server = Server {
        process: command(listen).spawn().unwrap(),
        said: Vec::new(),
        port: 0,
    };
    let output = server.process.stdout.take().unwrap();
    for line in BufReader::new(output).lines() {
        let line = line.unwrap();
        server.said.push(line.clone());
        if let Some(port) = line.strip_prefix("Control plane listening on http://localhost:") {
            server.port = port.parse().unwrap();
            return server;
        }
    }
    panic!("it stopped without listening: {:?}", server.said);
}

/// What a port answers to `GET /v2`.
fn identity(port: u16) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(b"GET /v2 HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    answer
}

#[test]
fn it_listens_on_a_port_of_the_systems_choosing_and_says_which() {
    let server = start("127.0.0.1:0");
    assert_ne!(server.port, 0);
    assert!(identity(server.port).ends_with(devshare_server::IDENTITY));
}

#[test]
fn started_twice_on_one_port_the_second_says_one_is_running_and_succeeds() {
    let first = start("127.0.0.1:0");

    let second = command(&format!("127.0.0.1:{}", first.port))
        .output()
        .unwrap();
    assert!(second.status.success(), "starting it again is not an error");
    let said = String::from_utf8(second.stdout).unwrap();
    assert!(
        said.contains(&format!(
            "already running at http://localhost:{}",
            first.port
        )),
        "{said}"
    );
    assert!(said.contains("Nothing more to start"), "{said}");

    // The first one was not disturbed.
    assert!(identity(first.port).ends_with(devshare_server::IDENTITY));
}

#[test]
fn a_port_held_by_another_program_is_left_to_it() {
    // Something that is not a control plane.
    let other = TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = other.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in other.incoming() {
            stream
                .unwrap()
                .write_all(b"HTTP/1.0 200 OK\r\n\r\nsomething else\n")
                .ok();
        }
    });

    let server = start(&format!("127.0.0.1:{taken}"));
    assert_ne!(server.port, taken);
    assert!(server
        .said
        .contains(&format!("Port {taken} is taken by another program.")));
    assert!(server.said.contains(&format!(
        "  export DEVSHARE_SERVER=http://localhost:{}",
        server.port
    )));
    assert!(identity(server.port).ends_with(devshare_server::IDENTITY));
}

#[test]
fn it_takes_no_command() {
    let mistaken = Command::new(env!("CARGO_BIN_EXE_devshare-server"))
        .arg("share")
        .output()
        .unwrap();
    assert_eq!(mistaken.status.code(), Some(2));
    assert!(String::from_utf8(mistaken.stderr)
        .unwrap()
        .contains("use: devshare share"));
}
