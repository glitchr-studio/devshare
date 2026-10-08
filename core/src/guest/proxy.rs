//! A web proxy on this device's loopback that carries a browser's requests
//! into the session: for a guest that cannot have a network interface, the
//! iPhone app without its packet-tunnel extension, whose own browser is
//! pointed at it.
//!
//! `CONNECT shop.test:443` opens a stream to that service on the host and
//! carries the bytes as they are: TLS stays end to end, and the browser
//! checks the certificate itself. A plain `GET http://shop.test/` is sent on
//! as an ordinary request, closing the connection after its answer so that
//! the next request, maybe for another name, gets a stream of its own.
//!
//! Every request must carry the proxy's password: other programs of the
//! device can reach its loopback too.
//!
//! A browser sends everything through its proxy, the fonts and scripts a
//! page takes from the internet too: with [`Reach::Internet`], names that
//! are not the session's are reached directly from this device, as without
//! a proxy. The session's names are never looked up anywhere else.

use std::{future::Future, net::SocketAddr, sync::Arc};

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use devshare_protocol::normalize_host;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

use super::{AddressPlan, OpenError, Opener};

/// The user name of the proxy's credentials; the password is drawn at start.
pub const USER: &str = "devshare";
/// The longest request head read.
const LONGEST_HEAD: usize = 16 * 1024;

/// What the proxy reaches besides the session's services.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    SessionOnly,
    /// Other names directly, from this device: a browser's proxy.
    Internet,
}

/// What the proxy reaches services with: the session, or anything else in
/// tests.
pub trait Dial: Send + Sync + 'static {
    type Stream: AsyncRead + AsyncWrite + Unpin + Send;
    fn dial(
        &self,
        host: &str,
        port: u16,
    ) -> impl Future<Output = Result<Self::Stream, OpenError>> + Send;
}

impl Dial for Opener {
    type Stream = tokio::io::Join<iroh::endpoint::RecvStream, iroh::endpoint::SendStream>;

    async fn dial(&self, host: &str, port: u16) -> Result<Self::Stream, OpenError> {
        let (send, recv) = self.open(host, port).await?;
        Ok(tokio::io::join(recv, send))
    }
}

/// The running proxy. Dropping it stops it.
pub struct Proxy {
    address: SocketAddr,
    password: String,
    task: JoinHandle<()>,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Proxy {
    /// Starts the proxy on a port of the loopback the system picks. Only the
    /// services `plan` lists are reached.
    pub async fn start<D: Dial>(dialer: D, plan: Arc<AddressPlan>, reach: Reach) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("opening the proxy's port")?;
        let address = listener.local_addr()?;
        let password: String = rand::random::<[u8; 16]>()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let expected = Arc::new(format!(
            "Basic {}",
            STANDARD.encode(format!("{USER}:{password}"))
        ));
        let dialer = Arc::new(dialer);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (dialer, plan, expected) = (dialer.clone(), plan.clone(), expected.clone());
                tokio::spawn(async move {
                    if let Err(error) = serve(stream, &*dialer, &plan, &expected, reach).await {
                        tracing::debug!("proxy: {error:#}");
                    }
                });
            }
        });
        Ok(Self {
            address,
            password,
            task,
        })
    }

    pub fn port(&self) -> u16 {
        self.address.port()
    }

    pub fn password(&self) -> &str {
        &self.password
    }
}

async fn serve<D: Dial>(
    mut client: TcpStream,
    dialer: &D,
    plan: &AddressPlan,
    expected: &str,
    reach: Reach,
) -> Result<()> {
    let (head, rest) = read_head(&mut client).await?;
    let Some(request) = Request::parse(&head) else {
        return answer(&mut client, "400 Bad Request").await;
    };
    if request.header("proxy-authorization") != Some(expected) {
        let refusal = "HTTP/1.1 407 Proxy Authentication Required\r\n\
                       Proxy-Authenticate: Basic realm=\"DevShare\"\r\n\
                       Content-Length: 0\r\nConnection: close\r\n\r\n";
        client.write_all(refusal.as_bytes()).await?;
        return Ok(());
    }
    let Some((host, port, forwarded)) = request.destination() else {
        return answer(&mut client, "400 Bad Request").await;
    };
    if !plan.shares(&host, port) {
        // A name of the session on a port it does not share is refused, here
        // and on the host; any other name is the internet's, when allowed.
        let elsewhere = reach == Reach::Internet && plan.address_of(&host).is_none();
        if !elsewhere {
            return answer(&mut client, "403 Forbidden").await;
        }
        return match TcpStream::connect((host.as_str(), port)).await {
            Ok(origin) => relay(client, origin, forwarded, rest).await,
            Err(_) => answer(&mut client, "502 Bad Gateway").await,
        };
    }
    let origin = match dialer.dial(&host, port).await {
        Ok(origin) => origin,
        Err(OpenError::Denied) => return answer(&mut client, "403 Forbidden").await,
        Err(error) => {
            tracing::debug!("proxy: {host}:{port}: {error}");
            return answer(&mut client, "502 Bad Gateway").await;
        }
    };
    relay(client, origin, forwarded, rest).await
}

