//! The HTTP/2 listener: transfers and their idle endings, window funding, header floods and its SETTINGS.

use super::*;
use bytes::Bytes;
use graphite_meter_testkit::{Identity, Scratch};
use h2::{
    Reason, RecvStream, SendStream,
    client::{self, SendRequest},
};
use http::{Request, Response};
use rustls::pki_types::ServerName;
use std::{future::poll_fn, sync::Arc};
use tokio_rustls::TlsConnector;

/// A server with an HTTP/2 listener on a local port and a client trusting its certificate.
struct H2 {
    server: Running,
    connector: TlsConnector,
    _scratch: Scratch,
}

/// A client connection and the task driving it, which ends when the connection closes.
struct Connection {
    client: SendRequest<Bytes>,
    _driver: JoinHandle<Result<(), h2::Error>>,
}

impl H2 {
    async fn start(env: &[(&str, &str)]) -> Self {
        let (scratch, identity) = (Scratch::new().unwrap(), Identity::generate().unwrap());
        let cert = scratch.file("cert.pem", &identity.certificate).unwrap();
        let key = scratch.file("key.pem", &identity.key).unwrap();
        let (cert, key) = (cert.to_str().unwrap(), key.to_str().unwrap());
        let mut env = env.to_vec();
        env.extend([("GM_TLS_CERT", cert), ("GM_TLS_KEY", key), ("GM_H2_ADDR", "localhost:0")]);
        let server = start(&env).await;
        let connector = TlsConnector::from(Arc::new(identity.client(&[b"h2"])));
        Self { server, connector, _scratch: scratch }
    }

    /// A connection whose streams open with `window` bytes of receive window.
    async fn connect(&self, window: u32) -> Connection {
        let socket = TcpStream::connect(self.server.h2.unwrap()).await.unwrap();
        let name = ServerName::try_from("localhost").unwrap();
        let stream = self.connector.connect(name, socket).await.unwrap();
        assert_eq!(stream.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
        let (client, connection) = client::Builder::new()
            .initial_window_size(window)
            .max_send_buffer_size(16 << 20)
            .handshake(stream)
            .await
            .unwrap();
        Connection { client, _driver: tokio::spawn(connection) }
    }
}

impl Connection {
    /// Opens a request whose body the caller sends.
    async fn open(&mut self, method: &str, path: &str) -> (client::ResponseFuture, SendStream<Bytes>) {
        self.send(method, path, false).await
    }

    async fn get(&mut self, method: &str, path: &str) -> Response<RecvStream> {
        self.send(method, path, true).await.0.await.unwrap()
    }

    async fn send(&mut self, method: &str, path: &str, end: bool) -> (client::ResponseFuture, SendStream<Bytes>) {
        poll_fn(|cx| self.client.poll_ready(cx)).await.unwrap();
        let request = Request::builder()
            .method(method)
            .uri(format!("https://localhost{path}"))
            .body(())
            .unwrap();
        self.client.send_request(request, end).unwrap()
    }

    async fn json(&mut self, method: &str, path: &str) -> serde_json::Value {
        serde_json::from_slice(&read(self.get(method, path).await.into_body()).await.unwrap()).unwrap()
    }

