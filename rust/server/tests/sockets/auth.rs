//! Revocation over real sockets: sign-out closes a WebSocket bus with 1008 and a WebTransport session with 3, and a
//! CONNECT presents its grant before the upgrade.

use super::{
    http3::{Connection, H3, transport},
    webtransport::{ending, open},
    *,
};
use futures_util::{SinkExt, StreamExt};
use graphite_meter_http3::webtransport::Session;
use graphite_meter_server::auth::{NewLogin, Store};
use http::{Request, Response};
use tokio_tungstenite::{
    client_async,
    tungstenite::{Message, client::IntoClientRequest},
};

/// Password authentication at `https://localhost`, with the local address a trusted proxy.
const AUTH: [(&str, &str); 5] = [
    ("GM_AUTH_MODE", "password"),
    ("GM_AUTH_PUBLIC_URL", "https://localhost"),
    (
        "GM_AUTH_PASSWORD_HASH",
        "$argon2id$v=19$m=19456,t=2,p=1$OT2po7nOdP+21BKX5CuZQw$9kVgfSWvlFy31939zUCVY62fHIuSqC8RwL67EpQ8qy8",
    ),
    ("GM_ADVERTISED_NATIVE_ENDPOINTS", "http3"),
    ("GM_TRUSTED_PROXIES", "127.0.0.1/32"),
];

fn signed_in(h3: &H3) -> (Store, NewLogin) {
    let store = h3.server.app.auth().store().unwrap().clone();
    let login = store.sign_in("operator", "Operator", "local").unwrap();
    (store, login)
}

async fn session(connection: &Connection, path: &str, grant: Option<&str>) -> Result<Session, Response<()>> {
    let mut request = Request::get(format!("https://localhost{path}"));
    if let Some(grant) = grant {
        request = request.header("authorization", format!("Bearer {grant}"));
    }
    let opened = Session::connect(&connection.requests, request.body(()).unwrap())
        .await
        .unwrap();
    opened.map(|(session, _)| session)
}

#[tokio::test]
async fn sign_out_closes_a_bus_with_1008() {
    let h3 = H3::start(&AUTH).await;
    let (store, login) = signed_in(&h3);
    let grant = store.grant(login.key, None).unwrap();
    let address = h3.server.address;
    let mut request = format!("ws://{address}/ws/ping").into_client_request().unwrap();
    let headers = request.headers_mut();
    headers.insert("authorization", format!("Bearer {grant}").parse().unwrap());
    headers.insert("x-forwarded-proto", "https".parse().unwrap());
    headers.insert("x-forwarded-host", "localhost".parse().unwrap());
    let (mut socket, answer) = client_async(request, TcpStream::connect(address).await.unwrap())
        .await
        .unwrap();
    assert_eq!(answer.status(), 101);
    socket.send(Message::text("PING,1")).await.unwrap();
    assert!(socket.next().await.unwrap().unwrap().is_text());
    assert!(store.sign_out(login.key, false));
    match socket.next().await {
        Some(Ok(Message::Close(Some(frame)))) => {
            assert_eq!((u16::from(frame.code), frame.reason.as_str()), (1008, "authentication required"));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_connect_needs_a_grant_and_sign_out_ends_its_session_with_3() {
    let h3 = H3::start(&AUTH).await;
    let (store, login) = signed_in(&h3);
    let connection = h3.connect(transport(None)).await;
    let refused = session(&connection, "/wt/ping", None).await.err().expect("a refusal");
    assert_eq!(refused.status(), 403);
    assert_eq!(refused.headers()["graphite-meter-auth"], "required");
    assert_eq!(refused.headers()["strict-transport-security"], "max-age=31536000");
    let grant = store.grant(login.key, None).unwrap();
    let opened = session(&connection, "/wt/ping", Some(&grant)).await.unwrap();
    assert!(open(&opened).await);
    assert!(store.sign_out(login.key, false));
    assert_eq!(opened.closed().await, Ok(ending(3, "authentication required")));
}