/// Carries a request to `origin`, and everything after it both ways.
async fn relay(
    mut client: TcpStream,
    mut origin: impl AsyncRead + AsyncWrite + Unpin,
    forwarded: Option<String>,
    rest: Vec<u8>,
) -> Result<()> {
    match forwarded {
        // A tunnel: the browser speaks to the service itself from now on.
        None => {
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
        }
        Some(head) => origin.write_all(head.as_bytes()).await?,
    }
    origin.write_all(&rest).await?;
    match tokio::io::copy_bidirectional(&mut client, &mut origin).await {
        Err(error) if error.kind() != std::io::ErrorKind::UnexpectedEof => Err(error.into()),
        _ => Ok(()),
    }
}

async fn answer(client: &mut TcpStream, status: &str) -> Result<()> {
    let reply = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    client.write_all(reply.as_bytes()).await?;
    Ok(())
}

/// The request head, and whatever came after it in the same reads.
async fn read_head(client: &mut TcpStream) -> Result<(String, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let rest = buffer.split_off(end + 4);
            let head = String::from_utf8(buffer).context("a request head that is not text")?;
            return Ok((head, rest));
        }
        if buffer.len() > LONGEST_HEAD {
            anyhow::bail!("a request head longer than {LONGEST_HEAD} bytes");
        }
        let read = client.read(&mut chunk).await?;
        if read == 0 {
            anyhow::bail!("the client closed before its request was complete");
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

struct Request<'a> {
    method: &'a str,
    target: &'a str,
    version: &'a str,
    headers: Vec<(&'a str, &'a str)>,
}

impl<'a> Request<'a> {
    fn parse(head: &'a str) -> Option<Self> {
        let mut lines = head.split("\r\n").filter(|line| !line.is_empty());
        let mut first = lines.next()?.split(' ');
        let (method, target, version) = (first.next()?, first.next()?, first.next()?);
        if first.next().is_some() || !version.starts_with("HTTP/1.") {
            return None;
        }
        let headers = lines
            .map(|line| {
                let (name, value) = line.split_once(':')?;
                Some((name.trim(), value.trim()))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            method,
            target,
            version,
            headers,
        })
    }

