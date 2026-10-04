use super::*;
use futures_util::{SinkExt, StreamExt};
use graphite_meter_core::wire;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone, Copy)]
enum FixtureMode {
    Latency,
    Negotiated,
}

/// The path of the request `stream` carries, read without taking it; None once the stream closed.
async fn request_path(stream: &TcpStream) -> Result<Option<String>, Error> {
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

/// The fixture's JSON for `path` at `origin`; None for a route it does not serve.
fn answer(path: &str, origin: &str, mode: FixtureMode) -> Option<Value> {
    Some(match (path, mode) {
        ("/servers", _) => {
            json!({"defaultSelection": ["self"], "servers": [{"id": "self", "url": ".", "name": "fixture"}]})
        }
        ("/preflight", FixtureMode::Latency) => json!({
            "generation": "fixture",
            "capabilities": {
                "throughput": [],
                "latency": [
                    {"baseUrl": origin.replacen("http://", "https://", 1), "transport": "webtransport"},
                    {"baseUrl": ".", "transport": "websocket"}
                ]
            }
        }),
        ("/preflight", FixtureMode::Negotiated) => json!({
            "generation": "fixture",
            "capabilities": {
                "throughput": [{"baseUrl": ".", "transport": "fetch-stream", "protocol": "negotiated"}],
                "latency": []
            }
        }),
        // The server reports its own hop, which may differ from the client's.
        ("/probe", _) => json!({
            "clientIp": "127.0.0.1", "clientIpVersion": 4, "clientIpSource": "socket", "protocolNegotiated": "http/1.1"
        }),
        _ => return None,
    })
}

/// Answers the one request `stream` carries with `body`, then closes.
async fn respond(mut stream: TcpStream, body: &str) -> Result<(), Error> {
    let _ = stream.read(&mut [0_u8; 2048]).await?;
    let length = body.len();
    let head =
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {length}\r\nConnection: close");
    stream.write_all(format!("{head}\r\n\r\n{body}").as_bytes()).await?;
    Ok(())
}

async fn serve(stream: TcpStream, origin: String, mode: FixtureMode) -> Result<(), Error> {
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
    let body = answer(&path, &origin, mode).ok_or("unexpected fixture request")?;
    respond(stream, &body.to_string()).await
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

#[tokio::test]
async fn a_slow_server_fails_alone_at_the_shared_deadline() -> Result<(), Error> {
    let (fast, fast_origin) = crate::fixtures::listener().await?;
    let slow = TcpListener::bind("127.0.0.1:0").await?;
    let catalog = json!({
        "defaultSelection": ["self", "slow"],
        "servers": [
            {"id": "self", "url": fast_origin, "name": "fast"},
            {"id": "slow", "url": format!("http://{}", slow.local_addr()?), "name": "slow"}
        ]
    })
    .to_string();
    let origin = fast_origin.clone();
    // The slow server's listener stays bound and never accepts, so its requests wait unanswered.
    let server = tokio::spawn(async move {
        while let Ok((stream, _)) = fast.accept().await {
            let (origin, catalog) = (origin.clone(), catalog.clone());
            tokio::spawn(async move {
                match request_path(&stream).await {
                    Ok(Some(path)) if path == "/servers" => respond(stream, &catalog).await,
                    _ => serve(stream, origin, FixtureMode::Negotiated).await,
                }
            });
        }
    });
    let config = download(fast_origin);
    let (snapshots, _) = watch::channel(Snapshot::default());
    let deadline = Instant::now() + Duration::from_secs(1);
    let (servers, failures) = prepare(&config, &Http::new(false)?, &snapshots, deadline).await?;
    server.abort();
    assert_eq!(servers.len(), 1);
    assert_eq!(failures[0].id, "slow");
    let reason = crate::failure::reason(failures[0].source.as_ref(), true);
    assert_eq!(reason, graphite_meter_core::failure::FailureReason::Timeout);
    Ok(())
}

#[tokio::test]
async fn automatic_latency_uses_websocket_when_advertised_quic_cannot_reply() -> Result<(), Error> {
    let (listener, origin) = crate::fixtures::listener().await?;
    let fixture_origin = origin.clone();
    let fixture = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve(stream, fixture_origin.clone(), FixtureMode::Latency));
        }
    });
    let http = Http::new(false)?;
    let config = Config { stages: vec![Stage::Latency], ..download(origin) };
    let (snapshots, _) = watch::channel(Snapshot::default());
    let prepared = prepare_run(&config, &http, &snapshots).await?;
    let latency = prepared.servers[0].latency.as_ref().ok_or("no latency path")?;
    assert_eq!(latency.transport, LatencyTransport::WebSocket);

    let forced = Config {
        latency_transport: Some(LatencyTransport::WebTransport),
        ..config
    };
    assert!(prepare_run(&forced, &http, &snapshots).await.is_err());
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
                    let body = answer(request.uri().path(), "", FixtureMode::Negotiated).unwrap_or_default();
                    Ok::<_, std::convert::Infallible>(http::Response::new(Full::new(Bytes::from(body.to_string()))))
                });
                let _ = http2::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    let config = Config { insecure: true, ..download(origin) };
    let (snapshots, _) = watch::channel(Snapshot::default());
    let prepared = prepare_run(&config, &Http::new(true)?, &snapshots).await;
    proxy.abort();
    let throughput = prepared?.servers.remove(0).throughput.ok_or("no throughput path")?;
    assert_eq!(throughput.protocol, Protocol::Http2);
    Ok(())
}
