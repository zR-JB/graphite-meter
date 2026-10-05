//! The network layer against canned peers, for what a real server never does, and under the process's environment:
//! proxies, the trust store and grants over verified TLS.
use bytes::Bytes;
use graphite_meter_client::{
    config::Config,
    model::{Dir, Failure, LaneHealth, Stage},
    net::{Client, Fault, ReadBuffer, Request, ThroughputPath, retrying, topology},
    run::{prepare::prepare, upload::UploadSession},
};
use graphite_meter_net::{ConnectError, Pool};
use graphite_meter_proto::{
    discovery::{Protocol, ThroughputTransport},
    json,
    origin::Origin,
    reason::FailureReason,
    route::Route,
    upload::Session,
};
use graphite_meter_testkit::{Identity, Scratch};
use http::Method;
use serde_json::Value;
use std::{
    ffi::OsStr,
    net::SocketAddr,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, UdpSocket},
    sync::mpsc,
};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

/// Set in a child process that runs one test with the environment its parent chose.
const CHILD: &str = "GRAPHITE_METER_TEST_CHILD";
/// The private key of the self-signed server a child trusts.
const KEY: &str = "GRAPHITE_METER_TEST_KEY";

fn client(insecure: bool) -> Client {
    Client::new(insecure, Arc::new(Pool::inline()))
}

async fn local() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (listener, address)
}

fn origin(scheme: &str, address: SocketAddr) -> Origin {
    Origin::parse(&format!("{scheme}://localhost:{}", address.port())).unwrap()
}

/// A probe's answer as any JSON object.
async fn probe(client: &Client, via: Protocol, origin: &Origin) -> Result<Value, Fault> {
    client
        .json(via, Request::new(Method::GET, origin, Route::Probe), json::decode)
        .await
}

/// Moves paused time on by `duration` while network waits keep the running clock.
async fn advance_clock(duration: Duration) {
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::task::yield_now().await;
    tokio::time::resume();
}

fn ok(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}", body.len())
}

trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Stream for T {}

/// The `n`th request's answer, given its head; `None` ends its connection unanswered.
type Answer = fn(usize, &str) -> Option<String>;