    fn header(&self, wanted: &str) -> Option<&'a str> {
        self.headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
            .map(|(_, value)| *value)
    }

    /// Where the request goes, and the head to send there for a plain
    /// request (`None` for a tunnel).
    fn destination(&self) -> Option<(String, u16, Option<String>)> {
        if self.method.eq_ignore_ascii_case("CONNECT") {
            let (host, port) = self.target.rsplit_once(':')?;
            return Some((normalize_host(host), port.parse().ok()?, None));
        }
        let rest = self.target.strip_prefix("http://")?;
        let (authority, path) = match rest.find('/') {
            Some(slash) => rest.split_at(slash),
            None => (rest, "/"),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().ok()?),
            None => (authority, 80),
        };
        // The proxy's own headers stay here; the connection closes after
        // the answer, the next request may be for another service.
        let mut head = format!("{} {path} {}\r\n", self.method, self.version);
        for (name, value) in &self.headers {
            let hop = [
                "proxy-authorization",
                "proxy-connection",
                "connection",
                "keep-alive",
            ]
            .iter()
            .any(|hop| name.eq_ignore_ascii_case(hop));
            if !hop {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
        }
        head.push_str("Connection: close\r\n\r\n");
        Some((normalize_host(host), port, Some(head)))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tokio::io::AsyncBufReadExt;

    use super::*;
    use crate::guest::addresses::tests::manifest;

    /// Services of the "host": a name and port to a local listener.
    struct Local(HashMap<(String, u16), SocketAddr>);

    impl Dial for Local {
        type Stream = TcpStream;

        async fn dial(&self, host: &str, port: u16) -> Result<TcpStream, OpenError> {
            let address = self
                .0
                .get(&(host.to_string(), port))
                .ok_or(OpenError::Unreachable)?;
            TcpStream::connect(address)
                .await
                .map_err(|error| OpenError::Failed(error.into()))
        }
    }

    /// A service that answers with the head it received, then closes.
    async fn mirror() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut reader = tokio::io::BufReader::new(stream);
                    let mut head = String::new();
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).await.unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        head.push_str(&line);
                    }
                    let body = format!("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{head}");
                    reader.get_mut().write_all(body.as_bytes()).await.ok();
                });
            }
        });
        address
    }

    async fn proxy() -> Proxy {
        let shop = mirror().await;
        let plan = AddressPlan::new(&manifest(&[
            ("shop.test", 443),
            ("shop.test", 80),
            ("api.test", 8080),
        ]))
        .unwrap();
        let services = HashMap::from([
            (("shop.test".to_string(), 443), shop),
            (("shop.test".to_string(), 80), shop),
        ]);
        Proxy::start(Local(services), Arc::new(plan), Reach::SessionOnly)
            .await
            .unwrap()
    }

    async fn ask(proxy: &Proxy, request: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", proxy.port()))
            .await
            .unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await.ok();
        answer
    }

    fn credentials(proxy: &Proxy) -> String {
        format!(
            "Proxy-Authorization: Basic {}\r\n",
            STANDARD.encode(format!("{USER}:{}", proxy.password()))
        )
    }

    #[tokio::test]
    async fn a_tunnel_reaches_the_service_and_carries_bytes_as_they_are() {
        let proxy = proxy().await;
        let request = format!(
            "CONNECT shop.test:443 HTTP/1.1\r\nHost: shop.test:443\r\n{}\r\nGET /inside HTTP/1.1\r\nX-Through: tunnel\r\n\r\n",
            credentials(&proxy)
        );
        let answer = ask(&proxy, &request).await;
        assert!(
            answer.starts_with("HTTP/1.1 200 Connection Established\r\n\r\nHTTP/1.1 200 OK"),
            "{answer}"
        );
        assert!(answer.contains("GET /inside HTTP/1.1") && answer.contains("X-Through: tunnel"));
    }

    #[tokio::test]
    async fn a_plain_request_goes_on_as_an_ordinary_one_without_the_proxy_s_headers() {
        let proxy = proxy().await;
        let request = format!(
            "GET http://Shop.test/cart?x=1 HTTP/1.1\r\nHost: shop.test\r\n{}Proxy-Connection: keep-alive\r\nAccept: */*\r\n\r\n",
            credentials(&proxy)
        );
        let answer = ask(&proxy, &request).await;
        assert!(answer.contains("GET /cart?x=1 HTTP/1.1\r\n"), "{answer}");
        assert!(answer.contains("Accept: */*") && answer.contains("Connection: close"));
        assert!(!answer.contains("Proxy-") && !answer.contains(proxy.password()));
    }

    #[tokio::test]
    async fn without_its_password_or_outside_the_session_nothing_goes_through() {
        let proxy = proxy().await;
        let answer = ask(&proxy, "CONNECT shop.test:443 HTTP/1.1\r\n\r\n").await;
        assert!(answer.starts_with("HTTP/1.1 407"), "{answer}");
        let wrong = "Proxy-Authorization: Basic ZGV2c2hhcmU6d3Jvbmc=\r\n";
        let answer = ask(
            &proxy,
            &format!("CONNECT shop.test:443 HTTP/1.1\r\n{wrong}\r\n"),
        )
        .await;
        assert!(answer.starts_with("HTTP/1.1 407"), "{answer}");

        let auth = credentials(&proxy);
        let answer = ask(
            &proxy,
            &format!("CONNECT glitchr.dev:443 HTTP/1.1\r\n{auth}\r\n"),
        )
        .await;
        assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
        let answer = ask(
            &proxy,
            &format!("CONNECT shop.test:22 HTTP/1.1\r\n{auth}\r\n"),
        )
        .await;
        assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
        // Shared, but nothing answers on the host.
        let answer = ask(
            &proxy,
            &format!("CONNECT api.test:8080 HTTP/1.1\r\n{auth}\r\n"),
        )
        .await;
        assert!(answer.starts_with("HTTP/1.1 502"), "{answer}");
        let answer = ask(&proxy, &format!("GET /relative HTTP/1.1\r\n{auth}\r\n")).await;
        assert!(answer.starts_with("HTTP/1.1 400"), "{answer}");
    }

    #[tokio::test]
    async fn a_browser_s_proxy_reaches_other_names_directly_but_never_the_session_s_elsewhere() {
        let elsewhere = mirror().await;
        let plan = AddressPlan::new(&manifest(&[("shop.test", 443)])).unwrap();
        let proxy = Proxy::start(Local(HashMap::new()), Arc::new(plan), Reach::Internet)
            .await
            .unwrap();
        let auth = credentials(&proxy);
        // Another name: straight from this device.
        let answer = ask(
            &proxy,
            &format!("GET http://{elsewhere}/font.css HTTP/1.1\r\n{auth}\r\n"),
        )
        .await;
        assert!(answer.contains("GET /font.css HTTP/1.1"), "{answer}");
        // A name of the session on a port it does not share: refused, not
        // looked up outside.
        let answer = ask(
            &proxy,
            &format!("CONNECT shop.test:8443 HTTP/1.1\r\n{auth}\r\n"),
        )
        .await;
        assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
    }
}
