use super::*;
use futures_util::{SinkExt, StreamExt};
use graphite_meter_core::wire;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone, Copy)]
pub(crate) enum FixtureMode {
    Latency,
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

async fn serve_selected(mut stream: TcpStream, origin: String, catalog: String) -> Result<(), Error> {
    let Some(path) = request_path(&stream).await? else {
        return Ok(());
    };
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
                    tokio::spawn(serve_selected(stream, origin, catalog));
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
