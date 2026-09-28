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

pub(crate) async fn serve(mut stream: TcpStream, origin: String, mode: FixtureMode) -> Result<(), Error> {
    let mut request = [0_u8; 2048];
    let path = loop {
        let size = stream.peek(&mut request).await?;
        if size == 0 {
            return Ok(());
        }
        if let Some(line_end) = request[..size].windows(2).position(|pair| pair == b"\r\n") {
            let line = std::str::from_utf8(&request[..line_end])?;
            break line.split_whitespace().nth(1).unwrap_or("").to_owned();
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
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
    let _ = stream.read(&mut request).await?;
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
    let mut request = [0_u8; 2048];
    let path = loop {
        let size = stream.peek(&mut request).await?;
        if size == 0 {
            return Ok(());
        }
        if let Some(end) = request[..size].windows(2).position(|pair| pair == b"\r\n") {
            break std::str::from_utf8(&request[..end])?
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .to_owned();
        }
        tokio::task::yield_now().await;
    };
    if path == "/preflight" {
        barrier.wait().await;
        if no_throughput {
            return serve(stream, origin, FixtureMode::Latency).await;
        }
    }
    if path == "/servers" {
        let _ = stream.read(&mut request).await?;
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
    let config = Config {
        url: first_origin,
        stages: vec![Stage::Download],
        loaded_latency: false,
        ..Config::default()
    };
    let (snapshots, _) = watch::channel(Snapshot::default());
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        prepare(
            &config,
            &Http::new(false)?,
            &snapshots,
            Instant::now() + PREPARATION_TIMEOUT,
        ),
    )
    .await?;
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
                .is_some_and(|error| error.contains("not advertised"))
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
    let config = Config {
        url: fast_origin,
        stages: vec![Stage::Download],
        loaded_latency: false,
        ..Config::default()
    };
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
    let config = Config {
        url: reachable_origin,
        stages: vec![Stage::Download],
        loaded_latency: false,
        ..Config::default()
    };
    let (snapshots, _) = watch::channel(Snapshot::default());
    let deadline = Instant::now() + PREPARATION_TIMEOUT;
    let Preparation { failures, .. } = prepare(&config, &Http::new(false)?, &snapshots, deadline).await?;
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
    let prepared = prepare(&config, &http, &snapshots, Instant::now() + PREPARATION_TIMEOUT).await?;
    assert_eq!(
        prepared.servers[0].latency.as_ref().unwrap().transport,
        LatencyTransport::WebSocket
    );

    let forced = Config {
        latency_transport: Some(LatencyTransport::WebTransport),
        ..config
    };
    assert!(
        prepare(&forced, &http, &snapshots, Instant::now() + PREPARATION_TIMEOUT)
            .await
            .is_err()
    );
    fixture.abort();
    Ok(())
}

#[tokio::test]
async fn unreachable_webtransport_preserves_ambiguous_fetch_error() -> Result<(), Error> {
    let _ = crate::crypto::provider().install_default();
    let (origin, fixture) = fixture(FixtureMode::Throughput).await?;
    let config = Config {
        url: origin,
        stages: vec![Stage::Download],
        loaded_latency: false,
        ..Config::default()
    };
    let (snapshots, _) = watch::channel(Snapshot::default());
    let error = prepare(
        &config,
        &Http::new(false)?,
        &snapshots,
        Instant::now() + PREPARATION_TIMEOUT,
    )
    .await
    .err()
    .ok_or("unreachable WebTransport unexpectedly passed preparation")?;
    assert!(error.to_string().contains("select an origin explicitly"));
    assert!(error.to_string().contains("advertised WebTransport is unavailable"));
    fixture.abort();
    Ok(())
}

/// A reverse proxy speaks HTTP/2 to the client and HTTP/1.1 upstream, and the server reports its
/// own hop; lanes use the version the client's connection negotiated, as Go's `response.Proto` does.
#[tokio::test]
async fn negotiated_protocol_behind_a_reverse_proxy_is_the_clients_own() -> Result<(), Error> {
    use http_body_util::Full;
    use hyper::{body::Bytes, server::conn::http2, service::service_fn};
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
    let _ = crate::crypto::provider().install_default();
    let (certificate, key) = crate::test_identity::generate_identity("localhost")?;
    let mut tls = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(
        vec![CertificateDer::from_pem_slice(certificate.as_bytes())?],
        PrivateKeyDer::from_pem_slice(key.as_bytes())?,
    )?;
    tls.alpn_protocols = vec![b"h2".to_vec()];
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
        url: origin,
        stages: vec![Stage::Download],
        loaded_latency: false,
        insecure: true,
        ..Config::default()
    };
    let (snapshots, _) = watch::channel(Snapshot::default());
    let prepared = prepare(
        &config,
        &Http::new(true)?,
        &snapshots,
        Instant::now() + PREPARATION_TIMEOUT,
    )
    .await;
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
    let config = Config {
        url: origin,
        stages: vec![Stage::Download],
        loaded_latency: false,
        ..Config::default()
    };
    let (snapshots, _) = watch::channel(Snapshot::default());
    let prepared = prepare(
        &config,
        &Http::new(false)?,
        &snapshots,
        Instant::now() + PREPARATION_TIMEOUT,
    )
    .await?;
    assert_eq!(
        prepared.servers[0].throughput.as_ref().unwrap().protocol,
        Protocol::Http1
    );
    fixture.abort();
    Ok(())
}
