//! Tests the WebSocket bus, its PONGs and the close codes of its endings.

use super::*;
use futures_util::{SinkExt, StreamExt};
use graphite_meter_proto::{bus::Pong, upload::Record};
use tokio::time::Instant;
use tokio_tungstenite::{
    WebSocketStream, client_async,
    tungstenite::{Message, protocol::frame::coding::CloseCode},
};

async fn bus(server: &Running) -> WebSocketStream<TcpStream> {
    let stream = TcpStream::connect(server.address).await.unwrap();
    let (socket, answer) = client_async(format!("ws://{}/ws/ping", server.address), stream)
        .await
        .unwrap();
    assert_eq!(answer.status(), 101);
    socket
}

async fn ping(socket: &mut WebSocketStream<TcpStream>, message: Message) -> u32 {
    socket.send(message).await.unwrap();
    let pong = socket.next().await.unwrap().unwrap();
    Pong::decode(pong.to_text().unwrap().as_bytes()).unwrap().id
}

async fn closed(socket: &mut WebSocketStream<TcpStream>) -> (u16, String) {
    match socket.next().await {
        Some(Ok(Message::Close(Some(frame)))) => (frame.code.into(), frame.reason.to_string()),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_message_over_two_kibibytes_ends_the_bus_with_1009() {
    let server = start(&[]).await;
    let mut socket = bus(&server).await;
    socket.send(Message::text("P".repeat(2049))).await.unwrap();
    assert_eq!(closed(&mut socket).await.0, u16::from(CloseCode::Size));
}

#[tokio::test]
async fn a_quiet_bus_closes_idle_within_the_contract_bound() {
    let server = start(&[]).await;
    let mut socket = bus(&server).await;
    assert_eq!(ping(&mut socket, Message::text("PING,1")).await, 1);
    tokio::time::pause();
    let started = Instant::now();
    assert_eq!(closed(&mut socket).await, (4001, "idle".into()));
    let elapsed = started.elapsed();
    // Paused time may run ahead while the close frame is in flight. The contract allows 30 to 45 s.
    assert!(
        elapsed > Duration::from_secs(29) && elapsed <= Duration::from_secs(45),
        "{elapsed:?}"
    );
}

#[tokio::test]
async fn a_bus_closes_at_the_operation_lifetime() {
    let server = start(&[("GM_MAX_OPERATION_DURATION", "1s")]).await;
    let mut socket = bus(&server).await;
    tokio::time::pause();
    assert_eq!(closed(&mut socket).await, (4002, "lifetime".into()));
}

async fn feed(server: &Running, id: &str) -> WebSocketStream<TcpStream> {
    let stream = TcpStream::connect(server.address).await.unwrap();
    let url = format!("ws://{}/ws/progress?id={id}", server.address);
    let (socket, answer) = client_async(url, stream).await.unwrap();
    assert_eq!(answer.status(), 101);
    socket
}

async fn record(socket: &mut WebSocketStream<TcpStream>) -> Record {
    loop {
        let text = socket.next().await.unwrap().unwrap().into_text().unwrap();
        if !text.is_empty() {
            return Record::decode(text.as_bytes()).unwrap();
        }
    }
}

#[tokio::test]
async fn a_progress_socket_carries_the_feed_from_ready_to_complete_and_a_refusal_alone() {
    let server = start(&[]).await;
    let id = server.upload_id().await;
    let mut socket = feed(&server, &id).await;
    assert_eq!(record(&mut socket).await, Record::Ready);
    let mut client = server.connect().await;
    let answer = client
        .request(&format!("POST /upload?id={id}"), &"x".repeat(500))
        .await;
    assert_eq!(answer.body, br#"{"bytes":500}"#);
    let progressed = loop {
        if let Record::Progress(counters) = record(&mut socket).await {
            break counters;
        }
    };
    assert_eq!(progressed.bytes(), 500);
    let finalized = client
        .request(&format!("DELETE /upload/progress?id={id}"), "")
        .await;
    assert_eq!(finalized.status, 204);
    match record(&mut socket).await {
        Record::Complete(counters) => assert_eq!(counters.bytes(), 500),
        other => panic!("a complete record, not {other:?}"),
    }
    assert_eq!(closed(&mut socket).await.0, u16::from(CloseCode::Normal));

    let mut refused = feed(&server, "forged").await;
    assert!(matches!(record(&mut refused).await, Record::Error { code, .. } if code == "invalid"));
}

#[tokio::test]
async fn a_progress_socket_at_capacity_still_upgrades_to_say_so() {
    let server = start(&[
        ("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "1"),
        ("GM_MAX_SESSIONS_PER_CLIENT", "1"),
    ])
    .await;
    let id = server.upload_id().await;
    let mut held = server.connect().await;
    held.send(&format!("GET {ENDLESS} HTTP/1.1\r\nHost: test\r\n\r\n"))
        .await;
    server.until_active(1).await;
    let mut socket = feed(&server, &id).await;
    assert!(
        matches!(record(&mut socket).await, Record::Error { code, .. } if code == "client-limit")
    );
    assert_eq!(closed(&mut socket).await.0, u16::from(CloseCode::Normal));
}
