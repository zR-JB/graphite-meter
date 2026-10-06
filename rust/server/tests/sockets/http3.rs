//! The HTTP/3 listener: transfers and their endings, exchange bounds beside admitted work, the connection lifecycle and
//! the TCP companion's bootstrap probe.

use super::*;
use bytes::Bytes;
use graphite_meter_http3::{Code, Error, RecvHalf, SendHalf, client};
use graphite_meter_testkit::{Identity, Scratch};
use http::{Request, Response};
use rustls::pki_types::ServerName;
use std::sync::Arc;
use tokio_rustls::TlsConnector;

/// The HTTP/3 listener's address, which must differ from the HTTP/1 listener's.
pub(super) const ADDRESS: &str = "127.0.0.7:0";
/// The client's keep-alive, which keeps a connection from idling out while tests move the clock in steps of `STEP`.
const KEEP_ALIVE: Duration = Duration::from_secs(2);
const STEP: Duration = Duration::from_secs(5);

/// A server with an HTTP/3 listener on a local port and a client identity trusting its certificate.
pub(super) struct H3 {
    pub(super) server: Running,
    pub(super) identity: Identity,
    _scratch: Scratch,
}

/// A client connection, its request sender and the task driving it, which ends when the connection closes.
pub(super) struct Connection {
    pub(super) quic: noq::Connection,
    pub(super) requests: client::SendRequest,
    _driver: JoinHandle<Result<(), Error>>,
    pub(super) endpoint: noq::Endpoint,
}

impl H3 {
    pub(super) async fn start(env: &[(&str, &str)]) -> Self {
        let (scratch, identity) = (Scratch::new().unwrap(), Identity::generate().unwrap());
        let cert = scratch.file("cert.pem", &identity.certificate).unwrap();
        let key = scratch.file("key.pem", &identity.key).unwrap();
        let (cert, key) = (cert.to_str().unwrap(), key.to_str().unwrap());
        let mut env = env.to_vec();
        env.extend([("GM_TLS_CERT", cert), ("GM_TLS_KEY", key), ("GM_H3_ADDR", ADDRESS)]);
        Self { server: start(&env).await, identity, _scratch: scratch }
    }

    /// A client configuration trusting the server, with `transport`.
    pub(super) fn client(&self, transport: noq::TransportConfig) -> noq::ClientConfig {
        let mut config = self.identity.quic_client();
        config.transport_config(Arc::new(transport));
        config
    }

    pub(super) async fn connect(&self, transport: noq::TransportConfig) -> Connection {
        self.connect_from("127.0.0.1", transport).await.unwrap()
    }

    /// A connection from the local address `source`.
    pub(super) async fn connect_from(
        &self,
        source: &str,
        transport: noq::TransportConfig,
    ) -> Result<Connection, noq::ConnectionError> {
        self.dial(source, self.server.quic.unwrap(), transport).await
    }

    /// A connection through `relay`, an address that forwards to the listener.
    pub(super) async fn connect_via(&self, relay: SocketAddr, transport: noq::TransportConfig) -> Connection {
        self.dial("127.0.0.1", relay, transport).await.unwrap()
    }

    async fn dial(
        &self,
        source: &str,
        target: SocketAddr,
        transport: noq::TransportConfig,
    ) -> Result<Connection, noq::ConnectionError> {
        let endpoint = noq::Endpoint::client(format!("{source}:0").parse().unwrap()).unwrap();
        let connecting = endpoint.connect_with(self.client(transport), target, "localhost");
        let quic = connecting.unwrap().await?;
        let (mut driver, requests) = client::new(quic.clone());
        let driver = tokio::spawn(async move { driver.drive().await });
        Ok(Connection { quic, requests, _driver: driver, endpoint })
    }
}

/// A transport with keep-alives, and with a stream receive window of `window` bytes when given.
pub(super) fn transport(window: Option<u32>) -> noq::TransportConfig {
    let mut transport = noq::TransportConfig::default();
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    if let Some(window) = window {
        transport.stream_receive_window(window.into());
    }
    transport
}

