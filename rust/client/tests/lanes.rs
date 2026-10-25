//! Lanes and buses against canned peers, for what the real server never does or does only after sign-in.
use futures_util::SinkExt;
use graphite_meter_client::{
    model::{Dir, LaneHealth, Stage},
    net::{Client, Fault, Lanes, LatencyPath, ThroughputPath, Work, topology},
};
use graphite_meter_net::Pool;
use graphite_meter_proto::{
    bus::Pong,
    discovery::{LatencyTransport, Protocol, ThroughputTransport},
    origin::Origin,
};
use graphite_meter_testkit::read_head;
use std::{assert_matches, sync::Arc, time::Duration};
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

/// Answers downloads only once an upload arrived, and reads uploads until they end; says when each arrived.
async fn bidirectional(
    mut stream: TcpStream,
    uploading: watch::Sender<bool>,
    downloading: watch::Sender<bool>,
) {
    let Some(head) = read_head(&mut stream).await else {
        return;
    };
    if head.starts_with("POST /upload?") {
        uploading.send_replace(true);
        let mut buffer = vec![0; 64 << 10];
        while stream.read(&mut buffer).await.is_ok_and(|read| read > 0) {}
    } else if head.starts_with("GET /download?") {
        downloading.send_replace(true);
        let _ = uploading.subscribe().wait_for(|uploading| *uploading).await;
        let answer = b"HTTP/1.1 200 OK\r\ncontent-length: 68719476736\r\n\r\n";
        if stream.write_all(answer).await.is_ok() && stream.write_all(&[0; 1024]).await.is_ok() {
            std::future::pending::<()>().await;
        }
    }
}

#[tokio::test]
async fn bidirectional_upload_lanes_start_while_downloads_wait_for_headers() {
    let ((listener, origin), uploading, downloading) = (
        local().await,
        watch::Sender::new(false),
        watch::Sender::new(false),
    );
    let (mut uploaded, mut downloaded) = (uploading.subscribe(), downloading.subscribe());
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(bidirectional(
                stream,
                uploading.clone(),
                downloading.clone(),
            ));
        }
    });
    let path = ThroughputPath {
        origin,
        transport: ThroughputTransport::FetchStream,
        protocol: Protocol::Http1,
    };
    let plans = topology(&path, Stage::Bidirectional, Dir { down: 1, up: 1 });
    let (client, token) = (client(), CancellationToken::new());
    let download = Lanes::start(
        &client,
        plans.clone(),
        Work::Download,
        Duration::ZERO,
        token.clone(),
    );
    downloaded.wait_for(|down| *down).await.unwrap();
    assert!(!download.ready(), "the download waits for its headers");
    let _upload = Lanes::start(
        &client,
        plans,
        Work::Upload("fixture".into()),
        Duration::ZERO,
        token.clone(),
    );
    let started = tokio::time::timeout(Duration::from_secs(5), uploaded.wait_for(|up| *up)).await;
    assert!(
        started.is_ok(),
        "upload lanes waited for download readiness"
    );
    let answered = async {
        while !(download.ready() && download.bytes() == 1024) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), answered)
        .await
        .unwrap();
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
    assert_eq!(
        bus.next().await.unwrap().0,
        Pong {
            id: 7,
            handling_nanos: 42
        }
    );
    let fault = bus.next().await.unwrap_err();
    assert_matches!(&fault, Fault::SignIn(server) if *server == origin);
}

#[tokio::test(start_paused = true)]
async fn a_download_lane_whose_answer_never_comes_fails_its_open_in_ten_seconds() {
    let (listener, origin) = local().await;
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                read_head(&mut stream).await;
                std::future::pending::<()>().await;
            });
        }
    });
    let path = ThroughputPath {
        origin,
        transport: ThroughputTransport::FetchStream,
        protocol: Protocol::Http1,
    };
    let plans = topology(&path, Stage::Download, Dir { down: 1, up: 0 });
    let (client, token) = (client(), CancellationToken::new());
    let mut lanes = Lanes::start(
        &client,
        plans,
        Work::Download,
        Duration::ZERO,
        token.clone(),
    );
    let started = tokio::time::Instant::now();
    while lanes.health() == LaneHealth::Ok && started.elapsed() < Duration::from_secs(60) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let waited = started.elapsed();
    assert!(
        (Duration::from_secs(10)..Duration::from_secs(11)).contains(&waited),
        "{waited:?} {:?}",
        lanes.health()
    );
    token.cancel();
}
