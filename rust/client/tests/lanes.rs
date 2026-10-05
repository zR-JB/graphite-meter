//! Lanes and buses against canned peers, for what the real server never does or does only after sign-in.
use futures_util::SinkExt;
use graphite_meter_client::{
    model::{Dir, Stage},
    net::{Client, Fault, Lanes, LatencyPath, ThroughputPath, Work, topology},
};
use graphite_meter_net::Pool;
use graphite_meter_proto::{
    bus::Pong,
    discovery::{LatencyTransport, Protocol, ThroughputTransport},
    origin::Origin,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
};
use tokio_tungstenite::tungstenite::{
    Message,
    protocol::{CloseFrame, frame::coding::CloseCode},
};
use tokio_util::sync::CancellationToken;

fn client() -> Client {
    Client::new(false, Arc::new(Pool::inline()))
}

async fn local() -> (TcpListener, Origin) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = Origin::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    (listener, origin)
}

async fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(stream.read_u8().await.ok()?);
    }
    String::from_utf8(head).ok()
}

/// Answers downloads only once an upload arrived, and reads uploads until they end.
async fn bidirectional(mut stream: TcpStream, uploading: watch::Sender<bool>) {
    let Some(head) = read_head(&mut stream).await else { return };
    if head.starts_with("POST /upload?") {
        uploading.send_replace(true);
        let mut buffer = vec![0; 64 << 10];
        while stream.read(&mut buffer).await.is_ok_and(|read| read > 0) {}
    } else if head.starts_with("GET /download?") {
        let _ = uploading.subscribe().wait_for(|uploading| *uploading).await;
        let answer = b"HTTP/1.1 200 OK\r\ncontent-length: 68719476736\r\n\r\n";
        if stream.write_all(answer).await.is_ok() && stream.write_all(&[0; 1024]).await.is_ok() {
            std::future::pending::<()>().await;
        }
    }
}

#[tokio::test]
async fn bidirectional_upload_lanes_start_while_downloads_wait_for_headers() {
    let ((listener, origin), uploading) = (local().await, watch::Sender::new(false));
    let mut uploaded = uploading.subscribe();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(bidirectional(stream, uploading.clone()));
        }
    });
    let path = ThroughputPath {
        origin,
        transport: ThroughputTransport::FetchStream,
        protocol: Protocol::Http1,
    };
    let plans = topology(&path, Stage::Bidirectional, Dir { down: 1, up: 1 });
    let (client, token) = (client(), CancellationToken::new());
    let download = Lanes::start(&client, plans.clone(), Work::Download, Duration::ZERO, token.clone());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!download.ready(), "the download waits for its headers");
    let _upload = Lanes::start(&client, plans, Work::Upload("fixture".into()), Duration::ZERO, token.clone());
    let started = tokio::time::timeout(Duration::from_secs(5), uploaded.wait_for(|up| *up)).await;
    assert!(started.is_ok(), "upload lanes waited for download readiness");
    let answered = async {
        while !(download.ready() && download.bytes() == 1024) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), answered).await.unwrap();
    token.cancel();
}

#[tokio::test]
async fn a_websocket_bus_closed_as_revoked_asks_its_server_for_sign_in() {
    let (listener, origin) = local().await;
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.send(Message::text("PONG,7,42")).await.unwrap();
        let revoked = CloseFrame {
            code: CloseCode::Policy,
            reason: "authentication required".into(),
        };
        socket.close(Some(revoked)).await.unwrap();
    });
    let path = LatencyPath {
        origin: origin.clone(),
        transport: LatencyTransport::WebSocket,
    };
    let mut bus = client().bus(&path).await.unwrap();
    assert_eq!(bus.next().await.unwrap(), Pong { id: 7, handling_nanos: 42 });
    let fault = bus.next().await.unwrap_err();
    assert!(matches!(&fault, Fault::SignIn(server) if *server == origin), "{fault:?}");
}
