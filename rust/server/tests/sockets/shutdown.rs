//! Every listener at once: a stop closes them all, ends the running work of each transport with its shutdown ending,
//! and the server returns once their connections drained.

use super::{
    http3::{H3, pass, read, transport},
    webtransport::ending,
    *,
};
use futures_util::StreamExt;
use graphite_meter_http3::{Code, Error};
use rustls::pki_types::ServerName;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::{client_async, tungstenite::Message};

/// The reason a stream's reset carried, after the bytes before it.
async fn reset(mut body: h2::RecvStream) -> Option<h2::Reason> {
    while let Some(chunk) = body.data().await {
        match chunk {
            Ok(chunk) => {
                let _ = body.flow_control().release_capacity(chunk.len());
            }
            Err(error) => return error.reason(),
        }
    }
    None
}

#[tokio::test]
async fn a_stop_closes_every_listener_and_ends_the_work_on_each_transport() {
    let env = [("GM_H1_TLS_ADDR", "localhost:0"), ("GM_H2_ADDR", "127.0.0.3:0")];
    let h3 = H3::start(&env).await;
    let server = &h3.server;
    let name = || ServerName::try_from("localhost").unwrap();

    let mut download = server.connect().await;
    download
        .send(&format!("GET {ENDLESS} HTTP/1.1\r\nHost: test\r\n\r\n"))
        .await;
    download.head().await.unwrap();

    let socket = TcpStream::connect(server.tls.unwrap()).await.unwrap();
    let tls = TlsConnector::from(Arc::new(h3.identity.client(&[b"http/1.1"])));
    let stream = tls.connect(name(), socket).await.unwrap();
    let (mut bus, _) = client_async("wss://localhost/ws/ping", stream).await.unwrap();

    let socket = TcpStream::connect(server.h2.unwrap()).await.unwrap();
    let tls = TlsConnector::from(Arc::new(h3.identity.client(&[b"h2"])));
    let (h2, connection) = h2::client::handshake(tls.connect(name(), socket).await.unwrap())
        .await
        .unwrap();
    let h2_connection = tokio::spawn(connection);
    let mut h2 = h2.ready().await.unwrap();
    let request = http::Request::get(format!("https://localhost{ENDLESS}"))
        .body(())
        .unwrap();
    let h2_download = h2.send_request(request, true).unwrap().0.await.unwrap().into_body();

    let quic = h3.connect(transport(None)).await;
    let (_, mut h3_download) = quic.send("GET", ENDLESS, b"").await;
    let sessions = h3.connect(transport(None)).await;
    let session = sessions.session("/wt/ping").await;
    server.until_active(5).await;

    let addresses = [Some(server.address), server.tls, server.h2, server.companion].map(Option::unwrap);
    let stopped = h3.server.stop();
    let bound = Duration::from_secs(2);
    tokio::time::timeout(bound, download.drain())
        .await
        .expect("the HTTP/1 download ends");
    let Some(Ok(Message::Close(Some(close)))) = bus.next().await else {
        panic!("the bus closes");
    };
    assert_eq!((u16::from(close.code), close.reason.as_str()), (1001, "shutdown"));
    assert!(!matches!(bus.next().await, Some(Ok(_))), "the bus answers the close and ends");
    assert_eq!(reset(h2_download).await, Some(h2::Reason::CANCEL));
    let ended = read(&mut h3_download).await.unwrap_err();
    let closed = matches!(ended, Error::Connection { code: Code::H3_NO_ERROR, .. });
    assert!(closed || ended == Error::Reset(Code::H3_REQUEST_CANCELLED), "{ended:?}");
    assert_eq!(session.closed().await, Ok(ending(4, "shutdown")));
    for address in addresses {
        drop(TcpListener::bind(address).await.expect("every listener closed at once"));
    }

    assert_eq!(quic.closed_within(bound).await, Some(Code::H3_NO_ERROR));
    let closed = tokio::time::timeout(bound, h2_connection).await;
    let _ = closed.expect("the HTTP/2 connection closes after its streams").unwrap();
    session.close(0, "").await;
    pass(Duration::from_secs(1)).await;
    assert_eq!(sessions.closed_within(bound).await, Some(Code::H3_NO_ERROR));
    let stopped = tokio::time::timeout(bound, stopped).await;
    stopped.expect("the server returns once drained").unwrap().unwrap();
}
