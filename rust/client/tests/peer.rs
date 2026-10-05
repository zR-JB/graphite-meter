//! The network layer against canned peers, for what a real server never does, and under the process's environment:
//! proxies, the trust store and grants over verified TLS.
use bytes::Bytes;
use graphite_meter_client::net::{Client, Fault, Request};
use graphite_meter_net::{ConnectError, Pool};
use graphite_meter_proto::{discovery::Protocol, json, origin::Origin, route::Route, upload::Session};
use graphite_meter_testkit::{Identity, Scratch};
use http::Method;
use serde_json::Value;
use std::{
    ffi::OsStr,
    net::SocketAddr,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
};
use tokio_rustls::TlsAcceptor;

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
fn peer(listener: TcpListener, tls: Option<TlsAcceptor>, answer: Answer) -> mpsc::UnboundedReceiver<(usize, String)> {
    let (heads, received) = mpsc::unbounded_channel();
    let requests = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        for connection in 0.. {
            let Ok((tcp, _)) = listener.accept().await else { return };
            let (heads, requests, tls) = (heads.clone(), requests.clone(), tls.clone());
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
    let (first, second) = (Some("Bearer first-grant".to_owned()), Some("Bearer second-grant".to_owned()));
    assert_eq!(sent, [first.clone(), first.clone(), second.clone(), second, first, None]);
}
