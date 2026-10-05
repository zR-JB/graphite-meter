//! The WebSocket bus: handshakes, PONGs and the close code of every ending.

use super::*;
use futures_util::{SinkExt, StreamExt};
use graphite_meter_proto::bus::Pong;
use tokio::time::Instant;
use tokio_tungstenite::{
    WebSocketStream, client_async,
    tungstenite::{
        Message,
        protocol::{CloseFrame, frame::coding::CloseCode},
    },
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

/// The close code and reason the server ends the bus with.
async fn closed(socket: &mut WebSocketStream<TcpStream>) -> (u16, String) {
    match socket.next().await {
        Some(Ok(Message::Close(Some(frame)))) => (frame.code.into(), frame.reason.to_string()),
        other => panic!("{other:?}"),
    }
}

/// RFC 6455's sample handshake nonce.
const NONCE: &str = "dGhlIHNhbXBsZSBub25jZQ==";

#[tokio::test]
async fn a_bus_answers_text_and_binary_pings_until_the_peer_closes() {
    let server = start(&[]).await;
    let mut socket = bus(&server).await;
    assert_eq!(ping(&mut socket, Message::text("PING,7")).await, 7);
    assert_eq!(ping(&mut socket, Message::binary(&b"PING,4294967295"[..])).await, u32::MAX);
    assert_eq!(server.active().await, 1, "the bus holds a handler");
    socket
        .close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() }))
        .await
        .unwrap();
    assert_eq!(closed(&mut socket).await.0, 1000, "the server answers the peer's close");
    assert!(socket.next().await.is_none());
    server.until_active(0).await;
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
    // Paused time may run ahead while the close frame is in flight; the contract allows 30–45 s.
    assert!(elapsed > Duration::from_secs(29) && elapsed <= Duration::from_secs(45), "{elapsed:?}");
}

#[tokio::test]
async fn a_bus_closes_at_the_operation_lifetime() {
    let server = start(&[("GM_MAX_OPERATION_DURATION", "1s")]).await;
    let mut socket = bus(&server).await;
    assert_eq!(closed(&mut socket).await, (4002, "lifetime".into()));
}

#[tokio::test]
async fn shutdown_closes_buses_with_1001_while_the_listener_refuses_connections() {
    let server = start(&[]).await;
    let address = server.address;
    let mut socket = bus(&server).await;
    assert_eq!(ping(&mut socket, Message::text("PING,2")).await, 2);
    let stopping = server.stop();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!stopping.is_finished(), "the bus drains through its close handshake");
    let refused = TcpStream::connect(address).await.unwrap_err();
    assert_eq!(refused.kind(), std::io::ErrorKind::ConnectionRefused);
    drop(
        tokio::net::TcpListener::bind(address)
            .await
            .expect("the address is free during the drain"),
    );
    assert_eq!(closed(&mut socket).await, (1001, "shutdown".into()));
    assert!(socket.next().await.is_none());
    stopping.await.unwrap().unwrap();
}

#[tokio::test]
async fn handshakes_refuse_head_and_http_1_0_with_the_headers_http_requires() {
    let server = start(&[]).await;
    let upgrade = format!(
        "Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {NONCE}\r\n\r\n"
    );
    let mut client = server.connect().await;
    client
        .send(&format!("HEAD /ws/ping HTTP/1.1\r\nHost: test\r\n{upgrade}"))
        .await;
    let answer = client.answer().await.unwrap();
    assert_eq!((answer.status, answer.header("allow")), (405, Some("GET")));
    let mut client = server.connect().await;
    client
        .send(&format!("GET /ws/ping HTTP/1.0\r\nHost: test\r\n{upgrade}"))
        .await;
    let answer = client.answer().await.unwrap();
    assert_eq!((answer.status, answer.header("upgrade")), (426, Some("websocket")));
    server.until_active(0).await;
}
