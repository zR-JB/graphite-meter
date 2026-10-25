//! The HTTP/3 listener: transfers, endings, exchange bounds, connection lifecycle and the TCP bootstrap probe.

use super::*;
use bytes::Bytes;
use graphite_meter_http3::{Code, Error, RecvHalf, SendHalf, client};
use graphite_meter_testkit::{Identity, Scratch};
use http::{Request, Response};
use rustls::pki_types::ServerName;
use std::sync::Arc;
use tokio_rustls::TlsConnector;

/// The HTTP/3 listener's address, apart from the HTTP/1 listener's.
pub(super) const ADDRESS: &str = "127.0.0.7:0";
/// The client's keep-alive, which keeps a connection from idling out while tests move the clock in steps of `STEP`.
const KEEP_ALIVE: Duration = Duration::from_secs(2);
const STEP: Duration = Duration::from_secs(5);

pub(super) struct H3 {
    pub(super) server: Running,
    pub(super) identity: Identity,
    _scratch: Scratch,
}

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
        env.extend([
            ("GM_TLS_CERT", cert),
            ("GM_TLS_KEY", key),
            ("GM_H3_ADDR", ADDRESS),
        ]);
        Self {
            server: start(&env).await,
            identity,
            _scratch: scratch,
        }
    }

    pub(super) fn client(&self, transport: noq::TransportConfig) -> noq::ClientConfig {
        let mut config = self.identity.quic_client();
        config.transport_config(Arc::new(transport));
        config
    }

    pub(super) async fn connect(&self, transport: noq::TransportConfig) -> Connection {
        self.connect_from("127.0.0.1", transport).await.unwrap()
    }

    pub(super) async fn connect_from(
        &self,
        source: &str,
        transport: noq::TransportConfig,
    ) -> Result<Connection, noq::ConnectionError> {
        self.dial(source, self.server.quic.unwrap(), transport)
            .await
    }

    /// A connection through `relay`, an address that forwards to the listener.
    pub(super) async fn connect_via(
        &self,
        relay: SocketAddr,
        transport: noq::TransportConfig,
    ) -> Connection {
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
        Ok(Connection {
            quic,
            requests,
            _driver: driver,
            endpoint,
        })
    }
}

pub(super) fn transport(window: Option<u32>) -> noq::TransportConfig {
    let mut transport = noq::TransportConfig::default();
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    if let Some(window) = window {
        transport.stream_receive_window(window.into());
    }
    transport
}

impl Connection {
    pub(super) async fn open(&self, method: &str, path: &str) -> (SendHalf, RecvHalf) {
        let request = Request::builder()
            .method(method)
            .uri(format!("https://localhost{path}"))
            .body(())
            .unwrap();
        self.requests.send_request(request).await.unwrap().split()
    }

