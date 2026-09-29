use super::*;
use futures_util::{SinkExt, StreamExt};
use graphite_meter_core::wire;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Barrier,
};
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone, Copy)]
pub(crate) enum FixtureMode {
    Latency,
    Throughput,
    Negotiated,
}

/// The path of the request `stream` carries, read without taking it; None once the stream closed.
pub(crate) async fn request_path(stream: &TcpStream) -> Result<Option<String>, Error> {
    let mut request = [0_u8; 2048];
    loop {
        let size = stream.peek(&mut request).await?;
        if size == 0 {
            return Ok(None);
        }
        if let Some(end) = request[..size].windows(2).position(|pair| pair == b"\r\n") {
            let line = std::str::from_utf8(&request[..end])?;
            return Ok(Some(line.split_whitespace().nth(1).unwrap_or("").to_owned()));
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

pub(crate) async fn serve(mut stream: TcpStream, origin: String, mode: FixtureMode) -> Result<(), Error> {
    let Some(path) = request_path(&stream).await? else {
        return Ok(());
    };
    if path == "/ws/ping" {
        let mut socket = tokio_tungstenite::accept_async(stream).await?;
        while let Some(message) = socket.next().await {
            if let Message::Text(text) = message?
                && wire::decode_ping(&text) == Ok(0)
            {
                socket.send(Message::Text(wire::encode_pong(0, 0).into())).await?;
            }
        }
        return Ok(());
    }
    let _ = stream.read(&mut [0_u8; 2048]).await?;
    let body = match path.as_str() {
        "/servers" => serde_json::json!({
            "defaultSelection": ["self"],
            "servers": [{"id": "self", "url": ".", "name": "fixture"}]
        }),
        "/preflight" => match mode {
            FixtureMode::Latency => serde_json::json!({
                "generation": "fixture",
                "capabilities": {
                    "throughput": [],
                    "latency": [
                        {"baseUrl": origin.replacen("http://", "https://", 1), "transport": "webtransport"},
                        {"baseUrl": ".", "transport": "websocket"}
                    ]
                }
            }),
            FixtureMode::Throughput => serde_json::json!({
                "generation": "fixture",
                "capabilities": {
                    "throughput": [
                        {"baseUrl": "http://127.0.0.1:1", "transport": "fetch-stream", "protocol": "negotiated"},
                        {"baseUrl": "http://127.0.0.1:2", "transport": "fetch-stream", "protocol": "negotiated"},
                        {"baseUrl": origin.replacen("http://", "https://", 1), "transport": "webtransport", "protocol": "http3"}
                    ],
                    "latency": []
                }
            }),
            FixtureMode::Negotiated => serde_json::json!({
                "generation": "fixture",
                "capabilities": {
                    "throughput": [
                        {"baseUrl": ".", "transport": "fetch-stream", "protocol": "negotiated"}
                    ],
                    "latency": []
                }
            }),
        },
        "/probe" => serde_json::json!({
            "clientIp": "127.0.0.1",
            "clientIpVersion": 4,
            "clientIpSource": "socket",
            "protocolNegotiated": "http/1.1"
        }),
        _ => return Err("unexpected fixture request".into()),
    }
    .to_string();
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await?;
    Ok(())
}

/// A download-only check of the catalogue at `url`.
fn download(url: String) -> Config {
    Config {
        url,
        stages: vec![Stage::Download],
        loaded_latency: false,
        ..Config::default()
    }
}

/// A path check of `config` within the preparation timeout.
async fn check(config: &Config, http: &Http, snapshots: &watch::Sender<Snapshot>) -> Result<Preparation, Error> {
    prepare(config, http, snapshots, Instant::now() + PREPARATION_TIMEOUT).await
}

async fn fixture(mode: FixtureMode) -> Result<(String, tokio::task::JoinHandle<()>), Error> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let fixture_origin = origin.clone();
    let fixture = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let origin = fixture_origin.clone();
            tokio::spawn(async move {
                let _ = serve(stream, origin, mode).await;
            });
        }
    });
    Ok((origin, fixture))
}

async fn serve_selected(
    mut stream: TcpStream,
    origin: String,
    catalog: String,
    barrier: Arc<Barrier>,
    no_throughput: bool,
) -> Result<(), Error> {
    let Some(path) = request_path(&stream).await? else {
        return Ok(());
    };
    if path == "/preflight" {
        barrier.wait().await;
        if no_throughput {
            return serve(stream, origin, FixtureMode::Latency).await;
        }
    }
    if path == "/servers" {
        let _ = stream.read(&mut [0_u8; 2048]).await?;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{catalog}",
                    catalog.len()
                )
                .as_bytes(),
            )
            .await?;
        return Ok(());
    }
    serve(stream, origin, FixtureMode::Negotiated).await
}

