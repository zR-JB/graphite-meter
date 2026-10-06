//! The network layer against canned peers, for what a real server never does, and under chosen proxy and trust
//! settings: proxies, the trust store and grants over verified TLS.
use graphite_meter_client::{
    config::{Config, PathChoice},
    model::Stage,
    net::{Client, Fault, ReadBuffer, Request, ThroughputPath},
    run::{prepare::prepare, upload::UploadSession},
};
use graphite_meter_net::{ConnectError, Pool, Proxy, Trust, Verify};
use graphite_meter_proto::{
    discovery::{Protocol, ThroughputTransport},
    json,
    origin::Origin,
    route::Route,
};
use graphite_meter_testkit::{Identity, Scratch};
use http::Method;
use serde_json::Value;
use std::{
    ffi::OsString,
    net::SocketAddr,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
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

fn ok(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}", body.len())
}

trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Stream for T {}

/// An HTTP/1.1 peer, over TLS with `tls`, sending each request head with its connection's number to the receiver;
/// `answer` gives the `n`th request's answer from its head, `None` ending its connection unanswered.
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

/// A verifying client trusting only the roots of `file` and `directory`, as their variables would name them.
fn trusting(file: &Path, directory: &Path) -> Client {
    let (file, directory) = (file.as_os_str().to_owned(), directory.as_os_str().to_owned());
    let trust = Trust::from_lookup(|name| match name {
        "SSL_CERT_FILE" => Some(file.clone()),
        "SSL_CERT_DIR" => Some(directory.clone()),
        _ => None::<OsString>,
    });
    Client::with(Verify::Trusted(Arc::new(trust)), Proxy::default(), Arc::new(Pool::inline()))
}

fn acceptor(identity: &Identity) -> TlsAcceptor {
    TlsAcceptor::from(Arc::new(identity.server(&[b"http/1.1"])))
}

#[tokio::test]
async fn all_proxy_is_not_read() {
    let (listener, address) = local().await;
    let mut heads = peer(listener, None, |_, _| Some(ok("{}")));
    let (proxy, target) = (format!("http://{address}"), Origin::parse("http://meter.invalid").unwrap());
    let through = |names: &'static [&'static str]| {
        let proxy = Proxy::from_lookup(|name| names.contains(&name).then(|| proxy.clone()));
        Client::with(Verify::trusted(), proxy, Arc::new(Pool::inline()))
    };
    let probed = probe(&through(&["ALL_PROXY", "all_proxy"]), Protocol::Http1, &target).await;
    assert!(matches!(probed, Err(Fault::Connect(ConnectError::Unreachable(_)))), "{probed:?}");
    assert!(heads.try_recv().is_err(), "nothing went through ALL_PROXY");
    probe(&through(&["HTTP_PROXY"]), Protocol::Http1, &target)
        .await
        .unwrap();
    let (_, head) = heads.recv().await.unwrap();
    assert!(head.starts_with("GET http://meter.invalid/probe HTTP/1.1\r\n"), "{head}");
}

#[tokio::test]
async fn an_empty_trust_store_fails_only_tls_connections() {
    let scratch = Scratch::new().unwrap();
    let verified = trusting(&scratch.file("empty.pem", "").unwrap(), &scratch.dir("none").unwrap());
    let ((plain, cleartext), (tls, secure)) = (local().await, local().await);
    let identity = Identity::generate().unwrap();
    let (_plain, _tls) = (
        peer(plain, None, |_, _| Some(ok("{}"))),
        peer(tls, Some(acceptor(&identity)), |_, _| Some(ok("{}"))),
    );
    let secure = origin("https", secure);
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
    let (scratch, mut identity) = (Scratch::new().unwrap(), Identity::self_signed("localhost").unwrap());
    let client = trusting(&scratch.file("root.pem", &identity.certificate).unwrap(), &scratch.dir("none").unwrap());
    identity.ca = identity.certificate.clone();
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
    let [first, second, target] = <[Origin; 3]>::try_from(origins).unwrap();
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

/// An upload session without lanes to a peer answering `answer`, and the request heads the peer saw.
async fn upload(
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
    let session =
        UploadSession::open(&client, &path, Protocol::Http1, Vec::new(), Duration::ZERO, Arc::default(), token);
    (session.await.unwrap(), heads)
}

#[tokio::test]
async fn a_departed_upload_session_asks_its_receiver_to_finalize() {
    let (session, mut heads) = upload(|_, head| match target(head) {
        "/upload/session" => Some(ok(r#"{"uploadId":"u0"}"#)),
        "/upload/progress" if head.starts_with("GET ") => Some(feed(&[r#"{"type":"ready"}"#], false)),
        _ => Some(ok("")),
    })
    .await;
    session.depart();
    let finalized = async {
        while let Some((_, head)) = heads.recv().await {
            if head.starts_with("DELETE /upload/progress?id=u0 HTTP/1.1\r\n") {
                return;
            }
        }
    };
    let finalized = tokio::time::timeout(Duration::from_secs(1), finalized).await;
    finalized.expect("the receiver was asked to finalize");
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

/// A dial before the mint would hold the upload lanes back by its handshakes while the download lanes start.
#[tokio::test]
async fn a_run_mints_its_receiver_over_its_throughput_check_s_connection() {
    let (listener, throughput) = local().await;
    let mut heads = peer(listener, None, |_, head| match target(head) {
        "/probe" => Some(ok(include_str!("../../../api/probe.golden.json"))),
        "/upload/session" => Some(ok(r#"{"uploadId":"u0"}"#)),
        _ => None,
    });
    let elsewhere = format!(
        r#",{{"baseUrl":"http://localhost:{}","transport":"fetch-stream","protocol":"http1"}}"#,
        throughput.port()
    );
    let (listener, served) = local().await;
    let _discovery = peer(listener, None, move |_, head| discovery(head, true, &elsewhere));
    let paths = PathChoice {
        throughput_origin: Some(origin("http", throughput)),
        ..PathChoice::default()
    };
    let config = Config { paths, ..downloads(&origin("http", served), Stage::Upload) };
    let prepared = prepare(&config, client(false)).await.unwrap();
    let paths = prepared.servers[0].path.clone().unwrap();
    assert_eq!(paths.throughput.origin, origin("http", throughput));
    let token = CancellationToken::new();
    let open = UploadSession::open(
        &prepared.client,
        &paths.throughput,
        paths.control,
        Vec::new(),
        Duration::ZERO,
        Arc::default(),
        token,
    );
    drop(open.await.unwrap());
    let probe = heads.recv().await.unwrap();
    let mint = heads.recv().await.unwrap();
    assert_eq!((probe.0, target(&probe.1)), (0, "/probe"));
    assert_eq!((mint.0, target(&mint.1)), (0, "/upload/session"));
}
