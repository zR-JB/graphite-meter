use super::*;
use futures_util::{SinkExt, StreamExt};
use graphite_meter_core::wire;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone, Copy)]
enum FixtureMode {
    Latency,
    Throughput,
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