    pub(super) async fn send(
        &self,
        method: &str,
        path: &str,
        body: &'static [u8],
    ) -> (Response<()>, RecvHalf) {
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

    /// The code the server closed the connection with, if it did within `bound` of real time.
    pub(super) async fn closed_within(&self, bound: Duration) -> Option<Code> {
        match tokio::time::timeout(bound, self.quic.closed()).await {
            Ok(noq::ConnectionError::ApplicationClosed(close)) => Some(close.error_code.into()),
            Ok(error) => panic!("closed without an application code: {error:?}"),
            Err(_) => None,
        }
    }

    fn resets(&self) -> u64 {
        self.quic.stats().frame_rx.reset_stream
    }
}

pub(super) async fn read(body: &mut RecvHalf) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body.data().await? {
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

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
async fn the_companion_points_at_the_public_port_of_a_remapped_listener() {
    let h3 = H3::start(&[("GM_H3_PUBLIC_ORIGIN", "https://localhost:17249")]).await;
    let connector = TlsConnector::from(Arc::new(h3.identity.client(&[b"http/1.1"])));
    let socket = TcpStream::connect(h3.server.companion.unwrap())
        .await
        .unwrap();
    let name = ServerName::try_from("localhost").unwrap();
    let tls = connector.connect(name, socket).await.unwrap();
    let probe = Client::new(tls).request("GET /probe", "").await;
    assert_eq!(probe.header("alt-svc"), Some("h3=\":17249\""));
}

#[tokio::test]
async fn the_companion_points_every_answer_at_http3_and_refuses_transfers_readably() {
    let h3 = H3::start(&[]).await;
    let (companion, port) = (h3.server.companion.unwrap(), h3.server.quic.unwrap().port());
    assert_eq!(
        companion.port(),
        port,
        "the companion shares the HTTP/3 port"
    );
    let connector = TlsConnector::from(Arc::new(h3.identity.client(&[b"http/1.1"])));
    let connect = async || {
        let socket = TcpStream::connect(companion).await.unwrap();
        let name = ServerName::try_from("localhost").unwrap();
        Client::new(connector.connect(name, socket).await.unwrap())
    };
    let probe = connect().await.request("GET /probe", "").await;
    assert_eq!(probe.status, 200);
    assert_eq!(
        probe.header("alt-svc"),
        Some(format!("h3=\":{port}\"").as_str())
    );
    assert_eq!(probe.header("connection"), Some("close"));
    let preflight = connect().await.request("OPTIONS /upload", "").await;
    assert_eq!(preflight.status, 204);
    for line in ["GET /download?bytes=1", "POST /upload?id=gmu_x"] {
        let refused = connect().await.request(line, "").await;
        assert_eq!(
            (refused.status, refused.header("graphite-meter-refusal")),
            (409, Some("wrong-protocol")),
            "{line}"
        );
        assert_eq!(refused.header("alt-svc"), probe.header("alt-svc"));
        assert_eq!(refused.header("access-control-allow-origin"), Some("*"));
        let exposed = refused.header("access-control-expose-headers");
        assert!(exposed.is_some_and(|names| names.contains("Graphite-Meter-Refusal")));
        assert_eq!(
            refused.body,
            b"this port serves transfers over HTTP/3, not TCP\n"
        );
    }
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
    let began = async {
        while connection.quic.stats().frame_rx.stream == frames {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), began)
        .await
        .expect("the reply begins");
    pass(Duration::from_secs(14)).await;
    assert_eq!(connection.resets(), 0, "the exchange runs until its bound");
    pass(Duration::from_secs(1)).await;
    assert_eq!(reply(&mut probe).await, Err(CANCELLED));
    assert_eq!(h3.server.active().await, 1, "the admitted download runs on");
    pass(Duration::from_secs(15)).await;
    assert_eq!(
        reply(&mut download).await,
        Err(CANCELLED),
        "a stalled reply ends at the idle bound"
    );
    assert_eq!(h3.server.active().await, 0);
}

#[tokio::test]
async fn cancelling_an_idle_progress_stream_releases_its_handler() {
    let h3 = H3::start(&[]).await;
    let connection = h3.connect(transport(None)).await;
    let id = h3.server.upload_id().await;
    let (progress, mut feed) = connection
        .send("GET", &format!("/upload/progress?id={id}"), b"")
        .await;
    assert_eq!(progress.status(), 200);
    feed.data().await.unwrap().expect("the ready record");
    h3.server.until_active(1).await;
    drop(feed);
    let released =
        tokio::time::timeout(Duration::from_millis(500), h3.server.until_active(0)).await;
    assert!(
        released.is_ok(),
        "released before the feed's next heartbeat"
    );
}

#[tokio::test]
async fn a_reply_stopped_while_the_connection_has_no_credit_is_reset() {
    let h3 = H3::start(&[]).await;
    let mut transport = transport(None);
    transport.receive_window((64_u32 << 10).into());
    let connection = h3.connect(transport).await;
    let open = async || {
        let (mut send, recv) = connection.open("GET", ENDLESS).await;
        send.finish().await.unwrap();
        recv
    };
    let mut credit_holder = open().await;
    credit_holder.response().await.unwrap();
    let mut stopped = open().await;
    h3.server.until_active(2).await;
    stopped.stop(Code::H3_REQUEST_CANCELLED);
    let reset = async {
        while connection.resets() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(2), reset)
        .await
        .expect("the stopped reply is reset");
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
    // The first reply fills the window and two more wait for credit; the second's head must not lose every grant to the third.
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
    assert_eq!(
        reply
            .expect("the waiting reply got credit")
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test]
async fn a_download_ends_on_the_server_s_clock_without_a_length() {
    let h3 = H3::start(&[]).await;
    let connection = h3.connect(transport(None)).await;
    let started = std::time::Instant::now();
    let (answer, mut body) = connection
        .send("GET", &format!("{ENDLESS}&ms=50"), b"")
        .await;
    let read = read(&mut body).await.unwrap().len();
    let elapsed = started.elapsed();
    assert_eq!(
        (answer.status(), answer.headers().get("content-length")),
        (http::StatusCode::OK, None)
    );
    assert!(
        read > 0 && elapsed >= Duration::from_millis(50) && elapsed < Duration::from_secs(2),
        "{read} bytes in {elapsed:?}"
    );
    let (probe, _) = connection.send("GET", "/probe", b"").await;
    assert_eq!(
        probe.status(),
        200,
        "the connection serves the next request"
    );
}

#[tokio::test]
async fn a_stage_s_last_download_retires_its_connection_without_a_close() {
    let h3 = H3::start(&[]).await;
    let retired = async |connection: &Connection| {
        while !connection.requests.going_away() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    let bound = Duration::from_secs(2);
    let ended = h3.connect(transport(None)).await;
    let (_, mut body) = ended
        .send("GET", &format!("{ENDLESS}&ms=50&last="), b"")
        .await;
    read(&mut body).await.unwrap();
    tokio::time::timeout(bound, retired(&ended))
        .await
        .expect("GOAWAY follows the body's end");
    let refused = ended.requests.send_request(
        http::Request::get("https://localhost/probe")
            .body(())
            .unwrap(),
    );
    assert!(
        matches!(refused.await, Err(Error::GoingAway)),
        "a later request takes a new connection"
    );
    assert_eq!(
        ended.closed_within(Duration::from_millis(300)).await,
        None,
        "nothing closes it out loud"
    );
    // A body the client cuts, as an early finish does, retires it too; what the client already sent is still served.
    let cut = h3.connect(transport(None)).await;
    let (_, body) = cut
        .send("GET", &format!("{ENDLESS}&ms=60000&last="), b"")
        .await;
    let (mut sending, mut sent) = cut.open("GET", "/probe").await;
    drop(body);
    tokio::time::timeout(bound, retired(&cut))
        .await
        .expect("GOAWAY follows the cut");
    sending.finish().await.unwrap();
    assert_eq!(sent.response().await.unwrap().status(), 200);
    let stopped = h3.server.stop();
    assert_eq!(
        cut.closed_within(bound).await,
        Some(Code::H3_NO_ERROR),
        "a stop still closes it"
    );
    stopped.await.unwrap().unwrap();
}