#[tokio::test]
async fn selected_servers_verify_concurrently_and_report_each_result() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let first = TcpListener::bind("127.0.0.1:0").await?;
    let second = TcpListener::bind("127.0.0.1:0").await?;
    let first_origin = format!("http://{}", first.local_addr()?);
    let second_origin = format!("http://{}", second.local_addr()?);
    let catalog = serde_json::json!({
        "defaultSelection": ["self", "beta"],
        "servers": [
            {"id": "self", "url": first_origin, "name": "alpha"},
            {"id": "beta", "url": second_origin, "name": "beta"}
        ]
    })
    .to_string();
    let barrier = Arc::new(Barrier::new(2));
    let serve_listener = |listener: TcpListener, origin: String, no_throughput| {
        let catalog = catalog.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let origin = origin.clone();
                let catalog = catalog.clone();
                let barrier = barrier.clone();
                tokio::spawn(async move {
                    let _ = serve_selected(stream, origin, catalog, barrier, no_throughput).await;
                });
            }
        })
    };
    let first_server = serve_listener(first, first_origin.clone(), false);
    let second_server = serve_listener(second, second_origin, true);
    let config = download(first_origin);
    let (snapshots, _) = watch::channel(Snapshot::default());
    let result = tokio::time::timeout(Duration::from_secs(3), check(&config, &Http::new(false)?, &snapshots)).await?;
    let Ok(Preparation { servers, failures }) = result else {
        return Err("one unusable server failed the whole selection".into());
    };
    assert_eq!(servers.len(), 1);
    assert_eq!(failures.len(), 1);
    assert!(failures[0].to_string().contains("beta"), "{}", failures[0]);
    assert_eq!(
        crate::failure::reason(failures[0].source.as_ref(), true),
        graphite_meter_core::failure::FailureReason::PreparationFailed
    );
    let snapshot = snapshots.borrow();
    assert!(
        snapshot
            .servers
            .iter()
            .any(|server| { server.id == "self" && server.throughput.is_some() && server.error.is_none() })
    );
    assert!(snapshot.servers.iter().any(|server| {
        server.id == "beta"
            && server
                .error
                .as_deref()
                .is_some_and(|error| error.contains("unavailable"))
    }));
    first_server.abort();
    second_server.abort();
    Ok(())
}

#[tokio::test]
async fn a_slow_server_fails_alone_at_the_shared_deadline() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let fast = TcpListener::bind("127.0.0.1:0").await?;
    let slow = TcpListener::bind("127.0.0.1:0").await?;
    let fast_origin = format!("http://{}", fast.local_addr()?);
    let catalog = serde_json::json!({
        "defaultSelection": ["self", "slow"],
        "servers": [
            {"id": "self", "url": fast_origin, "name": "fast"},
            {"id": "slow", "url": format!("http://{}", slow.local_addr()?), "name": "slow"}
        ]
    })
    .to_string();
    let origin = fast_origin.clone();
    let server = tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            tokio::select! {
                Ok((stream, _)) = fast.accept() => {
                    let (origin, catalog) = (origin.clone(), catalog.clone());
                    tokio::spawn(serve_selected(stream, origin, catalog, Arc::new(Barrier::new(1)), false));
                }
                Ok((stream, _)) = slow.accept() => held.push(stream),
            }
        }
    });
    let config = download(fast_origin);
    let (snapshots, _) = watch::channel(Snapshot::default());
    let deadline = Instant::now() + Duration::from_secs(1);
    let Preparation { servers, failures } = prepare(&config, &Http::new(false)?, &snapshots, deadline).await?;
    server.abort();
    assert_eq!(servers.len(), 1);
    assert_eq!(failures[0].id, "slow");
    assert_eq!(
        crate::failure::reason(failures[0].source.as_ref(), true),
        graphite_meter_core::failure::FailureReason::Timeout
    );
    Ok(())
}

/// Setup and the server chooser show Go's reason, not the OS error text.
#[tokio::test]
async fn an_unreachable_server_shows_a_reason() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let reachable = TcpListener::bind("127.0.0.1:0").await?;
    let reachable_origin = format!("http://{}", reachable.local_addr()?);
    // A released port refuses connections, as a stopped server does.
    let gone = TcpListener::bind("127.0.0.1:0").await?.local_addr()?;
    let catalog = serde_json::json!({
        "defaultSelection": ["self", "gone"],
        "servers": [
            {"id": "self", "url": reachable_origin, "name": "reachable"},
            {"id": "gone", "url": format!("http://{gone}"), "name": "gone"}
        ]
    })
    .to_string();
    let origin = reachable_origin.clone();
    let server = tokio::spawn(async move {
        while let Ok((stream, _)) = reachable.accept().await {
            let (origin, catalog) = (origin.clone(), catalog.clone());
            tokio::spawn(serve_selected(
                stream,
                origin,
                catalog,
                Arc::new(Barrier::new(1)),
                false,
            ));
        }
    });
    let config = download(reachable_origin);
    let (snapshots, _) = watch::channel(Snapshot::default());
    let Preparation { failures, .. } = check(&config, &Http::new(false)?, &snapshots).await?;
    server.abort();
    assert_eq!(failures[0].id, "gone");
    let snapshot = snapshots.borrow();
    let error = snapshot
        .servers
        .iter()
        .find(|server| server.id == "gone")
        .and_then(|server| server.error.as_deref());
    assert_eq!(error, Some("Server could not be reached"));
    Ok(())
}

