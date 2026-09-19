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
mod support;

#[test]
fn upgrade_validates_origin_and_key_without_negotiating_compression() {
    use http::{Request, StatusCode, header};
    let mut request = Request::builder()
        .uri("/ws/ping")
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==") // RFC 6455 example nonce; gitleaks:allow
        .header(header::SEC_WEBSOCKET_EXTENSIONS, "permessage-deflate")
        .header(header::ORIGIN, "https://meter.example")
        .body(())
        .unwrap();
    let response = handshake(&request, Some("https://meter.example"));
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    assert_eq!(
        response.headers()[header::SEC_WEBSOCKET_ACCEPT],
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
    assert!(
        !response
            .headers()
            .contains_key(header::SEC_WEBSOCKET_EXTENSIONS)
    );
    assert_eq!(
        handshake(&request, Some("https://other.example")).status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        handshake(&request, None).status(),
        StatusCode::SWITCHING_PROTOCOLS
    );
    request
        .headers_mut()
        .insert(header::CONNECTION, "keep-alive".parse().unwrap());
    request
        .headers_mut()
        .append(header::CONNECTION, "UpGrAdE".parse().unwrap());
    request
        .headers_mut()
        .insert(header::UPGRADE, "other, WebSocket".parse().unwrap());
    assert_eq!(
        handshake(&request, None).status(),
        StatusCode::SWITCHING_PROTOCOLS
    );
    request.headers_mut().append(
        header::SEC_WEBSOCKET_KEY,
        "dGhlIHNhbXBsZSBub25jZQ==".parse().unwrap(),
    );
    assert_eq!(handshake(&request, None).status(), StatusCode::BAD_REQUEST);
}

async fn session() -> (
    WebSocketStream<DuplexStream>,
    oneshot::Sender<CloseReason>,
    JoinHandle<()>,
) {
    let (client, server) = tokio::io::duplex(8192);
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(serve_ping(server, async {
        stopped.await.unwrap_or(CloseReason::Finished)
    }));
    (
        WebSocketStream::from_raw_socket(client, Role::Client, None).await,
        stop,
        task,
    )
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
        Message::Frame(Frame::message(
            b"PING,".to_vec(),
            OpCode::Data(Data::Text),
            false,
        )),
        Message::Frame(Frame::message(
            b"9".to_vec(),
            OpCode::Data(Data::Continue),
            true,
        )),
    ] {
        socket.send(message).await?;
    }
    for id in [7, 8, 9] {
        let message = receive(&mut socket).await?;
        assert!(message.is_text());
        assert_eq!(decode_pong(message.to_text()?)?.id, id);
    }
    socket
        .send(Message::Ping(b"control".to_vec().into()))
        .await?;
    assert_eq!(
        receive(&mut socket).await?,
        Message::Pong(b"control".to_vec().into())
    );
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
    stop.send(CloseReason::AuthenticationRequired).unwrap();
    let Message::Close(Some(frame)) = receive(&mut socket).await? else {
        panic!("expected authentication close");
    };
    assert_eq!(frame.code, CloseCode::Policy);
    assert_eq!(frame.reason, "authentication required");
    task.await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn blocked_reply_and_close_cannot_hold_session_forever() -> Result<(), TestError> {
    // A one-byte output buffer blocks the response while the client stops
    // reading. The deadline must interrupt the send as well as the read loop.
    let (client, server) = tokio::io::duplex(1);
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(serve_ping(server, async { stopped.await.unwrap() }));
    let mut socket = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
    socket.send(Message::Text("PING,1".into())).await?;
    tokio::task::yield_now().await;
    stop.send(CloseReason::Finished).unwrap();
    tokio::time::timeout(Duration::from_secs(6), task).await??;
    Ok(())
}

#[tokio::test]
async fn http_upgrade_retains_admission_and_shutdown_owns_the_socket() -> Result<(), TestError> {
    use graphite_meter_server::{config::Config, http_server::HttpServer};
    use http::{Request, StatusCode};
    use std::sync::Arc;
    use tokio::net::{TcpListener, TcpStream};

    let mut config = Config::default();
    config.limits.operations_per_client = 1;
    config.limits.sessions_per_client = 1;
    let server = Arc::new(HttpServer::new(Arc::new(config))?);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (stop, stopped) = oneshot::channel();
    let serving = server.clone();
    let task = tokio::spawn(serving.serve_http1(listener, async {
        let _ = stopped.await;
    }));
    let (mut socket, response) = tokio_tungstenite::client_async(
        format!("ws://{address}/ws/ping"),
        TcpStream::connect(address).await?,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    socket.send(Message::Text("PING,23".into())).await?;
    let pong = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await?
        .unwrap()?;
    assert_eq!(decode_pong(pong.to_text()?)?.id, 23);
    let response = server.respond(
        Request::builder().uri("/download?bytes=1").body(())?,
        address,
    );
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task).await???;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), socket.next()).await?,
        None | Some(Err(_))
    ));
    let response = server.respond(
        Request::builder().uri("/download?bytes=1").body(())?,
        address,
    );
    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn websocket_upgrade_works_over_validated_tls() -> Result<(), TestError> {
    use graphite_meter_server::{config::Config, http_server::HttpServer, tls::Certificates};
    use rustls::{
        ClientConfig, RootCertStore,
        pki_types::{CertificateDer, ServerName, pem::PemObject},
    };
    use std::{sync::Arc, time::SystemTime};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_rustls::TlsConnector;

    let identity = support::Identity::generate()?;
    let config = Config {
        tls_cert: identity
            .directory()
            .join("identity.pem")
            .to_str()
            .unwrap()
            .into(),
        tls_key: identity
            .directory()
            .join("identity.key")
            .to_str()
            .unwrap()
            .into(),
        ..Config::default()
    };
    let tls = Certificates::load(&config, SystemTime::now())?.config(vec![b"http/1.1".to_vec()])?;
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from_pem_file(&config.tls_cert)?)?;
    let mut client_tls = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let server = Arc::new(HttpServer::new(Arc::new(config))?);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.serve_https1(listener, tls, async {
        let _ = stopped.await;
    }));
    let stream = TlsConnector::from(Arc::new(client_tls))
        .connect(
            ServerName::try_from("localhost")?,
            TcpStream::connect(address).await?,
        )
        .await?;
    let (mut socket, _) = tokio_tungstenite::client_async(
        format!("wss://localhost:{}/ws/ping", address.port()),
        stream,
    )
    .await?;
    socket.send(Message::Text("PING,42".into())).await?;
    let pong = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await?
        .unwrap()?;
    assert_eq!(decode_pong(pong.to_text()?)?.id, 42);
    socket.close(None).await?;
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task).await???;
    Ok(())
}

#[tokio::test]
async fn quiet_upgraded_websocket_survives_http_idle_interval() -> Result<(), TestError> {
    use graphite_meter_server::{config::Config, http_server::HttpServer};
    use std::sync::Arc;
    use tokio::net::{TcpListener, TcpStream};
    let server = Arc::new(HttpServer::new(Arc::new(Config {
        max_operation_duration: Duration::from_secs(180),
        ..Config::default()
    }))?);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.serve_http1(listener, async {
        let _ = stopped.await;
    }));
    let (mut socket, _) = tokio_tungstenite::client_async(
        format!("ws://{address}/ws/ping"),
        TcpStream::connect(address).await?,
    )
    .await?;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::task::yield_now().await;
    tokio::time::resume();
    socket.send(Message::Text("PING,42".into())).await?;
    let pong = tokio::time::timeout(Duration::from_secs(1), socket.next())
        .await?
        .unwrap()?;
    assert_eq!(decode_pong(pong.to_text()?)?.id, 42);
    stop.send(()).unwrap();
    task.await??;
    Ok(())
}