/// An HTTP/1.1 peer, over TLS with `tls`, sending each request head with its connection's number to the receiver.
fn peer(
    listener: TcpListener,
    tls: Option<TlsAcceptor>,
    answer: impl Fn(usize, &str) -> Option<String> + Send + Sync + 'static,
) -> mpsc::UnboundedReceiver<(usize, String)> {
    let (heads, received) = mpsc::unbounded_channel();
    let (requests, answer) = (Arc::new(AtomicUsize::new(0)), Arc::new(answer));
    tokio::spawn(async move {
        for connection in 0.. {
            let Ok((tcp, _)) = listener.accept().await else { return };
            let (heads, requests, tls, answer) = (heads.clone(), requests.clone(), tls.clone(), answer.clone());
            tokio::spawn(async move {
                let mut stream: Box<dyn Stream> = match tls {
                    Some(tls) => match tls.accept(tcp).await {
                        Ok(stream) => Box::new(stream),
                        Err(_) => return,
                    },
                    None => Box::new(tcp),
                };
                while let Some(head) = read_head(&mut stream).await {
                    let _ = heads.send((connection, head.clone()));
                    let Some(reply) = answer(requests.fetch_add(1, Ordering::SeqCst), &head) else {
                        return;
                    };
                    if stream.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    received
}

async fn read_head(stream: &mut Box<dyn Stream>) -> Option<String> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(stream.read_u8().await.ok()?);
    }
    String::from_utf8(head).ok()
}

#[tokio::test]
async fn a_request_without_response_headers_in_ten_seconds_retires_its_http2_connection() {
    let (listener, address) = local().await;
    let (requests, mut received) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        for connection in 0..2 {
            let mut h2 = h2::server::handshake(listener.accept().await.unwrap().0).await.unwrap();
            let requests = requests.clone();
            tokio::spawn(async move {
                let mut withheld = Vec::new();
                while let Some(Ok((_, mut respond))) = h2.accept().await {
                    requests.send(connection).unwrap();
                    if connection == 0 {
                        withheld.push(respond);
                        continue;
                    }
                    let mut body = respond.send_response(http::Response::new(()), false).unwrap();
                    body.send_data(Bytes::from_static(b"{}"), true).unwrap();
                }
            });
        }
    });
    let (client, origin) = (client(false), origin("http", address));
    let first = tokio::spawn({
        let (client, origin) = (client.clone(), origin.clone());
        async move { probe(&client, Protocol::Http2, &origin).await }
    });
    assert_eq!(received.recv().await, Some(0));
    advance_clock(Duration::from_secs(10)).await;
    assert!(matches!(first.await.unwrap(), Err(Fault::TimedOut(_))));
    probe(&client, Protocol::Http2, &origin).await.unwrap();
    assert_eq!(received.recv().await, Some(1), "the next request dials a new connection");
}

#[tokio::test]
async fn an_http2_connection_that_reads_nothing_for_30_s_is_pinged_and_closed_20_s_later() {
    let (listener, address) = local().await;
    let (frames, mut received) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut stream = listener.accept().await.unwrap().0;
        let mut preface = [0; 24];
        stream.read_exact(&mut preface).await.unwrap();
        stream.write_all(&[0, 0, 0, 4, 0, 0, 0, 0, 0]).await.unwrap();
        loop {
            let mut header = [0; 9];
            if stream.read_exact(&mut header).await.is_err() {
                let _ = frames.send(None);
                return;
            }
            let mut payload = vec![0; u32::from_be_bytes([0, header[0], header[1], header[2]]) as usize];
            stream.read_exact(&mut payload).await.unwrap();
            let reply: &[u8] = match &header[3..5] {
                [4, 0] => &[0, 0, 0, 4, 1, 0, 0, 0, 0],
                // `:status: 200` ending the stream that asked.
                [1, _] => &[0, 0, 1, 1, 5, header[5], header[6], header[7], header[8], 0x88],
                _ => &[],
            };
            stream.write_all(reply).await.unwrap();
            let _ = frames.send(Some(header[3]));
        }
    });
    let (client, request) = (client(false), Request::new(Method::GET, &origin("http", address), Route::Probe));
    drop(client.control(Protocol::Http2, request).await.unwrap());
    advance_clock(Duration::from_secs(29)).await;
    assert_eq!(ping_or_end(&mut received, 200).await, None, "silent for 29 s");
    advance_clock(Duration::from_secs(2)).await;
    assert_eq!(ping_or_end(&mut received, 2000).await, Some(Some(6)), "pinged after 30 s");
    advance_clock(Duration::from_secs(18)).await;
    assert_eq!(ping_or_end(&mut received, 200).await, None, "waiting 20 s for the answer");
    advance_clock(Duration::from_secs(4)).await;
    assert_eq!(ping_or_end(&mut received, 2000).await, Some(None), "closed unanswered");
}

/// The next PING frame, `Some(None)` at the connection's end, or `None` when neither comes within `wait` ms.
async fn ping_or_end(received: &mut mpsc::UnboundedReceiver<Option<u8>>, wait: u64) -> Option<Option<u8>> {
    let next = async {
        while let Some(frame) = received.recv().await {
            if frame.is_none_or(|kind| kind == 6) {
                return frame;
            }
        }
        None
    };
    tokio::time::timeout(Duration::from_millis(wait), next).await.ok()
}