#[tokio::test]
async fn automatic_latency_uses_websocket_when_advertised_quic_cannot_reply() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let (origin, fixture) = fixture(FixtureMode::Latency).await?;
    let http = Http::new(false)?;
    let config = Config {
        url: origin,
        stages: vec![Stage::Latency],
        loaded_latency: false,
        ..Config::default()
    };
    let (snapshots, _) = watch::channel(Snapshot::default());
    let prepared = check(&config, &http, &snapshots).await?;
    assert_eq!(
        prepared.servers[0].latency.as_ref().unwrap().transport,
        LatencyTransport::WebSocket
    );

    let forced = Config {
        latency_transport: Some(LatencyTransport::WebTransport),
        ..config
    };
    assert!(check(&forced, &http, &snapshots).await.is_err());
    fixture.abort();
    Ok(())
}

#[tokio::test]
async fn unreachable_webtransport_preserves_ambiguous_fetch_error() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let (origin, fixture) = fixture(FixtureMode::Throughput).await?;
    let config = download(origin);
    let (snapshots, _) = watch::channel(Snapshot::default());
    let error = check(&config, &Http::new(false)?, &snapshots)
        .await
        .err()
        .ok_or("unreachable WebTransport unexpectedly passed preparation")?;
    assert!(
        error
            .to_string()
            .contains("several throughput targets are available; select an origin")
    );
    assert!(error.to_string().contains("advertised WebTransport is unavailable"));
    fixture.abort();
    Ok(())
}

/// Selecting an entry the catalogue named but the client left out says why.
#[tokio::test]
async fn selecting_a_left_out_catalogue_entry_names_its_fault() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let catalog = serde_json::json!({
        "defaultSelection": ["self"],
        "servers": [
            {"id": "self", "url": ".", "name": "self"},
            {"id": "broken", "url": "https://two words.example", "name": "broken"}
        ]
    })
    .to_string();
    let server = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let _ = stream.read(&mut [0; 2048]).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{catalog}",
                catalog.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    let config = Config {
        servers: vec!["broken".into()],
        ..download(origin)
    };
    let (snapshots, _) = watch::channel(Snapshot::default());
    let result = check(&config, &Http::new(false)?, &snapshots).await;
    server.abort();
    let error = result.err().ok_or("a left-out entry was prepared")?;
    assert_eq!(
        error.to_string(),
        "the catalogue's server \"broken\" was left out: invalid catalogue origin"
    );
    Ok(())
}

/// A reverse proxy speaks HTTP/2 to the client and HTTP/1.1 upstream, and the server reports its
/// own hop; lanes use the version the client's connection negotiated, as Go's `response.Proto` does.
#[tokio::test]
async fn negotiated_protocol_behind_a_reverse_proxy_is_the_clients_own() -> Result<(), Error> {
    use http_body_util::Full;
    use hyper::{body::Bytes, server::conn::http2, service::service_fn};
    use hyper_util::rt::{TokioExecutor, TokioIo};
    let _ = crate::crypto::provider().install_default();
    let tls = crate::fixtures::server_tls(rustls::DEFAULT_VERSIONS, &[b"h2"])?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("https://{}", listener.local_addr()?);
    let proxy = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(stream) = acceptor.accept(socket).await else {
                    return;
                };
                let service = service_fn(|request: http::Request<hyper::body::Incoming>| async move {
                    let body = match request.uri().path() {
                        "/servers" => serde_json::json!({
                            "defaultSelection": ["self"],
                            "servers": [{"id": "self", "url": ".", "name": "proxied"}]
                        }),
                        "/preflight" => serde_json::json!({
                            "generation": "proxied",
                            "capabilities": {
                                "throughput": [{"baseUrl": ".", "transport": "fetch-stream", "protocol": "negotiated"}],
                                "latency": []
                            }
                        }),
                        // The upstream hop behind the proxy.
                        _ => serde_json::json!({
                            "clientIp": "127.0.0.1",
                            "clientIpVersion": 4,
                            "clientIpSource": "forwarded",
                            "protocolNegotiated": "http/1.1"
                        }),
                    };
                    Ok::<_, std::convert::Infallible>(http::Response::new(Full::new(Bytes::from(body.to_string()))))
                });
                let _ = http2::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    let config = Config {
        insecure: true,
        ..download(origin)
    };
    let (snapshots, _) = watch::channel(Snapshot::default());
    let prepared = check(&config, &Http::new(true)?, &snapshots).await;
    proxy.abort();
    assert_eq!(
        prepared?.servers[0]
            .throughput
            .as_ref()
            .ok_or("no throughput path")?
            .protocol,
        Protocol::Http2
    );
    Ok(())
}

#[tokio::test]
async fn negotiated_fetch_protocol_uses_verified_http_version() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let (origin, fixture) = fixture(FixtureMode::Negotiated).await?;
    let config = download(origin);
    let (snapshots, _) = watch::channel(Snapshot::default());
    let prepared = check(&config, &Http::new(false)?, &snapshots).await?;
    assert_eq!(
        prepared.servers[0].throughput.as_ref().unwrap().protocol,
        Protocol::Http1
    );
    fixture.abort();
    Ok(())
}
