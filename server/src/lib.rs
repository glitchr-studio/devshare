//! The control plane. It maps an invitation code to the address of a host
//! for a limited time, and knows nothing else: not what is shared, not who
//! joined.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::{ConnectInfo, DefaultBodyLimit, Path, State},
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use devshare_protocol::{code, CreateInvitation, InvitationCreated, InvitationLookup};

/// An invitation never outlives this, whatever the host asks for.
const MAX_TTL: u64 = 24 * 60 * 60;
const MAX_INVITATIONS: usize = 100_000;
const MAX_BODY: usize = 16 * 1024;
/// A host's address is a key and a few relays or sockets: anything bigger
/// is not one.
const MAX_HOST: usize = 2048;

/// What one address may ask, so that codes cannot be guessed at speed and
/// the registry cannot be filled: a bucket per address, refilled one token a
/// second. A lookup or a page costs one, an invitation six.
const BURST: f64 = 60.0;
const REFILL_PER_SECOND: f64 = 1.0;
const COST_LOOKUP: f64 = 1.0;
const COST_CREATE: f64 = 6.0;

struct Bucket {
    tokens: f64,
    at: Instant,
}

struct Invitation {
    host: serde_json::Value,
    owner_token: String,
    expires: Instant,
}

#[derive(Clone, Default)]
pub struct Registry {
    invitations: Arc<Mutex<HashMap<String, Invitation>>>,
    buckets: Arc<Mutex<HashMap<IpAddr, Bucket>>>,
}

impl Registry {
    fn purge(&self) {
        let now = Instant::now();
        self.invitations
            .lock()
            .unwrap()
            .retain(|_, invitation| invitation.expires > now);
        self.buckets
            .lock()
            .unwrap()
            .retain(|_, bucket| now.duration_since(bucket.at) < Duration::from_secs(3600));
    }

    /// Whether `from` may spend `cost` now. This machine itself always may:
    /// it is the host's own process, or the server's.
    fn allow(&self, from: IpAddr, cost: f64) -> bool {
        if from.is_loopback() {
            return true;
        }
        let now = Instant::now();
        let mut buckets = self.buckets.lock().unwrap();
        let bucket = buckets.entry(from).or_insert(Bucket {
            tokens: BURST,
            at: now,
        });
        let refilled =
            bucket.tokens + now.duration_since(bucket.at).as_secs_f64() * REFILL_PER_SECOND;
        bucket.tokens = refilled.min(BURST);
        bucket.at = now;
        if bucket.tokens >= cost {
            bucket.tokens -= cost;
            true
        } else {
            false
        }
    }
}

/// Equal without saying, by how long it took, where they differ.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// What `GET /v1` answers: how a control plane is told from anything else
/// that listens on a port.
pub const IDENTITY: &str = devshare_protocol::CONTROL_PLANE;

/// Whether a DevShare control plane answers at `address`.
pub async fn answers_at(address: std::net::SocketAddr) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let asked = async {
        let mut stream = tokio::net::TcpStream::connect(address).await.ok()?;
        stream
            .write_all(b"GET /v1 HTTP/1.0\r\nHost: localhost\r\n\r\n")
            .await
            .ok()?;
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await.ok()?;
        Some(answer.ends_with(IDENTITY))
    };
    tokio::time::timeout(Duration::from_secs(2), asked)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// Makes sure a control plane answers at `server` when `server` names this
/// machine: if nothing listens there, one is started inside this process,
/// on the loopback interface only, and lives as long as the process does.
/// Returns whether one was started.
///
/// Loopback only: on a shared network anyone nearby could otherwise query
/// it. The invitation link and the QR code need no control plane at all, so
/// nothing is lost for guests; the short code typed by hand is for the day
/// a public control plane exists.
///
/// A control plane somewhere else is not this function's business, and
/// neither is a port something already listens on.
pub async fn ensure_local(server: &str) -> std::io::Result<bool> {
    let Some(port) = local_port(server) else {
        return Ok(false);
    };
    let here = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    if tokio::net::TcpStream::connect(here).await.is_ok() {
        return Ok(false);
    }
    let listener = tokio::net::TcpListener::bind(here).await?;
    tokio::spawn(serve(listener));
    Ok(true)
}

