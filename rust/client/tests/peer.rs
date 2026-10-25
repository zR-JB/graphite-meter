//! The network layer against canned peers: what a real server never does, proxies, the trust store and grants over TLS.
use graphite_meter_client::{
    config::{Config, PathChoice},
    controller::{Command, Controller},
    events::Events,
    model::Stage,
    net::{Class, Client, Fault, ReadBuffer, Request, ThroughputPath},
    run::{prepare::prepare, upload::UploadSession},
};
use graphite_meter_net::{ConnectError, Pool, Proxy, Trust, Verify};
use graphite_meter_proto::{
    discovery::{Protocol, ThroughputTransport},
    json,
    lease::LeaseRequest,
    origin::Origin,
    reason::FailureReason,
    route::Route,
};
use graphite_meter_testkit::{Identity, Scratch, read_head};
use http::Method;
use serde_json::Value;
use std::{
    assert_matches,
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

async fn probe(client: &Client, via: Protocol, origin: &Origin) -> Result<Value, Fault> {
    client
        .json(
            via,
            Request::new(Method::GET, origin, Route::Probe),
            json::decode,
        )
        .await
}

fn ok(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
}

trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Stream for T {}

/// An HTTP/1.1 peer, over TLS with `tls`, reporting each head with its connection's number; `answer` gives the `n`th answer or `None`.
fn peer(
    listener: TcpListener,
    tls: Option<TlsAcceptor>,
    answer: impl Fn(usize, &str) -> Option<String> + Send + Sync + 'static,
) -> mpsc::UnboundedReceiver<(usize, String)> {
    let (heads, received) = mpsc::unbounded_channel();
    let (requests, answer) = (Arc::new(AtomicUsize::new(0)), Arc::new(answer));
    tokio::spawn(async move {
        for connection in 0.. {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let (heads, requests, tls, answer) =
                (heads.clone(), requests.clone(), tls.clone(), answer.clone());
            tokio::spawn(async move {
                let mut stream: Box<dyn Stream> = match tls {
                    Some(tls) => match tls.accept(tcp).await {
                        Ok(stream) => Box::new(stream),
                        Err(_) => return,
                    },
                    None => Box::new(tcp),
                };
                while let Some(mut head) = read_head(&mut stream).await {
                    let length = head.lines().find_map(|line| {
                        let line = line.to_ascii_lowercase();
                        line.strip_prefix("content-length: ")?.trim().parse().ok()
                    });
                    let mut body = vec![0; length.unwrap_or(0)];
                    if stream.read_exact(&mut body).await.is_err() {
                        return;
                    }
                    head.push_str(&String::from_utf8_lossy(&body));
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

#[tokio::test]
async fn an_abandoned_quic_dial_stops_sending() {
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let origin = Origin::parse(&format!("https://{}", silent.local_addr().unwrap())).unwrap();
    let client = client(false);
    let dial = client.dial(&origin, Protocol::Http3, ReadBuffer::Adaptive);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), dial)
            .await
            .is_err()
    );
    // Its connection's close may still go out; a dial left running resends its Initial about a second in.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut datagram = [0; 2048];
    while silent.try_recv(&mut datagram).is_ok() {}
    let resent =
        tokio::time::timeout(Duration::from_millis(1200), silent.recv(&mut datagram)).await;
    assert!(resent.is_err(), "the abandoned dial kept sending");
}

/// A verifying client trusting only the roots of `file` and `directory`, as their variables would name them.
fn trusting(file: &Path, directory: &Path) -> Client {
    let (file, directory) = (
        file.as_os_str().to_owned(),
        directory.as_os_str().to_owned(),
    );
    let trust = Trust::from_lookup(|name| match name {
        "SSL_CERT_FILE" => Some(file.clone()),
        "SSL_CERT_DIR" => Some(directory.clone()),
        _ => None::<OsString>,
    });
    Client::with(
        Verify::Trusted(Arc::new(trust)),
        Proxy::default(),
        Arc::new(Pool::inline()),
    )
}

fn acceptor(identity: &Identity) -> TlsAcceptor {
    TlsAcceptor::from(Arc::new(identity.server(&[b"http/1.1"])))
}

#[tokio::test]
async fn all_proxy_is_not_read() {
    let (listener, address) = local().await;
    let mut heads = peer(listener, None, |_, _| Some(ok("{}")));
    let (proxy, target) = (
        format!("http://{address}"),
        Origin::parse("http://meter.invalid").unwrap(),
    );
    let through = |names: &'static [&'static str]| {
        let proxy = Proxy::from_lookup(|name| names.contains(&name).then(|| proxy.clone()));
        Client::with(Verify::trusted(), proxy, Arc::new(Pool::inline()))
    };
    let probed = probe(
        &through(&["ALL_PROXY", "all_proxy"]),
        Protocol::Http1,
        &target,
    )
    .await;
    assert_matches!(probed, Err(Fault::Connect(ConnectError::Unreachable(_))));
    assert!(heads.try_recv().is_err(), "nothing went through ALL_PROXY");
    probe(&through(&["HTTP_PROXY"]), Protocol::Http1, &target)
        .await
        .unwrap();
    let (_, head) = heads.recv().await.unwrap();
    assert!(
        head.starts_with("GET http://meter.invalid/probe HTTP/1.1\r\n"),
        "{head}"
    );
}

#[tokio::test]
async fn an_empty_trust_store_fails_only_tls_connections() {
    let scratch = Scratch::new().unwrap();
    let verified = trusting(
        &scratch.file("empty.pem", "").unwrap(),
        &scratch.dir("none").unwrap(),
    );
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
    let refused = probe(&verified, Protocol::Http1, &secure)
        .await
        .unwrap_err();
    assert_matches!(&refused, Fault::Connect(ConnectError::Tls(_)));
    let text = refused.to_string();
    assert!(
        text.starts_with("Certificate not trusted: its issuer is unknown. "),
        "{text}"
    );
    probe(&client(true), Protocol::Http1, &secure)
        .await
        .unwrap();
}

#[tokio::test]
async fn grants_travel_only_in_authorization_and_a_refusal_drops_only_its_server_s() {
    let (scratch, mut identity) = (
        Scratch::new().unwrap(),
        Identity::self_signed("localhost").unwrap(),
    );
    let client = trusting(
        &scratch.file("root.pem", &identity.certificate).unwrap(),
        &scratch.dir("none").unwrap(),
    );
    identity.ca = identity.certificate.clone();
    let refused = |request, _: &str| match request {
        1 => Some(
            "HTTP/1.1 403 Forbidden\r\ngraphite-meter-auth: required\r\ncontent-length: 0\r\n\r\n"
                .into(),
        ),
        _ => Some(ok("{}")),
    };
    let mut origins = Vec::new();
    let mut heads = Vec::new();
    for answer in [
        refused,
        |_, _: &str| Some(ok("{}")),
        |_, _: &str| Some(ok("{}")),
    ] {
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
    assert_matches!(&signed_out, Err(Fault::SignIn(issuer)) if *issuer == first);
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
    let (first, granted) = (
        Some("Bearer first-grant".to_owned()),
        Some("Bearer second-grant".to_owned()),
    );
    assert_eq!(
        sent,
        [
            first.clone(),
            first.clone(),
            granted.clone(),
            granted,
            first,
            None
        ]
    );
    let revoked = client.upload_error(&second, "revoked");
    assert_matches!(&revoked, Fault::SignIn(issuer) if *issuer == second);
    probe(&client, Protocol::Http1, &second).await.unwrap();
    let (_, head) = heads[1].recv().await.unwrap();
    assert!(
        !head.contains("authorization: "),
        "a revoked upload record drops its server's grant: {head}"
    );
}

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
    let grant = LeaseRequest {
        measurements: 4,
        sessions: 0,
        uploads: 0,
    };
    client.lease(&path.origin, &[], grant).await.unwrap();
    let session = UploadSession::open(
        &client,
        &path.origin,
        &path,
        Vec::new(),
        Duration::ZERO,
        Arc::default(),
        token,
    );
    (session.await.unwrap(), heads)
}

#[tokio::test]
async fn a_departed_upload_session_asks_its_receiver_to_finalize() {
    let (session, mut heads) = upload(|_, head| match target(head) {
        "/lease" => Some(ok(LEASE)),
        "/upload/progress" if head.starts_with("GET ") => {
            Some(feed(&[r#"{"type":"ready"}"#], false))
        }
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

const LEASE: &str = r#"{"token":"t","measurements":99,"sessions":99,"uploads":["u0"]}"#;

/// A server catalog of the peer alone, its preflight offering HTTP/1.1 fetch streams and `extra` targets, and its
/// lease.
fn discovery(head: &str, checkpoints: bool, extra: &str) -> Option<String> {
    let preflight = format!(
        r#"{{"server":{{"name":"Peer","location":""}},"engineVersion":"1","generation":"g","capabilities":{{"uploadCheckpoint":{checkpoints},"maxStageMs":300000,"throughput":[{{"baseUrl":".","transport":"fetch-stream","protocol":"http1"}}{extra}],"latency":[]}},"lease":{{"maxMeasurements":32,"maxSessions":8}}}}"#
    );
    match target(head) {
        "/servers" => Some(ok(r#"{"servers":[{"id":"self","url":".","name":"Peer"}]}"#)),
        "/preflight" => Some(ok(&preflight)),
        "/lease" => Some(ok(LEASE)),
        _ => None,
    }
}

fn downloads(url: &Origin, stage: Stage) -> Config {
    Config {
        url: Some(url.clone()),
        stages: vec![stage],
        loaded_latency: false,
        ..Config::default()
    }
}

/// The run's receiver is minted with its lease at the discovery origin, which a transfer origin never sees.
#[tokio::test]
async fn a_run_mints_its_receiver_with_its_lease_at_the_discovery_origin() {
    let (listener, throughput) = local().await;
    let mut transfers = peer(listener, None, |_, head| match target(head) {
        "/probe" => Some(ok(include_str!("../../../api/probe.golden.json"))),
        _ => None,
    });
    let elsewhere = format!(
        r#",{{"baseUrl":"http://localhost:{}","transport":"fetch-stream","protocol":"http1"}}"#,
        throughput.port()
    );
    let (listener, served) = local().await;
    let mut discovered = peer(listener, None, move |_, head| {
        discovery(head, true, &elsewhere)
    });
    let paths = PathChoice {
        throughput_origin: Some(origin("http", throughput)),
        ..PathChoice::default()
    };
    let config = Config {
        paths,
        ..downloads(&origin("http", served), Stage::Upload)
    };
    let prepared = prepare(&config, client(false)).await.unwrap();
    let paths = prepared.servers[0].path.clone().unwrap();
    assert_eq!(paths.throughput.origin, origin("http", throughput));
    let open = UploadSession::open(
        &prepared.client,
        &prepared.servers[0].origin,
        &paths.throughput,
        Vec::new(),
        Duration::ZERO,
        Arc::default(),
        CancellationToken::new(),
    );
    drop(open.await.unwrap());
    let heads: Vec<_> = std::iter::from_fn(|| discovered.try_recv().ok())
        .map(|(_, head)| head)
        .collect();
    let leases: Vec<_> = heads
        .iter()
        .filter(|head| target(head) == "/lease")
        .collect();
    assert_eq!(leases.len(), 2, "{heads:?}");
    assert!(
        leases[0].ends_with(r#""sessions":0,"uploads":0}"#),
        "{}",
        leases[0]
    );
    assert!(
        leases[1].contains("graphite-meter-lease: t\r\n") && leases[1].ends_with(r#""uploads":1}"#),
        "{}",
        leases[1]
    );
    assert_eq!(target(&transfers.recv().await.unwrap().1), "/probe");
    assert!(
        transfers.try_recv().is_err(),
        "nothing else reached the transfer origin"
    );
}

#[tokio::test]
async fn a_server_of_another_api_version_is_named_either_way_and_a_lease_cut_below_the_ask_at_the_limit()
 {
    const LIMITS: &str = r#","lease":{"maxMeasurements":32,"maxSessions":8}"#;
    let newer = format!(
        "Peer runs Graphite Meter 0.15.0, newer than this client ({}); update this client.",
        graphite_meter_client::VERSION
    );
    for (shape, text) in [
        // A 0.13.4 server's: no API version and no lease.
        (
            "older",
            "Peer runs Graphite Meter 0.13.4 (API version 1); this client needs API version 2, from 0.13.5. Update Peer.",
        ),
        ("newer", newer.as_str()),
        (
            "cut",
            "the test needs more measurements at once than this server admits for one client",
        ),
    ] {
        let (listener, served) = local().await;
        let _peer = peer(listener, None, move |_, head| {
            let found = discovery(head, true, "")?;
            let body = &found[found.find("\r\n\r\n")? + 4..];
            let older = body
                .replace(LIMITS, "")
                .replace(r#""engineVersion":"1""#, r#""engineVersion":"0.13.4""#);
            let newer = body.replace(
                r#""engineVersion":"1""#,
                r#""engineVersion":"0.15.0","apiVersion":3"#,
            );
            match (target(head), shape) {
                ("/preflight", "older") => Some(ok(&older)),
                ("/preflight", "newer") => Some(ok(&newer)),
                ("/lease", _) => Some(ok(
                    r#"{"token":"t","measurements":1,"sessions":0,"uploads":[]}"#,
                )),
                _ => Some(found),
            }
        });
        let config = downloads(&origin("http", served), Stage::Download);
        let prepared = prepare(&config, client(false)).await.unwrap();
        let failure = prepared.servers[0].path.clone().unwrap_err();
        assert_eq!(failure.text, text, "{shape}");
    }
}

#[tokio::test]
async fn a_transfer_port_whose_probe_names_another_api_version_is_named() {
    let (listener, served) = local().await;
    let _peer = peer(listener, None, |_, head| match target(head) {
        "/probe" => Some(ok(
            r#"{"engineVersion":"0.15.0","apiVersion":3,"clientIp":"127.0.0.1","futureShape":[]}"#,
        )),
        "/lease" => Some(ok(LEASE)),
        _ => discovery(head, true, ""),
    });
    let at = origin("http", served);
    let prepared = prepare(&downloads(&at, Stage::Download), client(false))
        .await
        .unwrap();
    let failure = prepared.servers[0].path.clone().unwrap_err();
    assert_eq!(
        failure.text,
        format!(
            "{at} runs Graphite Meter 0.15.0, newer than this client ({}); update this client.",
            graphite_meter_client::VERSION
        )
    );
}

/// A check replaced by a run, the run stopped, and the check a stop starts again end their leases, which the server
/// would otherwise hold against this client's share until they idle out.
#[tokio::test]
async fn a_check_or_run_cut_short_ends_its_lease() {
    let (listener, served) = local().await;
    let mut heads = peer(listener, None, |_, head| match target(head) {
        // The probe's answer never completes, so every check is still running when it is cut short.
        "/probe" => Some("HTTP/1.1 200 OK\r\ncontent-length: 64\r\n\r\n{".into()),
        _ => discovery(head, false, ""),
    });
    let config = downloads(&origin("http", served), Stage::Download);
    let (events, _received) = Events::channel();
    let mut controller = Controller::new(true, Arc::new(Pool::inline()), events);
    let mut seen = Vec::new();
    let mut probed = async |seen: &mut Vec<String>| {
        while let Some((_, head)) = heads.recv().await {
            seen.push(head);
            if target(seen.last().unwrap()) == "/probe" {
                return;
            }
        }
    };
    for command in [
        Command::Check(config.clone()),
        Command::Run(config.clone()),
        Command::Stop,
    ] {
        controller.command(command);
        probed(&mut seen).await;
    }
    controller.command(Command::Stop);
    controller.settled().await;
    let leases = |seen: &[String]| {
        let leases = seen.iter().filter(|head| target(head) == "/lease");
        leases
            .map(|head| head.starts_with("DELETE ") && head.contains("graphite-meter-lease: t\r\n"))
            .collect::<Vec<_>>()
    };
    let ended = tokio::time::timeout(Duration::from_secs(2), async {
        while leases(&seen).len() < 6 {
            seen.extend(heads.recv().await.map(|(_, head)| head));
        }
    });
    ended.await.unwrap_or_default();
    assert_eq!(
        leases(&seen),
        [false, true].repeat(3),
        "each lease taken, then ended with its token"
    );
}

#[tokio::test]
async fn a_proxy_that_refuses_fails_the_path_and_one_that_reaches_no_target_is_retried() {
    for (answer, reason, class) in [
        (
            "407 Proxy Authentication Required",
            FailureReason::PathFailed,
            Class::Final,
        ),
        (
            "502 Bad Gateway",
            FailureReason::ConnectionLost,
            Class::Redial,
        ),
    ] {
        let (listener, address) = local().await;
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_head(&mut stream).await;
            let refusal = format!("HTTP/1.1 {answer}\r\ncontent-length: 0\r\n\r\n");
            stream.write_all(refusal.as_bytes()).await.unwrap();
        });
        let proxy = format!("http://{address}");
        let proxy = Proxy::from_lookup(|name| (name == "HTTPS_PROXY").then(|| proxy.clone()));
        let client = Client::with(Verify::trusted(), proxy, Arc::new(Pool::inline()));
        let origin = Origin::parse("https://meter.example").unwrap();
        let fault = probe(&client, Protocol::Negotiated, &origin)
            .await
            .unwrap_err();
        assert_eq!((fault.reason(), fault.class()), (reason, class), "{fault}");
        assert_eq!(
            fault.to_string(),
            format!("Could not connect: proxy refused CONNECT with {answer}")
        );
    }
}