impl Connection {
    /// Opens a request whose body the caller sends.
    pub(super) async fn open(&self, method: &str, path: &str) -> (SendHalf, RecvHalf) {
        let request = Request::builder()
            .method(method)
            .uri(format!("https://localhost{path}"))
            .body(())
            .unwrap();
        self.requests.send_request(request).await.unwrap().split()
    }

    /// Sends a request with `body` and reads its answer's head.
    pub(super) async fn send(&self, method: &str, path: &str, body: &'static [u8]) -> (Response<()>, RecvHalf) {
        let (mut send, mut recv) = self.open(method, path).await;
        if !body.is_empty() {
            send.send_data(Bytes::from_static(body)).await.unwrap();
        }
        send.finish().await.unwrap();
        (recv.response().await.unwrap(), recv)
    }

    pub(super) async fn json(&self, method: &str, path: &str) -> serde_json::Value {
        let mut body = self.send(method, path, b"").await.1;
        serde_json::from_slice(&read(&mut body).await.unwrap()).unwrap()
    }

    pub(super) async fn upload_id(&self) -> String {
        self.json("POST", "/upload/session").await["uploadId"]
            .as_str()
            .unwrap()
            .into()
    }

    /// The code the server closed the connection with, if it did within `bound` of real time.
    pub(super) async fn closed_within(&self, bound: Duration) -> Option<Code> {
        match tokio::time::timeout(bound, self.quic.closed()).await {
            Ok(noq::ConnectionError::ApplicationClosed(close)) => Some(Code(close.error_code.into_inner())),
            Ok(error) => panic!("closed without an application code: {error:?}"),
            Err(_) => None,
        }
    }

    fn resets(&self) -> u64 {
        self.quic.stats().frame_rx.reset_stream
    }
}

