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
enum FixtureMode {
    Latency,
    Throughput,
    Negotiated,
}

async fn serve(mut stream: TcpStream, origin: String, mode: FixtureMode) -> Result<(), Error> {
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
                socket
                    .send(Message::Text(wire::encode_pong(0, 0).into()))
                    .await?;
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
    refuse_preflight: bool,
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
        if refuse_preflight {
            stream
                .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await?;
            return Ok(());
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
    let serve_listener = |listener: TcpListener, origin: String, refuse_preflight| {
        let catalog = catalog.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let origin = origin.clone();
                let catalog = catalog.clone();
                let barrier = barrier.clone();
                tokio::spawn(async move {
                    let _ =
                        serve_selected(stream, origin, catalog, barrier, refuse_preflight).await;
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
        prepare(&config, &Http::new(false)?, &snapshots),
    )
    .await?;
    let error = result.err().ok_or("refused preflight was accepted")?;
    assert!(error.to_string().contains("beta"), "{error}");
    let snapshot = snapshots.borrow();
    assert!(snapshot.servers.iter().any(|server| {
        server.id == "self" && server.throughput.is_some() && server.error.is_none()
    }));
    assert!(snapshot.servers.iter().any(|server| {
        server.id == "beta"
            && server
                .error
                .as_deref()
                .is_some_and(|error| error.contains("503"))
    }));
    first_server.abort();
    second_server.abort();
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
    let prepared = prepare(&config, &http, &snapshots).await?;
    assert_eq!(
        prepared[0].latency.as_ref().unwrap().transport,
        LatencyTransport::WebSocket
    );

    let forced = Config {
        latency_transport: Some(LatencyTransport::WebTransport),
        ..config
    };
    assert!(prepare(&forced, &http, &snapshots).await.is_err());
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
    let error = prepare(&config, &Http::new(false)?, &snapshots)
        .await
        .err()
        .ok_or("unreachable WebTransport unexpectedly passed preparation")?;
    assert!(error.to_string().contains("select an origin explicitly"));
    assert!(
        error
            .to_string()
            .contains("advertised WebTransport is unavailable")
    );
    fixture.abort();
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
    let prepared = prepare(&config, &Http::new(false)?, &snapshots).await?;
    assert_eq!(
        prepared[0].throughput.as_ref().unwrap().protocol,
        Protocol::Http1
    );
    fixture.abort();
    Ok(())
}