#[tokio::test]
async fn a_bodyless_post_failing_on_a_reused_connection_is_sent_once_more() {
    let (listener, address) = local().await;
    let mut heads = peer(listener, None, |request, _| match request {
        1 => None,
        _ => Some(ok(&format!(r#"{{"uploadId":"u{request}"}}"#))),
    });
    let (client, origin) = (client(false), origin("http", address));
    let session = || {
        client.json(
            Protocol::Http1,
            Request::new(Method::POST, &origin, Route::UploadSession),
            Session::decode,
        )
    };
    assert_eq!(session().await.unwrap().upload_id, "u0");
    assert_eq!(session().await.unwrap().upload_id, "u2");
    for connection in [0, 0, 1] {
        let (seen, head) = heads.recv().await.unwrap();
        assert_eq!(seen, connection);
        assert!(head.starts_with("POST /upload/session HTTP/1.1\r\n"), "{head}");
    }
}

/// A minted upload ID as `retrying` gets it from a peer answering `answer`, and the requests that took.
async fn mint(answer: Answer) -> (Result<Session, Fault>, usize) {
    let (listener, address) = local().await;
    let mut heads = peer(listener, None, answer);
    let (client, origin) = (client(false), origin("http", address));
    let request = || Request::new(Method::POST, &origin, Route::UploadSession);
    let minted = retrying(|| client.json(Protocol::Http1, request(), Session::decode)).await;
    drop(client);
    let mut requests = 0;
    while heads.try_recv().is_ok() {
        requests += 1;
    }
    (minted, requests)
}

#[tokio::test]
async fn minting_redials_a_server_error_and_stands_at_a_client_error() {
    let (minted, requests) = mint(|request, _| match request {
        0 => Some("HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\n\r\n".into()),
        _ => Some(ok(r#"{"uploadId":"u1"}"#)),
    })
    .await;
    assert_eq!((minted.unwrap().upload_id.as_str(), requests), ("u1", 2));
    let (refused, requests) = mint(|_, _| Some("HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n".into())).await;
    assert!(matches!(refused, Err(Fault::Status { .. })), "{refused:?}");
    assert_eq!(requests, 1);
}

#[tokio::test]
async fn a_departed_upload_session_asks_its_receiver_to_finalize() {
    let (listener, address) = local().await;
    let mut heads = peer(listener, None, |_, head| match head.split_once(' ').map(|(_, rest)| rest) {
        Some(rest) if rest.starts_with("/upload/session ") => Some(ok(r#"{"uploadId":"u0"}"#)),
        Some(rest) if rest.starts_with("/upload/progress?id=u0 ") && head.starts_with("GET") => {
            Some("HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n11\r\n{\"type\":\"ready\"}\n\r\n".into())
        }
        Some(rest) if rest.starts_with("/upload/progress") => Some(ok("")),
        _ => None,
    });
    let origin = origin("http", address);
    let path = ThroughputPath {
        origin,
        transport: ThroughputTransport::FetchStream,
        protocol: Protocol::Http1,
    };
    let plans = topology(&path, Stage::Upload, Dir { down: 0, up: 1 });
    let (client, token) = (client(false), CancellationToken::new());
    let session = UploadSession::open(&client, &path, plans, Duration::ZERO, Arc::default(), token);
    session.await.unwrap().depart();
    let finalized = async {
        while let Some((_, head)) = heads.recv().await {
            if head.starts_with("DELETE /upload/progress?id=u0 HTTP/1.1\r\n") {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(1), finalized)
        .await
        .expect("the receiver was asked to finalize");
}

#[tokio::test]
async fn an_abandoned_quic_dial_stops_sending() {
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let origin = Origin::parse(&format!("https://{}", silent.local_addr().unwrap())).unwrap();
    let client = client(false);
    let dial = client.dial(&origin, Protocol::Http3, ReadBuffer::Adaptive);
    assert!(tokio::time::timeout(Duration::from_millis(200), dial).await.is_err());
    // Its connection's close may still go out; a dial left running resends its Initial about a second in.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut datagram = [0; 2048];
    while silent.try_recv(&mut datagram).is_ok() {}
    let resent = tokio::time::timeout(Duration::from_secs(2), silent.recv(&mut datagram)).await;
    assert!(resent.is_err(), "the abandoned dial kept sending");
}

#[tokio::test]
async fn an_answer_over_64_kib_fails() {
    fn padded(bytes: usize) -> String {
        format!(r#"{{"a":"{}"}}"#, "x".repeat(bytes - 8))
    }
    let (listener, address) = local().await;
    let _heads = peer(listener, None, |request, _| {
        let close = "HTTP/1.1 200 OK\r\nconnection: close\r\n";
        Some(match request {
            0 => format!("{close}content-length: 65536\r\n\r\n{}", padded(65536)),
            1 => format!("{close}content-length: 65537\r\n\r\n{}", padded(65537)),
            _ => format!("{close}transfer-encoding: chunked\r\n\r\n10001\r\n{}\r\n0\r\n\r\n", padded(65537)),
        })
    });
    let (client, origin) = (client(false), origin("http", address));
    probe(&client, Protocol::Http1, &origin).await.unwrap();
    for _ in ["declared", "streamed"] {
        let oversized = probe(&client, Protocol::Http1, &origin).await;
        assert!(
            matches!(&oversized, Err(Fault::Malformed(detail)) if detail.contains("exceeds 65536")),
            "{oversized:?}"
        );
    }
}

/// Runs `test` again in a child process with only `variables` of the proxy and trust settings set.
async fn rerun(test: &str, variables: &[(&str, &OsStr)]) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args([test, "--exact", "--nocapture"]).env(CHILD, "1");
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "ALL_PROXY", "SSL_CERT_FILE", "SSL_CERT_DIR"] {
        command.env_remove(name).env_remove(name.to_ascii_lowercase());
    }
    command.envs(variables.iter().copied());
    let status = tokio::task::spawn_blocking(move || command.status()).await.unwrap();
    assert!(status.unwrap().success(), "{test} failed in its child process");
}

fn child() -> bool {
    std::env::var_os(CHILD).is_some()
}

fn acceptor(identity: &Identity) -> TlsAcceptor {
    TlsAcceptor::from(Arc::new(identity.server(&[b"http/1.1"])))
}

#[tokio::test]
async fn all_proxy_is_not_read() {
    if !child() {
        let (listener, address) = local().await;
        let mut heads = peer(listener, None, |_, _| Some(ok("{}")));
        let proxy = format!("http://{address}");
        rerun("all_proxy_is_not_read", &[("ALL_PROXY", proxy.as_ref()), ("all_proxy", proxy.as_ref())]).await;
        assert!(heads.try_recv().is_err(), "nothing went through ALL_PROXY");
        rerun("all_proxy_is_not_read", &[("HTTP_PROXY", proxy.as_ref())]).await;
        let (_, head) = heads.recv().await.unwrap();
        assert!(head.starts_with("GET http://meter.invalid/probe HTTP/1.1\r\n"), "{head}");
        return;
    }
    let target = Origin::parse("http://meter.invalid").unwrap();
    let probed = probe(&client(false), Protocol::Http1, &target).await;
    match std::env::var_os("HTTP_PROXY") {
        Some(_) => assert!(probed.is_ok(), "{probed:?}"),
        None => assert!(matches!(probed, Err(Fault::Connect(ConnectError::Unreachable(_)))), "{probed:?}"),
    }
}

#[tokio::test]
async fn an_empty_trust_store_fails_only_tls_connections() {
    if !child() {
        let scratch = Scratch::new().unwrap();
        let (file, directory) = (scratch.file("empty.pem", "").unwrap(), scratch.dir("none").unwrap());
        let variables = [("SSL_CERT_FILE", file.as_os_str()), ("SSL_CERT_DIR", directory.as_os_str())];
        return rerun("an_empty_trust_store_fails_only_tls_connections", &variables).await;
    }
    let ((plain, cleartext), (tls, secure)) = (local().await, local().await);
    let identity = Identity::generate().unwrap();
    let (_plain, _tls) = (
        peer(plain, None, |_, _| Some(ok("{}"))),
        peer(tls, Some(acceptor(&identity)), |_, _| Some(ok("{}"))),
    );
    let (verified, secure) = (client(false), origin("https", secure));
    probe(&verified, Protocol::Http1, &origin("http", cleartext))
        .await
        .unwrap();
    let refused = probe(&verified, Protocol::Http1, &secure).await.unwrap_err();
    assert!(matches!(&refused, Fault::Connect(ConnectError::Tls(_))), "{refused:?}");
    let text = refused.to_string();
    assert!(
        text.starts_with("Certificate not trusted: certificate signed by unknown authority. "),
        "{text}"
    );
    probe(&client(true), Protocol::Http1, &secure).await.unwrap();
}

#[tokio::test]
async fn grants_travel_only_in_authorization_and_a_refusal_drops_only_its_server_s() {
    if !child() {
        let (scratch, identity) = (Scratch::new().unwrap(), Identity::self_signed("localhost").unwrap());
        let file = scratch.file("root.pem", &identity.certificate).unwrap();
        let key = scratch.file("root.key", &identity.key).unwrap();
        let directory = scratch.dir("none").unwrap();
        let variables = [
            ("SSL_CERT_FILE", file.as_os_str()),
            ("SSL_CERT_DIR", directory.as_os_str()),
            (KEY, key.as_os_str()),
        ];
        return rerun("grants_travel_only_in_authorization_and_a_refusal_drops_only_its_server_s", &variables).await;
    }
    let read = |name| std::fs::read_to_string(std::env::var_os(name).unwrap()).unwrap();
    let identity = Identity {
        ca: read("SSL_CERT_FILE"),
        certificate: read("SSL_CERT_FILE"),
        key: read(KEY),
    };
    let refused = |request, _: &str| match request {
        1 => Some("HTTP/1.1 403 Forbidden\r\ngraphite-meter-auth: required\r\ncontent-length: 0\r\n\r\n".into()),
        _ => Some(ok("{}")),
    };
    let mut origins = Vec::new();
    let mut heads = Vec::new();
    for answer in [refused, |_, _: &str| Some(ok("{}")), |_, _: &str| Some(ok("{}"))] {
        let (listener, address) = local().await;
        heads.push(peer(listener, Some(acceptor(&identity)), answer));
        origins.push(origin("https", address));
    }
    let ([first, second, target], client) = (<[Origin; 3]>::try_from(origins).unwrap(), client(false));
    client.grant(&first, "first-grant");
    client.grant(&second, "second-grant");
    client.enroll(&second, &[]);
    client.enroll(&first, &[second.clone(), target.clone()]);
    for origin in [&first, &target, &second] {
        probe(&client, Protocol::Http1, origin).await.unwrap();
    }
    let signed_out = probe(&client, Protocol::Http1, &first).await;
    assert!(matches!(&signed_out, Err(Fault::SignIn(issuer)) if *issuer == first), "{signed_out:?}");
    for origin in [&target, &second] {
        probe(&client, Protocol::Http1, origin).await.unwrap();
    }
    let mut sent = Vec::new();
    for heads in &mut heads {
        for _ in 0..2 {
            let (_, head) = heads.recv().await.unwrap();
            assert!(head.starts_with("GET /probe HTTP/1.1\r\n"), "{head}");
            sent.push(
                head.lines()
                    .find_map(|line| line.strip_prefix("authorization: "))
                    .map(str::to_owned),
            );
        }
    }
    let (first, granted) = (Some("Bearer first-grant".to_owned()), Some("Bearer second-grant".to_owned()));
    assert_eq!(sent, [first.clone(), first.clone(), granted.clone(), granted, first, None]);
    let revoked = client.upload_error(&second, "revoked");
    assert!(matches!(&revoked, Fault::SignIn(issuer) if *issuer == second), "{revoked:?}");
    probe(&client, Protocol::Http1, &second).await.unwrap();
    let (_, head) = heads[1].recv().await.unwrap();
    assert!(
        !head.contains("authorization: "),
        "a revoked upload record drops its server's grant: {head}"
    );
}

/// The path a request head asks for, without its query.
fn target(head: &str) -> &str {
    let target = head.split(' ').nth(1).unwrap_or_default();
    target.split_once('?').map_or(target, |(path, _)| path)
}

/// A chunked progress feed carrying `records`, left open unless `ended`.
fn feed(records: &[&str], ended: bool) -> String {
    let lines: String = records.iter().map(|record| format!("{record}\n")).collect();
    let end = if ended { "0\r\n\r\n" } else { "" };
    format!(
        "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n{lines}\r\n{end}",
        lines.len()
    )
}

const BUSY: &str = "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n";

/// An upload session without lanes to a peer answering `answer`, sharing `replaced` with the run, and the request
/// heads the peer saw.
async fn upload(
    replaced: Arc<AtomicBool>,
    answer: impl Fn(usize, &str) -> Option<String> + Send + Sync + 'static,
) -> (UploadSession, mpsc::UnboundedReceiver<(usize, String)>) {
    let (listener, address) = local().await;
    let heads = peer(listener, None, answer);
    let path = ThroughputPath {
        origin: origin("http", address),
        transport: ThroughputTransport::FetchStream,
        protocol: Protocol::Http1,
    };
    let (client, token) = (client(false), CancellationToken::new());
    let session = UploadSession::open(&client, &path, Vec::new(), Duration::ZERO, replaced, token);
    (session.await.unwrap(), heads)
}

/// The failure `session`'s health ends with, polled as a stage does.
async fn failure(session: &mut UploadSession) -> Failure {
    let failed = async {
        loop {
            if let LaneHealth::Failed(failure) = session.health() {
                return failure;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), failed)
        .await
        .expect("the session failed")
}

fn minted(heads: &mut mpsc::UnboundedReceiver<(usize, String)>) -> usize {
    std::iter::from_fn(|| heads.try_recv().ok())
        .filter(|(_, head)| head.starts_with("POST /upload/session "))
        .count()
}

#[tokio::test]
async fn an_unknown_receiver_is_replaced_once_per_server_and_run_and_then_ends_the_stage() {
    let replaced = Arc::new(AtomicBool::new(false));
    let invalid = |request, head: &str| match target(head) {
        "/upload/session" => Some(ok(&format!(r#"{{"uploadId":"u{request}"}}"#))),
        "/upload/progress" if head.starts_with("GET ") => Some(feed(&[r#"{"type":"error","code":"invalid"}"#], true)),
        _ => Some(ok("")),
    };
    let (mut first, mut heads) = upload(replaced.clone(), invalid).await;
    assert_eq!(failure(&mut first).await.reason, FailureReason::ProtocolError);
    assert_eq!(minted(&mut heads), 2, "one replacement receiver");
    let (mut later, mut heads) = upload(replaced, invalid).await;
    assert_eq!(failure(&mut later).await.reason, FailureReason::ProtocolError);
    assert_eq!(minted(&mut heads), 1, "no second replacement in the run");
}

#[tokio::test]
async fn a_progress_feed_ending_early_is_reopened_for_the_same_receiver() {
    let (mut session, mut heads) = upload(Arc::default(), |request, head| match target(head) {
        "/upload/session" => Some(ok(r#"{"uploadId":"u0"}"#)),
        "/upload/progress" if head.starts_with("GET ") => Some(match request {
            1 => feed(&[r#"{"type":"ready"}"#, r#"{"type":"progress","bytes":1000,"nanos":1000000}"#], true),
            _ => feed(&[r#"{"type":"ready"}"#, r#"{"type":"progress","bytes":2000,"nanos":2000000}"#], false),
        }),
        _ => Some(ok("")),
    })
    .await;
    let recovered = async {
        while session.fed().map(|fed| fed.bytes) != Some(2000) {
            assert_eq!(session.health(), LaneHealth::Ok);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), recovered)
        .await
        .expect("the feed was reopened");
    let feeds: Vec<_> = std::iter::from_fn(|| heads.try_recv().ok())
        .filter(|(_, head)| head.starts_with("GET /upload/progress"))
        .collect();
    assert_eq!(feeds.len(), 2);
    assert!(
        feeds
            .iter()
            .all(|(_, head)| head.starts_with("GET /upload/progress?id=u0 ")),
        "{feeds:?}"
    );
}

#[tokio::test]
async fn a_missed_checkpoint_is_asked_again_every_100_ms_within_its_budget() {
    let asked = Arc::new(AtomicUsize::new(0));
    let (session, _heads) = upload(Arc::default(), {
        let asked = asked.clone();
        move |_, head| match target(head) {
            "/upload/session" => Some(ok(r#"{"uploadId":"u0"}"#)),
            "/upload/checkpoint" => Some(match asked.fetch_add(1, Ordering::SeqCst) {
                2 => ok(r#"{"bytes":5,"nanos":7}"#),
                _ => BUSY.into(),
            }),
            "/upload/progress" if head.starts_with("GET ") => Some(feed(&[r#"{"type":"ready"}"#], false)),
            _ => Some(ok("")),
        }
    })
    .await;
    let started = std::time::Instant::now();
    let receiver = session.checkpoint(Duration::from_millis(1500)).await.unwrap();
    assert_eq!((receiver.counters.bytes(), receiver.counters.nanos()), (5, 7));
    assert_eq!(asked.load(Ordering::SeqCst), 3);
    assert!(started.elapsed() >= Duration::from_millis(200), "{:?}", started.elapsed());

    let started = std::time::Instant::now();
    assert!(session.checkpoint(Duration::from_millis(500)).await.is_err());
    assert!(started.elapsed() >= Duration::from_millis(400), "{:?}", started.elapsed());
    let missed = asked.load(Ordering::SeqCst) - 3;
    assert!((1..=5).contains(&missed), "{missed} asks within 500 ms");
}

/// A catalogue of the peer alone, and its preflight offering HTTP/1.1 fetch streams and `extra` targets.
fn discovery(head: &str, checkpoints: bool, extra: &str) -> Option<String> {
    let preflight = format!(
        r#"{{"server":{{"name":"Peer","location":""}},"engineVersion":"1","generation":"g","capabilities":{{"uploadCheckpoint":{checkpoints},"throughput":[{{"baseUrl":".","transport":"fetch-stream","protocol":"http1"}}{extra}],"latency":[]}}}}"#
    );
    match target(head) {
        "/servers" => Some(ok(r#"{"servers":[{"id":"self","url":".","name":"Peer"}]}"#)),
        "/preflight" => Some(ok(&preflight)),
        _ => None,
    }
}

fn downloads(url: &Origin, stage: Stage) -> Config {
    Config {
        url: url.clone(),
        stages: vec![stage],
        loaded_latency: false,
        ..Config::default()
    }
}

#[tokio::test]
async fn uploads_need_every_server_to_offer_receiver_checkpoints() {
    let (listener, address) = local().await;
    let _heads = peer(listener, None, |_, head| match target(head) {
        "/probe" => Some(ok(include_str!("../../../api/probe.golden.json"))),
        _ => discovery(head, false, ""),
    });
    let url = origin("http", address);
    let prepared = prepare(&downloads(&url, Stage::Download), client(false)).await.unwrap();
    assert!(prepared.servers[0].path.is_ok(), "{:?}", prepared.servers[0].path);
    let refused = Failure::new(
        FailureReason::PreparationFailed,
        "receiver checkpoint support is required; upgrade this measurement server",
    );
    for stage in [Stage::Upload, Stage::Bidirectional] {
        let prepared = prepare(&downloads(&url, stage), client(false)).await.unwrap();
        assert_eq!(prepared.servers[0].path, Err(refused.clone()), "{stage:?}");
    }
}

#[tokio::test]
async fn a_path_check_ends_12_s_after_it_began_not_when_one_request_times_out() {
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let quic = format!(
        r#",{{"baseUrl":"https://localhost:{}","transport":"webtransport","protocol":"http3"}}"#,
        silent.local_addr().unwrap().port()
    );
    let (listener, address) = local().await;
    let mut heads = peer(listener, None, move |_, head| match target(head) {
        // Writing nothing leaves the probe unanswered.
        "/probe" => Some(String::new()),
        _ => discovery(head, true, &quic),
    });
    let config = downloads(&origin("http", address), Stage::Download);
    let mut check = tokio::spawn(async move { prepare(&config, client(false)).await });
    while !heads.recv().await.unwrap().1.starts_with("GET /probe ") {}
    advance_clock(Duration::from_secs(10)).await;
    let mut initial = [0; 1500];
    let dialed = tokio::time::timeout(Duration::from_secs(2), silent.recv_from(&mut initial)).await;
    assert!(dialed.is_ok(), "the timed-out probe moved on to the WebTransport path");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!check.is_finished(), "the fetch-stream probe's timeout leaves the WebTransport check");
    advance_clock(Duration::from_millis(2500)).await;
    let prepared = tokio::time::timeout(Duration::from_secs(5), &mut check)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let late = Failure::new(FailureReason::Timeout, "the path check did not finish within 12 seconds");
    assert_eq!(prepared.servers[0].path, Err(late));
}