/// A whole body; a reset ends it with its error.
pub(super) async fn read(body: &mut RecvHalf) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body.data().await? {
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// A whole reply's body; a reset ends it with its error.
async fn reply(recv: &mut RecvHalf) -> Result<Vec<u8>, Error> {
    recv.response().await?;
    read(recv).await
}

/// Moves the clock on by `duration` in steps the keep-alives span, so no connection idles out.
pub(super) async fn pass(mut duration: Duration) {
    while !duration.is_zero() {
        let step = duration.min(STEP);
        advance_clock(step).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        duration -= step;
    }
}

const CANCELLED: Error = Error::Reset(Code::H3_REQUEST_CANCELLED);

#[tokio::test]
async fn http3_serves_its_routes_downloads_and_uploads() {
    let h3 = H3::start(&[]).await;
    let connection = h3.connect(transport(None)).await;
    let (probe, mut body) = connection.send("GET", "/probe", b"").await;
    assert_eq!(probe.headers()["access-control-allow-origin"], "*");
    assert!(!probe.headers().contains_key("alt-svc") && !probe.headers().contains_key("connection"));
    let probe: serde_json::Value = serde_json::from_slice(&read(&mut body).await.unwrap()).unwrap();
    assert_eq!(probe["protocolNegotiated"], "h3");
    assert_eq!(connection.send("GET", "/preflight", b"").await.0.status(), 404, "no UI route");
    let mut download = connection.send("GET", "/download?bytes=1000000", b"").await.1;
    assert_eq!(read(&mut download).await.unwrap().len(), 1_000_000);
    let (head, mut body) = connection.send("HEAD", "/download?bytes=5", b"").await;
    assert_eq!(head.headers()["content-length"], "5", "HEAD keeps its length");
    assert!(read(&mut body).await.unwrap().is_empty());
    let id = connection.upload_id().await;
    let mut answer = connection
        .send("POST", &format!("/upload?id={id}"), &[7; 300_000])
        .await
        .1;
    assert_eq!(read(&mut answer).await.unwrap(), br#"{"bytes":300000}"#);
    assert_eq!(h3.server.active().await, 0, "finished transfers release their handlers");
}

#[tokio::test]
async fn the_companion_answers_bootstrap_probes_with_alt_svc_and_serves_no_transfer() {
    let h3 = H3::start(&[]).await;
    let (companion, port) = (h3.server.companion.unwrap(), h3.server.quic.unwrap().port());
    assert_eq!(companion.port(), port, "the companion shares the HTTP/3 port");
    let connector = TlsConnector::from(Arc::new(h3.identity.client(&[b"http/1.1"])));
    let connect = async || {
        let socket = TcpStream::connect(companion).await.unwrap();
        let name = ServerName::try_from("localhost").unwrap();
        Client::new(connector.connect(name, socket).await.unwrap())
    };
    let probe = connect().await.request("GET /probe", "").await;
    assert_eq!(probe.status, 200);
    assert_eq!(probe.header("alt-svc"), Some(format!("h3=\":{port}\"").as_str()));
    assert_eq!(probe.header("connection"), Some("close"));
    assert_eq!(connect().await.request("GET /download?bytes=1", "").await.status, 404);
}

#[tokio::test]
async fn an_unadmitted_request_is_reset_at_fifteen_seconds_beside_an_admitted_download() {
    let h3 = H3::start(&[]).await;
    // Too little stream credit for any reply's head: both replies stall.
    let connection = h3.connect(transport(Some(64))).await;
    let (_download, mut download) = connection.open("GET", ENDLESS).await;
    h3.server.until_active(1).await;
    let frames = connection.quic.stats().frame_rx.stream;
    let (_probe, mut probe) = connection.open("GET", "/probe").await;
    // The reply's first bytes show its exchange began before the clock moves.
    while connection.quic.stats().frame_rx.stream == frames {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    pass(Duration::from_secs(14)).await;
    assert_eq!(connection.resets(), 0, "the exchange runs until its bound");
    pass(Duration::from_secs(1)).await;
    assert_eq!(reply(&mut probe).await, Err(CANCELLED));
    assert_eq!(h3.server.active().await, 1, "the admitted download runs on");
    pass(Duration::from_secs(15)).await;
    assert_eq!(reply(&mut download).await, Err(CANCELLED), "a stalled reply ends at the idle bound");
    assert_eq!(h3.server.active().await, 0);
}

#[tokio::test]
async fn an_upload_and_a_feed_the_peer_leaves_idle_end_after_thirty_seconds() {
    let h3 = H3::start(&[]).await;
    let connection = h3.connect(transport(None)).await;
    let id = connection.upload_id().await;
    let (mut upload, mut answer) = connection.open("POST", &format!("/upload?id={id}")).await;
    upload.send_data(Bytes::from_static(b"partial")).await.unwrap();
    // Too little stream credit for the feed's head.
    let stalled = h3.connect(transport(Some(64))).await;
    let _feed = stalled.open("GET", &format!("/upload/progress?id={id}")).await;
    h3.server.until_active(2).await;
    pass(Duration::from_secs(28)).await;
    assert_eq!((h3.server.active().await, stalled.resets()), (2, 0));
    pass(Duration::from_secs(3)).await;
    let answer = answer.response().await.unwrap();
    assert_eq!(answer.status(), 408);
    assert_eq!(answer.headers()["x-graphite-upload-refusal"], "idle");
    h3.server.until_active(0).await;
    assert_eq!(stalled.resets(), 1, "the feed's stream is reset");
}

#[tokio::test]
async fn cancelling_an_idle_progress_stream_releases_its_handler() {
    let h3 = H3::start(&[]).await;
    let connection = h3.connect(transport(None)).await;
    let id = connection.upload_id().await;
    let (progress, mut feed) = connection.send("GET", &format!("/upload/progress?id={id}"), b"").await;
    assert_eq!(progress.status(), 200);
    feed.data().await.unwrap().expect("the ready record");
    h3.server.until_active(1).await;
    drop(feed);
    let released = tokio::time::timeout(Duration::from_millis(500), h3.server.until_active(0)).await;
    assert!(released.is_ok(), "released before the feed's next heartbeat");
}

#[tokio::test]
async fn shutdown_resets_transfers_and_gives_the_rest_five_seconds_after_goaway() {
    let h3 = H3::start(&[]).await;
    let connection = h3.connect(transport(Some(64))).await;
    let id = connection.upload_id().await;
    let (_download, mut download) = connection.open("GET", ENDLESS).await;
    let (mut upload, mut answer) = connection.open("POST", &format!("/upload?id={id}")).await;
    upload.send_data(Bytes::from_static(b"partial")).await.unwrap();
    let _unadmitted = connection.open("GET", "/probe").await;
    h3.server.until_active(2).await;
    let stopped = h3.server.stop();
    assert_eq!(reply(&mut download).await, Err(CANCELLED));
    assert_eq!(answer.response().await.unwrap_err(), CANCELLED);
    while !connection.requests.going_away() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let late = connection
        .requests
        .send_request(Request::get("https://localhost/probe").body(()).unwrap());
    assert_eq!(late.await.err(), Some(Error::GoingAway), "a request after the GOAWAY is refused");
    pass(Duration::from_millis(4500)).await;
    assert_eq!(
        connection.closed_within(Duration::from_millis(50)).await,
        None,
        "the unadmitted request gets its grace"
    );
    pass(Duration::from_secs(1)).await;
    assert_eq!(connection.closed_within(Duration::from_secs(2)).await, Some(Code::H3_NO_ERROR));
    stopped.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_funded_connection_goes_away_fifteen_seconds_after_its_admitted_work() {
    let h3 = H3::start(&[]).await;
    let (funded, control) = (h3.connect(transport(None)).await, h3.connect(transport(None)).await);
    let id = funded.upload_id().await;
    let mut answer = funded.send("POST", &format!("/upload?id={id}"), b"abc").await.1;
    assert_eq!(read(&mut answer).await.unwrap(), br#"{"bytes":3}"#);
    for _ in 0..2 {
        pass(Duration::from_secs(7)).await;
        for connection in [&funded, &control] {
            assert_eq!(connection.send("GET", "/probe", b"").await.0.status(), 200);
        }
    }
    assert_eq!(funded.closed_within(Duration::from_millis(50)).await, None);
    pass(Duration::from_millis(1500)).await;
    let closed = funded.closed_within(Duration::from_secs(2)).await;
    assert_eq!(closed, Some(Code::H3_NO_ERROR), "unadmitted requests never extend its idle period");
    assert_eq!(
        control.closed_within(Duration::from_millis(50)).await,
        None,
        "a connection without credit stays"
    );
}

#[tokio::test]
async fn connection_credit_reaches_the_reply_that_waited_longest() {
    let h3 = H3::start(&[]).await;
    let mut transport = transport(None);
    // Each grant of connection credit is smaller than what one reply queues at a time.
    transport.receive_window((64_u32 << 10).into());
    let connection = h3.connect(transport).await;
    let open = async || {
        let (mut send, recv) = connection.open("GET", ENDLESS).await;
        send.finish().await.unwrap();
        (send, recv)
    };
    // The first reply fills the window, then two more wait for credit in that order; reading the first frees credit
    // a little at a time, and the second's head must not lose every grant to the third.
    let (_bulk, mut bulk) = open().await;
    bulk.response().await.unwrap();
    let ((_other, mut other), _later) = (open().await, open().await);
    let reading = async {
        loop {
            tokio::time::sleep(Duration::from_millis(5)).await;
            bulk.data().await.unwrap();
        }
    };
    let reply = async {
        tokio::select! {
            reply = other.response() => reply,
            () = reading => unreachable!(),
        }
    };
    let reply = tokio::time::timeout(Duration::from_secs(5), reply).await;
    assert_eq!(reply.expect("the waiting reply got credit").unwrap().status(), 200);
}