/// Whether `server` names a control plane on this machine: one that someone
/// on another network cannot reach.
pub fn is_local(server: &str) -> bool {
    local_port(server).is_some()
}

/// The port of `http://localhost:8787`, when the address is this machine.
fn local_port(server: &str) -> Option<u16> {
    let authority = server.strip_prefix("http://")?.split('/').next()?;
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.ends_with(']') => (host, port.parse().ok()?),
        _ => (authority, 80),
    };
    matches!(host, "localhost" | "127.0.0.1" | "[::1]").then_some(port)
}

pub fn router(registry: Registry) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1", get(|| async { IDENTITY }))
        .route("/v1/invitations", post(create))
        .route("/v1/invitations/{code}", get(lookup).delete(withdraw))
        .route("/{code}", get(page))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(registry)
}

/// Serves until the listener fails, forgetting expired invitations as it goes.
pub async fn serve(listener: tokio::net::TcpListener) -> std::io::Result<()> {
    let registry = Registry::default();
    tokio::spawn({
        let registry = registry.clone();
        async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                registry.purge();
            }
        }
    });
    // With the address of whoever asks: the page says who opened it.
    let service = router(registry).into_make_service_with_connect_info::<std::net::SocketAddr>();
    axum::serve(listener, service).await
}

async fn create(
    State(registry): State<Registry>,
    ConnectInfo(from): ConnectInfo<SocketAddr>,
    Json(request): Json<CreateInvitation>,
) -> Result<Json<InvitationCreated>, StatusCode> {
    if !registry.allow(from.ip(), COST_CREATE) {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    if request.ttl == 0 {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    if request.host.to_string().len() > MAX_HOST {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let ttl = request.ttl.min(MAX_TTL);
    let owner_token: String = rand::random::<[u8; 32]>()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    registry.purge();
    let mut invitations = registry.invitations.lock().unwrap();
    if invitations.len() >= MAX_INVITATIONS {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let code = loop {
        let candidate = code::from_random(rand::random());
        if !invitations.contains_key(&candidate) {
            break candidate;
        }
    };
    invitations.insert(
        code.clone(),
        Invitation {
            host: request.host,
            owner_token: owner_token.clone(),
            expires: Instant::now() + Duration::from_secs(ttl),
        },
    );
    Ok(Json(InvitationCreated {
        code,
        owner_token,
        ttl,
    }))
}

async fn lookup(
    State(registry): State<Registry>,
    ConnectInfo(from): ConnectInfo<SocketAddr>,
    Path(code): Path<String>,
) -> Result<Json<InvitationLookup>, StatusCode> {
    if !registry.allow(from.ip(), COST_LOOKUP) {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    let invitations = registry.invitations.lock().unwrap();
    match invitations.get(&code) {
        Some(invitation) if invitation.expires > Instant::now() => Ok(Json(InvitationLookup {
            host: invitation.host.clone(),
        })),
        _ => Err(StatusCode::NOT_FOUND),
    }
}

/// What a browser gets when it opens an invitation link, on a phone that
/// scanned the QR code for instance: the code, and what to do with it.
async fn page(
    State(registry): State<Registry>,
    ConnectInfo(from): ConnectInfo<SocketAddr>,
    Path(asked): Path<String>,
) -> (StatusCode, axum::response::Html<String>) {
    if !registry.allow(from.ip(), COST_LOOKUP) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            axum::response::Html(PAGE.replace("{body}", PAGE_SLOW_DOWN)),
        );
    }
    // Said to whoever runs this: the one sign that a device which scanned
    // the QR code did reach this machine.
    tracing::info!("the invitation page was opened from {}", from.ip());
    let known = code::parse(&asked).filter(|code| {
        let invitations = registry.invitations.lock().unwrap();
        invitations
            .get(code)
            .is_some_and(|invitation| invitation.expires > Instant::now())
    });
    let (status, body) = match known {
        // The code is made of letters and digits of its own alphabet only:
        // nothing of the request is written back as it came.
        Some(code) => (
            StatusCode::OK,
            PAGE_INVITED.replace("{code}", &code::display(&code)),
        ),
        None => (StatusCode::NOT_FOUND, PAGE_GONE.to_string()),
    };
    (status, axum::response::Html(PAGE.replace("{body}", &body)))
}

const PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>DevShare invitation</title>
<style>
:root { color-scheme: light dark; }
body { font: 16px/1.5 system-ui, sans-serif; margin: 12vh auto 0; max-width: 30rem; padding: 0 1.25rem; }
h1 { font-size: 1.15rem; margin: 0 0 1.5rem; }
.code { font: 2rem ui-monospace, Menlo, Consolas, monospace; letter-spacing: 0.08em; margin: 0 0 1.5rem; }
code { font-family: ui-monospace, Menlo, Consolas, monospace; font-size: 0.92em; word-break: break-all; }
p { margin: 0 0 1rem; }
.muted { opacity: 0.65; }
</style>
</head>
<body>
{body}
</body>
</html>
"#;

const PAGE_INVITED: &str = r#"<h1>You are invited to a DevShare session</h1>
<p class="code">{code}</p>
<p>On a computer with DevShare, join with this page's address:</p>
<p><code id="command">devshare join {code}</code></p>
<p class="muted">DevShare for iPhone, iPad and Android is not available yet: this session can be joined from a computer only.</p>
<script>document.getElementById('command').textContent = 'devshare join ' + location.href;</script>"#;

const PAGE_SLOW_DOWN: &str = r#"<h1>Too many requests</h1>
<p>This address asked for too many pages in a short time. Try again in a minute.</p>"#;

const PAGE_GONE: &str = r#"<h1>This invitation is no longer valid</h1>
<p>It expired, was withdrawn, or was mistyped. Ask whoever shared it for a new one.</p>"#;

async fn withdraw(
    State(registry): State<Registry>,
    ConnectInfo(from): ConnectInfo<SocketAddr>,
    Path(code): Path<String>,
    headers: HeaderMap,
) -> StatusCode {
    if !registry.allow(from.ip(), COST_LOOKUP) {
        return StatusCode::TOO_MANY_REQUESTS;
    }
    let presented = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let mut invitations = registry.invitations.lock().unwrap();
    match (invitations.get(&code), presented) {
        (Some(invitation), Some(token))
            if constant_time_eq(invitation.owner_token.as_bytes(), token.as_bytes()) =>
        {
            invitations.remove(&code);
            StatusCode::NO_CONTENT
        }
        // The same answer whether the code exists or not.
        _ => StatusCode::NOT_FOUND,
    }
}

#[cfg(test)]
mod tests {
    use super::{local_port, Registry, BURST, COST_CREATE, COST_LOOKUP};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn an_address_gets_a_burst_then_one_token_a_second_and_loopback_is_free() {
        let registry = Registry::default();
        let far = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
        let allowed = (0..200)
            .filter(|_| registry.allow(far, COST_LOOKUP))
            .count();
        assert_eq!(allowed as f64, BURST);
        assert!(!registry.allow(far, COST_LOOKUP));
        // Another address has its own bucket; creating costs more.
        let other = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 8));
        let creates = (0..100)
            .filter(|_| registry.allow(other, COST_CREATE))
            .count();
        assert_eq!(creates as f64, (BURST / COST_CREATE).floor());
        for _ in 0..500 {
            assert!(registry.allow(IpAddr::V4(Ipv4Addr::LOCALHOST), COST_CREATE));
            assert!(registry.allow(IpAddr::V6(Ipv6Addr::LOCALHOST), COST_LOOKUP));
        }
    }

    #[test]
    fn only_this_machine_is_local() {
        assert_eq!(local_port("http://localhost:8787"), Some(8787));
        assert_eq!(local_port("http://127.0.0.1:9000/"), Some(9000));
        assert_eq!(local_port("http://[::1]:9000"), Some(9000));
        assert_eq!(local_port("http://localhost"), Some(80));
        assert_eq!(local_port("http://10.0.0.5:8787"), None);
        assert_eq!(local_port("https://join.example"), None);
        assert_eq!(local_port("https://localhost:8787"), None);
    }
}
