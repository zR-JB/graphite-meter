#[path = "support/native.rs"]
mod native;

use futures_util::{SinkExt, StreamExt};
use graphite_meter_core::wire::decode_pong;
use graphite_meter_server::websocket::{CloseReason, handshake, serve_ping};
use std::{error::Error, time::Duration};
use tokio::{io::DuplexStream, sync::oneshot, task::JoinHandle};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        protocol::{
            Role,
            frame::{
                Frame,
                coding::{CloseCode, Data, OpCode},
            },
        },
    },
};

type TestError = Box<dyn Error + Send + Sync>;
// RFC 6455 example nonce.
const NONCE: &str = "dGhlIHNhbXBsZSBub25jZQ==";

#[test]
fn upgrade_validates_origin_and_key_without_negotiating_compression() {
    use http::{Request, StatusCode, header};
    let mut request = Request::builder()
        .uri("/ws/ping")
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(header::SEC_WEBSOCKET_KEY, NONCE)
        .header(header::SEC_WEBSOCKET_EXTENSIONS, "permessage-deflate")
        .header(header::ORIGIN, "https://meter.example")
        .body(())
        .unwrap();
    let response = handshake(&request, Some("https://meter.example"));
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    let headers = response.headers();
    assert_eq!(headers[header::SEC_WEBSOCKET_ACCEPT], "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    assert!(!headers.contains_key(header::SEC_WEBSOCKET_EXTENSIONS));
    let foreign = handshake(&request, Some("https://other.example"));
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);
    assert_eq!(handshake(&request, None).status(), StatusCode::SWITCHING_PROTOCOLS);
    let headers = request.headers_mut();
    headers.insert(header::CONNECTION, "keep-alive".parse().unwrap());
    headers.append(header::CONNECTION, "UpGrAdE".parse().unwrap());
    headers.insert(header::UPGRADE, "other, WebSocket".parse().unwrap());
    assert_eq!(handshake(&request, None).status(), StatusCode::SWITCHING_PROTOCOLS);
    request
        .headers_mut()
        .append(header::SEC_WEBSOCKET_KEY, NONCE.parse().unwrap());
    assert_eq!(handshake(&request, None).status(), StatusCode::BAD_REQUEST);
}

#[test]
fn upgrade_checks_run_in_go_s_order() {
    use http::{Method, Request, StatusCode, header};
    // As Go's library, the upgrade tokens come before the method, and the method before the version and key.
    let request = |method: Method, upgrade: bool| {
        let mut request = Request::builder().method(method).uri("/ws/ping");
        if upgrade {
            request = request
                .header(header::CONNECTION, "Upgrade")
                .header(header::UPGRADE, "websocket");
        }
        request
    };
    let response = handshake(&request(Method::HEAD, false).body(()).unwrap(), None);
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    assert_eq!(response.headers()[header::UPGRADE], "websocket");
    let response = handshake(&request(Method::HEAD, true).body(()).unwrap(), None);
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let repeated = request(Method::GET, true)
        .header(header::SEC_WEBSOCKET_KEY, NONCE)
        .header(header::SEC_WEBSOCKET_KEY, NONCE)
        .body(())
        .unwrap();
    let response = handshake(&repeated, None);
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response.headers()[header::SEC_WEBSOCKET_VERSION], "13");
}

type Session = (
    WebSocketStream<DuplexStream>,
    oneshot::Sender<CloseReason>,
    JoinHandle<()>,
);

/// A ping session over a pipe that buffers `buffer` bytes, which lives `lifetime` or until its sender stops it.
async fn session_with(buffer: usize, lifetime: Duration) -> Session {
    let (client, server) = tokio::io::duplex(buffer);
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(serve_ping(server, tokio::time::Instant::now() + lifetime, async {
        stopped.await.unwrap_or(CloseReason::Finished)
    }));
    (
        WebSocketStream::from_raw_socket(client, Role::Client, None).await,
        stop,
        task,
    )
}

async fn session() -> Session {
    session_with(8192, Duration::from_secs(120)).await
}

async fn receive(socket: &mut WebSocketStream<DuplexStream>) -> Result<Message, TestError> {
    Ok(tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await?
        .ok_or("connection ended without a frame")??)
}

