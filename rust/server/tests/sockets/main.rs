//! The running server over real sockets: HTTP/1, HTTP/2 and HTTP/3 transfers and their endings, connection bounds,
//! delayed links, WebSocket buses, WebTransport sessions and TLS.

mod auth;
mod connection;
mod delayed;
mod http2;
mod http3;
mod quic;
mod shards;
mod shutdown;
mod tls;
mod transfer;
mod websocket;
mod webtransport;
mod webtransport_upload;

use graphite_meter_net::Pool;
use graphite_meter_server::{
    app::{App, Endpoint},
    config::{self, Config, Loaded},
    limits::Budget,
    runtime::Server,
};
use std::{ffi::OsString, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::TcpStream,
    runtime::Handle,
    sync::oneshot,
    task::JoinHandle,
};

/// A download no test waits to finish.
const ENDLESS: &str = "/download?bytes=68719476736";

/// Moves paused time on by `duration` while real network waits keep the running clock.
async fn advance_clock(duration: Duration) {
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::task::yield_now().await;
    tokio::time::resume();
}

fn config(env: &[(&str, &str)]) -> Config {
    let lookup = |name: &str| {
        let given = env.iter().find(|(key, _)| *key == name).map(|(_, value)| *value);
        let local = (name == "GM_H1_ADDR").then_some("127.0.0.1:0");
        given.or(local).map(OsString::from)
    };
    match config::load(lookup, Vec::<OsString>::new(), &mut Vec::new()) {
        Ok(Loaded::Config(config)) => *config,
        other => panic!("{env:?}: {other:?}"),
    }
}

/// Pinned runtimes beside a multi-thread test runtime, else none.
fn pool() -> Pool {
    Pool::beside(&Handle::current()).unwrap_or_else(|_| Pool::inline())
}

/// A server serving on local ports until stopped.
struct Running {
    address: SocketAddr,
    tls: Option<SocketAddr>,
    h2: Option<SocketAddr>,
    quic: Option<SocketAddr>,
    companion: Option<SocketAddr>,
    /// The endpoints sharing the HTTP/3 port.
    endpoints: usize,
    budget: Budget,
    app: Arc<App>,
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<(), String>>,
}

/// Binds a server; an HTTP/3 port that another test took between its TCP and UDP binds is tried again.
async fn bind(env: &[(&str, &str)]) -> Server {
    for _ in 0..8 {
        match Server::bind(config(env), pool()).await {
            Err(error) if error.starts_with("listen udp") && error.ends_with("address already in use") => continue,
            bound => return bound.unwrap(),
        }
    }
    panic!("no free HTTP/3 port pair")
}

async fn start(env: &[(&str, &str)]) -> Running {
    let server = bind(env).await;
    let (address, tls) = (server.local_addr(Endpoint::H1).unwrap(), server.local_addr(Endpoint::H1Tls));
    let (h2, quic) = (server.local_addr(Endpoint::H2), server.local_addr(Endpoint::Quic));
    let (companion, budget) = (server.local_addr(Endpoint::H3Companion), server.budget());
    let (endpoints, app) = (server.quic_endpoints(), server.app());
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.serve(async {
        let _ = stopped.await;
    }));
    Running {
        address,
        tls,
        h2,
        quic,
        companion,
        endpoints,
        budget,
        app,
        stop,
        task,
    }
}

impl Running {
    /// Starts the shutdown; the task ends once connections drained.
    fn stop(self) -> JoinHandle<Result<(), String>> {
        self.stop.send(()).unwrap();
        self.task
    }

    async fn connect(&self) -> Client<TcpStream> {
        Client::new(TcpStream::connect(self.address).await.unwrap())
    }

    /// One request on a connection of its own that the server closes after answering.
    async fn get(&self, path: &str) -> Answer {
        let mut client = self.connect().await;
        client
            .send(&format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n"))
            .await;
        client.answer().await.unwrap()
    }

    /// The handlers in use, as `/probe` reports them.
    async fn active(&self) -> u64 {
        let probe: serde_json::Value = serde_json::from_slice(&self.get("/probe").await.body).unwrap();
        probe["load"]["active"].as_u64().unwrap()
    }

    /// Waits until `/probe` reports `active` handlers.
    async fn until_active(&self, active: u64) {
        let reached = async {
            while self.active().await != active {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), reached).await.unwrap();
    }

    async fn upload_id(&self) -> String {
        let mut client = self.connect().await;
        let answer = client.request("POST /upload/session", "").await;
        let session: serde_json::Value = serde_json::from_slice(&answer.body).unwrap();
        session["uploadId"].as_str().unwrap().into()
    }
}

/// An answer's status line, header fields with lower-case names, and body.
#[derive(Debug)]
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.as_str())
    }
}

/// An HTTP/1.1 client over a raw stream.
struct Client<S> {
    stream: BufReader<S>,
    /// The next answer has no body, as the answer to HEAD.
    bodiless: bool,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Client<S> {
    fn new(stream: S) -> Self {
        Self { stream: BufReader::new(stream), bodiless: false }
    }

    async fn send(&mut self, bytes: &str) {
        self.bodiless = bytes.starts_with("HEAD ");
        self.stream.get_mut().write_all(bytes.as_bytes()).await.unwrap();
    }

    /// `line` (method and target) with `body`, on this connection.
    async fn request(&mut self, line: &str, body: &str) -> Answer {
        let length = body.len();
        self.send(&format!("{line} HTTP/1.1\r\nHost: test\r\nContent-Length: {length}\r\n\r\n{body}"))
            .await;
        self.answer().await.unwrap()
    }

    /// The next answer head; `None` when the connection ends first.
    async fn head(&mut self) -> Option<Answer> {
        let mut line = String::new();
        if self.stream.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let status = line.split(' ').nth(1)?.parse().ok()?;
        let mut headers = Vec::new();
        loop {
            line.clear();
            self.stream.read_line(&mut line).await.ok()?;
            match line.trim_end().split_once(": ") {
                Some((name, value)) => headers.push((name.to_ascii_lowercase(), value.into())),
                None => break,
            }
        }
        Some(Answer { status, headers, body: Vec::new() })
    }

    /// The next whole answer; `None` when the connection ends first.
    async fn answer(&mut self) -> Option<Answer> {
        let mut answer = self.head().await?;
        if self.bodiless || matches!(answer.status, 101 | 204) {
            return Some(answer);
        }
        if let Some(length) = answer.header("content-length") {
            answer.body = vec![0; length.parse().unwrap()];
            self.stream.read_exact(&mut answer.body).await.ok()?;
        } else if answer.header("transfer-encoding") == Some("chunked") {
            while let Some(chunk) = self.chunk().await {
                answer.body.extend(chunk);
            }
        } else {
            self.stream.read_to_end(&mut answer.body).await.ok()?;
        }
        Some(answer)
    }

    /// The next chunk of a chunked body; `None` at its end.
    async fn chunk(&mut self) -> Option<Vec<u8>> {
        let mut line = String::new();
        self.stream.read_line(&mut line).await.ok()?;
        let length = usize::from_str_radix(line.trim_end(), 16).ok()?;
        let mut chunk = vec![0; length + 2];
        self.stream.read_exact(&mut chunk).await.ok()?;
        chunk.truncate(length);
        (length > 0).then_some(chunk)
    }

    /// Reads until the connection ends, returning how many bytes came.
    async fn drain(&mut self) -> usize {
        let mut buffer = vec![0; 64 << 10];
        let mut total = 0;
        while let Ok(read @ 1..) = self.stream.read(&mut buffer).await {
            total += read;
        }
        total
    }
}