    async fn upload_id(&mut self) -> String {
        self.json("POST", "/upload/session").await["uploadId"]
            .as_str()
            .unwrap()
            .into()
    }
}

/// A whole body, releasing its window as it arrives; a reset ends it with the reason.
async fn read(mut body: RecvStream) -> Result<Vec<u8>, Option<Reason>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk.map_err(|error| error.reason())?;
        body.flow_control().release_capacity(chunk.len()).unwrap();
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Sends `bytes` as flow control admits them.
async fn send(stream: &mut SendStream<Bytes>, mut bytes: Bytes, end: bool) {
    while !bytes.is_empty() {
        stream.reserve_capacity(bytes.len());
        while stream.capacity() == 0 {
            poll_fn(|cx| stream.poll_capacity(cx)).await.unwrap().unwrap();
        }
        let chunk = bytes.split_to(stream.capacity().min(bytes.len()));
        stream.send_data(chunk, end && bytes.is_empty()).unwrap();
    }
}

#[tokio::test]
async fn http2_serves_its_routes_downloads_and_uploads() {
    let h2 = H2::start(&[]).await;
    let mut connection = h2.connect(65_535).await;
    let probe = connection.get("GET", "/probe").await;
    assert_eq!(probe.headers()["access-control-allow-origin"], "*");
    assert!(!probe.headers().contains_key("connection"));
    let probe: serde_json::Value = serde_json::from_slice(&read(probe.into_body()).await.unwrap()).unwrap();
    assert_eq!(probe["protocolNegotiated"], "h2");
    assert_eq!(connection.get("GET", "/preflight").await.status(), 404, "no UI route");
    let download = connection.get("GET", "/download?bytes=1000000").await;
    assert_eq!(read(download.into_body()).await.unwrap().len(), 1_000_000);
    let id = connection.upload_id().await;
    let (answer, mut upload) = connection.open("POST", &format!("/upload?id={id}")).await;
    send(&mut upload, Bytes::from(vec![7; 300_000]), true).await;
    let answer = read(answer.await.unwrap().into_body()).await.unwrap();
    assert_eq!(answer, br#"{"bytes":300000}"#);
    assert_eq!(h2.server.active().await, 0, "finished transfers release their handlers");
}

/// Whether an upload that sent one byte may send twice the default window within `bound`; h2's send buffer caps it
/// below a mebibyte.
async fn window_opens(upload: &mut SendStream<Bytes>, bound: Duration) -> bool {
    upload.send_data(Bytes::from_static(b"x"), false).unwrap();
    upload.reserve_capacity(4 << 20);
    let opened = async {
        while upload.capacity() < 2 * 65_535 {
            poll_fn(|cx| upload.poll_capacity(cx)).await.unwrap().unwrap();
        }
    };
    tokio::time::timeout(bound, opened).await.is_ok()
}

/// Waits until the upload `id` counted `bytes`.
async fn counted(connection: &mut Connection, id: &str, bytes: u64) {
    let reached = async {
        while connection.json("POST", &format!("/upload/checkpoint?id={id}")).await["bytes"] != bytes {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), reached).await.unwrap();
}

#[tokio::test]
async fn under_pressure_a_running_upload_keeps_its_window_after_the_one_that_raised_it_ends() {
    let h2 = H2::start(&[]).await;
    let budget = h2.server.budget.clone();
    let idle = super::quic::settled(&budget).await;
    let mut connection = h2.connect(65_535).await;
    let id = connection.upload_id().await;
    let (raised, mut raiser) = connection.open("POST", &format!("/upload?id={id}")).await;
    assert!(window_opens(&mut raiser, Duration::from_secs(5)).await);
    let pressure = budget.lease(budget.usage().limit / 4 * 3).unwrap();
    let (running, mut upload) = connection.open("POST", &format!("/upload?id={id}")).await;
    upload.send_data(Bytes::from_static(b"x"), false).unwrap();
    counted(&mut connection, &id, 2).await;
    raiser.send_data(Bytes::new(), true).unwrap();
    assert_eq!(read(raised.await.unwrap().into_body()).await.unwrap(), br#"{"bytes":1}"#);
    // More than the credit the window granted before, so the server must keep granting it.
    let sent = 26 << 20;
    send(&mut upload, Bytes::from(vec![7; sent]), false).await;
    counted(&mut connection, &id, 2 + sent as u64).await;
    assert!(
        window_opens(&mut upload, Duration::from_secs(2)).await,
        "the window it read at stays open while it runs"
    );
    upload.send_data(Bytes::new(), true).unwrap();
    assert_eq!(running.await.unwrap().status(), 200);
    drop((pressure, connection, upload, raiser));
    let drained = async {
        while budget.usage().used != idle {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), drained)
        .await
        .expect("the budget drains to its baseline");
}

#[tokio::test]
async fn an_upload_keeps_a_large_window_nearly_full() {
    let h2 = H2::start(&[]).await;
    let mut connection = h2.connect(65_535).await;
    let id = connection.upload_id().await;
    let (answer, mut upload) = connection.open("POST", &format!("/upload?id={id}")).await;
    assert!(window_opens(&mut upload, Duration::from_secs(5)).await);
    let sent = 15 << 19;
    send(&mut upload, Bytes::from(vec![7; sent]), false).await;
    counted(&mut connection, &id, 1 + sent as u64).await;
    // Refreshed once a mebibyte of its 8 MiB is unclaimed, where h2 alone waits for a third of it.
    upload.reserve_capacity(8 << 20);
    let refreshed = async {
        while upload.capacity() <= 7 << 20 {
            poll_fn(|cx| upload.poll_capacity(cx)).await.unwrap().unwrap();
        }
    };
    let refreshed = tokio::time::timeout(Duration::from_secs(2), refreshed).await;
    assert!(refreshed.is_ok(), "{} bytes of window", upload.capacity());
    upload.send_data(Bytes::new(), true).unwrap();
    assert_eq!(answer.await.unwrap().status(), 200);
}

#[tokio::test]
async fn unadmitted_header_and_data_floods_stay_within_the_floor() {
    let h2 = H2::start(&[]).await;
    let budget = h2.server.budget.clone();
    let idle = super::quic::settled(&budget).await;
    let mut flood = h2.connect(0).await;
    let pad = http::HeaderValue::from_bytes(&[b'p'; 24 << 10]).unwrap();
    let mut held = Vec::new();
    for _ in 0..64 {
        poll_fn(|cx| flood.client.poll_ready(cx)).await.unwrap();
        let mut head = Request::post("https://localhost/probe").body(()).unwrap();
        head.headers_mut().insert("x-pad", pad.clone());
        held.push(flood.client.send_request(head, false).unwrap());
    }
    for (_, upload) in &mut held {
        for _ in 0..64 {
            upload.reserve_capacity(1);
            if upload.capacity() > 0 {
                let _ = upload.send_data(Bytes::from_static(b"x"), false);
            }
        }
    }
    let mut refused = 0;
    for (response, _) in held {
        let response = tokio::time::timeout(Duration::from_secs(5), response).await.unwrap();
        refused += usize::from(response.is_err_and(|error| error.reason() == Some(Reason::REFUSED_STREAM)));
        let used = budget.usage().used.saturating_sub(idle);
        assert!(used <= graphite_meter_server::transport::http2::FLOOR_BYTES, "{used} bytes");
    }
    assert!(refused > 0, "the flood reached the connection's allowance");
    let mut sibling = h2.connect(65_535).await;
    assert_eq!(sibling.json("GET", "/probe").await["protocolNegotiated"], "h2");
}

#[tokio::test]
async fn transfers_and_feeds_the_peer_leaves_idle_end_after_thirty_seconds() {
    let h2 = H2::start(&[]).await;
    // Without stream credit the download and the feed write nothing past their heads.
    let mut connection = h2.connect(0).await;
    let download = connection.get("GET", ENDLESS).await;
    let id = h2.server.upload_id().await;
    let feed = connection.get("GET", &format!("/upload/progress?id={id}")).await;
    assert_eq!(feed.status(), 200);
    let (answer, mut upload) = connection.open("POST", &format!("/upload?id={id}")).await;
    send(&mut upload, Bytes::from_static(b"partial"), false).await;
    h2.server.until_active(3).await;
    advance_clock(Duration::from_secs(29)).await;
    assert_eq!(h2.server.active().await, 3);
    advance_clock(Duration::from_secs(2)).await;
    assert_eq!(
        read(download.into_body()).await.unwrap_err(),
        Some(Reason::CANCEL),
        "an idle download is reset"
    );
    assert_eq!(read(feed.into_body()).await.unwrap_err(), Some(Reason::CANCEL));
    let answer = answer.await.unwrap();
    assert_eq!(answer.status(), 408);
    assert_eq!(answer.headers()["x-graphite-upload-refusal"], "idle");
    h2.server.until_active(0).await;
}

#[tokio::test]
async fn a_connection_announces_250_streams_and_64_kib_frames_and_waits_ten_seconds_for_its_preface() {
    let h2 = H2::start(&[]).await;
    let socket = TcpStream::connect(h2.server.h2.unwrap()).await.unwrap();
    let name = ServerName::try_from("localhost").unwrap();
    let mut stream = h2.connector.connect(name, socket).await.unwrap();
    let mut settings = [0; 9];
    stream.read_exact(&mut settings).await.unwrap();
    assert_eq!(settings[3], 4, "the server sends its SETTINGS first");
    let mut rest = vec![0; usize::from(settings[2])];
    stream.read_exact(&mut rest).await.unwrap();
    let announced: Vec<_> = rest
        .chunks(6)
        .map(|entry| {
            (
                u16::from_be_bytes([entry[0], entry[1]]),
                u32::from_be_bytes([entry[2], entry[3], entry[4], entry[5]]),
            )
        })
        .collect();
    for setting in [(3, 250), (5, 65_536)] {
        assert!(announced.contains(&setting), "{announced:?}");
    }
    advance_clock(Duration::from_secs(9)).await;
    let read = tokio::time::timeout(Duration::from_millis(50), stream.read(&mut [0; 1])).await;
    assert!(read.is_err(), "the connection waits for its preface");
    advance_clock(Duration::from_secs(1)).await;
    let closed = tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut Vec::new())).await;
    assert!(closed.is_ok(), "the connection closed");
}