#[tokio::test]
async fn ping_accepts_text_binary_and_fragmented_messages() -> Result<(), TestError> {
    let (mut socket, stop, task) = session().await;
    for message in [
        Message::Text("invalid".into()),
        Message::Binary(vec![255].into()),
        Message::Text("PING,7".into()),
        Message::Binary(b"PING,8".to_vec().into()),
        Message::Frame(Frame::message(b"PING,".to_vec(), OpCode::Data(Data::Text), false)),
        Message::Frame(Frame::message(b"9".to_vec(), OpCode::Data(Data::Continue), true)),
    ] {
        socket.send(message).await?;
    }
    for id in [7, 8, 9] {
        let message = receive(&mut socket).await?;
        assert!(message.is_text());
        assert_eq!(decode_pong(message.to_text()?)?.id, id);
    }
    socket.send(Message::Ping(b"control".to_vec().into())).await?;
    assert_eq!(receive(&mut socket).await?, Message::Pong(b"control".to_vec().into()));
    stop.send(CloseReason::Finished).unwrap();
    let Message::Close(Some(frame)) = receive(&mut socket).await? else {
        panic!("expected close frame");
    };
    assert_eq!(frame.code, CloseCode::Normal);
    task.await?;
    Ok(())
}

#[tokio::test]
async fn oversized_messages_and_revocation_send_distinct_close_codes() -> Result<(), TestError> {
    let (mut socket, _stop, task) = session().await;
    socket.send(Message::Text("a".repeat(2049).into())).await?;
    let Message::Close(Some(frame)) = receive(&mut socket).await? else {
        panic!("expected size refusal");
    };
    assert_eq!(frame.code, CloseCode::Size);
    task.await?;

    let (mut socket, stop, task) = session().await;
    stop.send(CloseReason::Revoked).unwrap();
    let Message::Close(Some(frame)) = receive(&mut socket).await? else {
        panic!("expected authentication close");
    };
    assert_eq!(frame.code, CloseCode::Policy);
    assert_eq!(frame.reason, "authentication required");
    task.await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn control_frames_do_not_extend_the_idle_bound() -> Result<(), TestError> {
    let (mut socket, _stop, task) = session().await;
    socket.send(Message::Text("PING,1".into())).await?;
    assert!(receive(&mut socket).await?.is_text());
    tokio::time::advance(Duration::from_secs(20)).await;
    socket.send(Message::Ping(Vec::new().into())).await?;
    assert!(receive(&mut socket).await?.is_pong());
    tokio::time::advance(Duration::from_secs(10)).await;
    let Message::Close(Some(frame)) = receive(&mut socket).await? else {
        panic!("expected idle close");
    };
    assert_eq!(frame.reason, "idle");
    task.await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn blocked_reply_and_close_cannot_hold_session_forever() -> Result<(), TestError> {
    // A one-byte output buffer blocks the response while the client stops
    // reading. The deadline must interrupt the send as well as the read loop.
    let (mut socket, stop, task) = session_with(1, Duration::from_secs(120)).await;
    socket.send(Message::Text("PING,1".into())).await?;
    tokio::task::yield_now().await;
    stop.send(CloseReason::Finished).unwrap();
    tokio::time::timeout(Duration::from_secs(6), task).await??;
    Ok(())
}

#[tokio::test]
async fn quiet_upgraded_websocket_ends_with_idle_code() -> Result<(), TestError> {
    use graphite_meter_server::config::{Config, NativeKind};
    use graphite_meter_server::http::HttpServer;
    use std::sync::Arc;
    use tokio::net::TcpStream;
    let config = Config {
        max_operation_duration: Duration::from_secs(180),
        ..Config::default()
    };
    let server = native::serve(Arc::new(HttpServer::new(config.validated()?)?), NativeKind::H1, None).await;
    let address = server.address;
    let (mut socket, _) =
        tokio_tungstenite::client_async(format!("ws://{address}/ws/ping"), TcpStream::connect(address).await?).await?;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::task::yield_now().await;
    tokio::time::resume();
    let close = tokio::time::timeout(Duration::from_secs(1), socket.next())
        .await?
        .unwrap()?;
    let Message::Close(Some(close)) = close else {
        panic!("expected idle close")
    };
    assert_eq!(u16::from(close.code), 4001);
    assert_eq!(close.reason, "idle");
    server.shutdown().await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn lifetime_caps_a_quiet_websocket_before_its_idle_bound() -> Result<(), TestError> {
    let (mut socket, _stop, task) = session_with(8192, Duration::from_secs(10)).await;
    tokio::time::advance(Duration::from_secs(10)).await;
    let Message::Close(Some(frame)) = receive(&mut socket).await? else {
        panic!("expected lifetime close");
    };
    assert_eq!(u16::from(frame.code), 4002);
    assert_eq!(frame.reason, "lifetime");
    task.await?;
    Ok(())
}
