#[path = "support/http1.rs"]
mod http1;

#[path = "support/http2.rs"]
mod http2;

#[path = "support/native.rs"]
mod native;

#[path = "../../test_tls.rs"]
mod test_tls;

use bytes::Bytes;
use futures_util::StreamExt;
use graphite_meter_server::config::{AuthConfig, AuthMode, Config, NativeKind};
use http::{HeaderValue, Request};
use rustls::pki_types::ServerName;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    task::JoinHandle,
};
use tokio_rustls::{TlsConnector, client::TlsStream};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

const HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0";
struct Harness {
    connector: TlsConnector,
    h2_connector: TlsConnector,
    listeners: [native::NativeServer; 2],
}
impl Harness {
    async fn start(trusted_proxies: Vec<ipnet::IpNet>) -> Self {
        let (tls, mut client) = test_tls::configs("localhost", &[&rustls::version::TLS13], &[b"http/1.1"]).unwrap();
        let connector = TlsConnector::from(Arc::new(client.clone()));
        client.alpn_protocols = vec![b"h2".to_vec()];
        let h2_connector = TlsConnector::from(Arc::new(client));
        let config = Config {
            trusted_proxies,
            advertised_native: Some(Default::default()),
            public: graphite_meter_server::config::PublicOrigins { both: vec!["self".into()], ..Default::default() },
            auth: AuthConfig {
                mode: AuthMode::Password,
                public_url: "https://localhost".into(),
                password_hash: HASH.into(),
                ..AuthConfig::default()
            },
            ..Config::default()
        };
        let server = native::server(config);
        let h1 = native::serve(server.clone(), NativeKind::H1Tls, Some(Arc::new(tls.clone()))).await;
        let h2 = native::serve(server, NativeKind::H2, Some(Arc::new(tls))).await;
        Self { connector, h2_connector, listeners: [h1, h2] }
    }
    async fn tls(connector: &TlsConnector, address: SocketAddr) -> TlsStream<TcpStream> {
        let tcp = TcpStream::connect(address).await.unwrap();
        let name = ServerName::try_from("localhost").unwrap();
        connector.connect(name, tcp).await.unwrap()
    }
    async fn connect(&self) -> TlsStream<TcpStream> {
        Self::tls(&self.connector, self.listeners[0].address).await
    }
    async fn h2(&self, window: u32) -> (h2::client::SendRequest<Bytes>, JoinHandle<Result<(), h2::Error>>) {
        let stream = Self::tls(&self.h2_connector, self.listeners[1].address).await;
        let handshake = h2::client::Builder::new().initial_window_size(window).handshake(stream);
        let (client, connection) = handshake.await.unwrap();
        (client, tokio::spawn(connection))
    }

    async fn request(&self, method: &str, path: &str, headers: &str, body: &str) -> (String, Vec<u8>) {
        let stream = self.connect().await;
        http1::exchange(stream, method, path, "localhost", headers, body.as_bytes()).await
    }

    async fn login(&self, client: &str) -> (String, String) {
        let (headers, _) = self.request("GET", "/login", client, "").await;
        assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
        assert!(headers.contains("x-frame-options: DENY\r\n"), "{headers}");
        let nonce = cookie(&headers, "__Host-gm_login");
        let body = form_urlencoded::Serializer::new(String::new())
            .append_pair("csrf", &nonce)
            .append_pair("password", "correct horse battery staple")
            .finish();
        let (headers,_)=self.request("POST","/auth/password",&format!("{client}Cookie: __Host-gm_login={nonce}\r\nOrigin: https://localhost\r\nContent-Type: application/x-www-form-urlencoded\r\n"),&body).await;
        assert!(headers.starts_with("HTTP/1.1 303"), "{headers}");
        (cookie(&headers, "__Host-gm_session"), cookie(&headers, "__Host-gm_csrf"))
    }
    async fn stop(mut self) {
        for listener in &mut self.listeners {
            listener.stop();
        }
        for listener in self.listeners {
            listener.shutdown().await;
        }
    }
}
fn cookie(headers: &str, name: &str) -> String {
    headers
        .lines()
        .find_map(|line| {
            line.strip_prefix(&format!("set-cookie: {name}="))
                .map(|value| value.split(';').next().unwrap().to_owned())
        })
        .unwrap()
}
/// An HTTP/2 request from the public origin, signed in where a session is given.
fn h2_request(method: &str, path: &str, session: Option<&str>) -> Request<()> {
    let mut request = http2::request(method, path);
    let headers = request.headers_mut();
    headers.insert("origin", HeaderValue::from_static("https://localhost"));
    if let Some(session) = session {
        headers.insert("cookie", format!("__Host-gm_session={session}").parse().unwrap());
    }
    request
}
fn credentials(session: &str, csrf: &str) -> String {
    format!("Cookie: __Host-gm_session={session}\r\nOrigin: https://localhost\r\nX-CSRF-Token: {csrf}\r\n")
}
async fn read_until(stream: &mut TlsStream<TcpStream>, needle: &[u8]) {
    let mut result = Vec::new();
    while !result.windows(needle.len()).any(|w| w == needle) {
        let mut block = [0; 4096];
        let n = stream.read(&mut block).await.unwrap();
        assert!(n > 0);
        result.extend_from_slice(&block[..n]);
    }
}

#[tokio::test]
async fn password_tls_upload_h2_and_websocket_are_bound_to_login_lifetime() {
    let flow = tokio::time::timeout(Duration::from_secs(20), password_flow());
    flow.await.unwrap();
}

async fn password_flow() {
    let h = Harness::start(Vec::new()).await;
    let (session, csrf) = h.login("").await;
    let form = "Origin: https://localhost\r\nContent-Type: application/x-www-form-urlencoded\r\n";
    let (oversized, _) = h.request("POST", "/auth/password", form, &"x".repeat(5000)).await;
    assert!(oversized.contains("location: /login?error=failed\r\n"), "{oversized}");
    let headers = credentials(&session, &csrf);
    let (other, other_csrf) = h.login("").await;
    let (_, body) = h.request("POST", "/upload/session", &headers, "").await;
    let upload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let id = upload["uploadId"].as_str().unwrap();
    let mut progress = h.connect().await;
    let head = format!("GET /upload/progress?id={id} HTTP/1.1\r\nHost: localhost\r\n{headers}\r\n");
    progress.write_all(head.as_bytes()).await.unwrap();
    read_until(&mut progress, b"ready").await;
    // Leave an upload body incomplete: revocation must cancel reads,
    // not wait for the next frame or the 120-second upload bound.
    let mut uploading = h.connect().await;
    let head = format!("POST /upload?id={id} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100000\r\n{headers}\r\nx");
    uploading.write_all(head.as_bytes()).await.unwrap();
    let (mut client, driver) = h.h2(0).await;
    let download = h2_request("GET", "/download?bytes=1000000", Some(&session));
    let (response, _) = client.send_request(download, true).unwrap();
    let mut stalled = response.await.unwrap().into_body();
    let (response, _) = client
        .send_request(h2_request("GET", "/login", Some(&session)), true)
        .unwrap();
    assert_eq!(response.await.unwrap().status(), 403); // native listener has no UI authority
    let mut ws_request = "wss://localhost/ws/ping".into_client_request().unwrap();
    let ws_headers = ws_request.headers_mut();
    ws_headers.insert("cookie", format!("__Host-gm_session={session}").parse().unwrap());
    ws_headers.insert("origin", "https://localhost".parse().unwrap());
    let connected = tokio_tungstenite::client_async(ws_request, h.connect().await).await;
    let (mut websocket, _) = connected.unwrap();
    let logout = format!("csrf={csrf}");
    let logout_headers = format!("{headers}Content-Type: application/x-www-form-urlencoded\r\n");
    let (logged_out, _) = h.request("POST", "/auth/logout", &logout_headers, &logout).await;
    assert!(logged_out.starts_with("HTTP/1.1 303"), "{logged_out}");
    assert!(logged_out.contains("__Host-gm_session=;"));
    let revoked = async {
        assert!(stalled.data().await.unwrap().is_err());
        match websocket.next().await.unwrap().unwrap() {
            Message::Close(Some(frame)) => assert_eq!(frame.reason, "authentication required"),
            message => panic!("unexpected {message:?}"),
        }
        let _ = progress.read_to_end(&mut Vec::new()).await;
        let _ = uploading.read_to_end(&mut Vec::new()).await;
    };
    let revoked = tokio::time::timeout(Duration::from_secs(1), revoked).await;
    revoked.expect("revocation cancels every owned transport promptly");
    let (response, _) = client
        .send_request(h2_request("GET", "/download?bytes=0", Some(&other)), true)
        .unwrap();
    assert_eq!(response.await.unwrap().status(), 200);
    let (ok, _) = h.request("GET", "/probe", &credentials(&other, &other_csrf), "").await;
    assert!(ok.starts_with("HTTP/1.1 200"));
    let (denied, _) = h.request("GET", "/probe", &headers, "").await;
    assert!(denied.starts_with("HTTP/1.1 403"));
    driver.abort();
    let _ = driver.await;
    h.stop().await;
}

#[tokio::test]
async fn approval_pages_require_client_evidence_behind_a_trusted_proxy() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};

    let h = Harness::start(vec!["127.0.0.0/8".parse().unwrap()]).await;
    // The CLI and browser approval pages for `challenge`.
    let pages = |challenge: &str| {
        let browser = format!("/auth/browser?challenge={challenge}&client_origin=https://client.example");
        [format!("/auth/cli?challenge={challenge}"), browser]
    };
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(b"approval verifier"));
    // As in Go, the CLI page sends a signed-out caller to sign in before it reads the address.
    for (path, without_evidence) in pages(&challenge).into_iter().zip(["HTTP/1.1 303", "HTTP/1.1 403"]) {
        let (unresolved, body) = h.request("GET", &path, "", "").await;
        assert!(unresolved.starts_with(without_evidence), "{unresolved}");
        let body = String::from_utf8(body).unwrap();
        if without_evidence.ends_with("403") {
            assert!(body.contains("Too many approvals are open."));
        }
        let (allowed, _) = h.request("GET", &path, "X-Real-IP: 192.0.2.1\r\n", "").await;
        assert!(allowed.starts_with("HTTP/1.1 303"), "{allowed}");
    }
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(b"signed-in approval verifier"));
    let client = "X-Real-IP: 192.0.2.1\r\n";
    let (session, csrf) = h.login(client).await;
    let signed_in = credentials(&session, &csrf);
    // A refusal card shows `message` and no request value.
    let card = |body: Vec<u8>, message: &str, challenge: &str| {
        let body = String::from_utf8(body).unwrap();
        assert!(body.contains(message) && !body.contains(challenge));
        assert!(!body.contains("client.example") && !body.contains(&csrf));
    };
    for path in pages(&challenge) {
        let (denied, body) = h.request("GET", &path, &signed_in, "").await;
        assert!(
            denied.starts_with("HTTP/1.1 403") && denied.contains("content-type: text/html"),
            "{path}: {denied}"
        );
        card(body, "Too many approvals are open.", &challenge);
    }
    for path in [
        "/auth/cli?challenge=invalid".to_owned(),
        format!("/auth/browser?challenge={challenge}&client_origin=http://client.example"),
    ] {
        let (denied, body) = h.request("GET", &path, &format!("{client}{signed_in}"), "").await;
        assert!(denied.starts_with("HTTP/1.1 403"));
        card(body, "This approval link is not valid.", &challenge);
    }
    // Use a fresh address: the earlier anonymous browser approval still holds its original client's slot.
    let client = "X-Real-IP: 198.51.100.1\r\n";
    // One login may open eight approvals; its ninth gets a generic card without request values or counts.
    for index in 0..9 {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(format!("bounded-{index}")));
        let path = format!("/auth/cli?challenge={challenge}");
        let (headers, body) = h.request("GET", &path, &format!("{client}{signed_in}"), "").await;
        assert!(headers.starts_with(if index < 8 { "HTTP/1.1 200" } else { "HTTP/1.1 403" }));
        if index == 8 {
            card(body, "Too many approvals are open.", &challenge);
        }
    }
    h.stop().await;
}

#[tokio::test]
async fn an_ambiguous_request_is_refused_before_hsts_as_in_go() {
    let h = Harness::start(Vec::new()).await;
    let repeated = "Authorization: Bearer a\r\nAuthorization: Bearer b\r\n";
    let (refused, _) = h.request("GET", "/probe", repeated, "").await;
    assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");
    assert!(refused.contains("x-frame-options: DENY\r\n"), "{refused}");
    assert!(!refused.contains("strict-transport-security"), "{refused}");
    // A refusal once the connection is trusted keeps it.
    let invalid = "Authorization: Bearer invalid\r\n";
    let (refused, _) = h.request("GET", "/probe", invalid, "").await;
    assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");
    let hsts = "strict-transport-security: max-age=31536000\r\n";
    assert!(refused.contains(hsts), "{refused}");
    h.stop().await;
}

#[tokio::test]
async fn an_empty_origin_is_given_no_cors_headers_as_in_go() {
    let h = Harness::start(Vec::new()).await;
    let (session, _) = h.login("").await;
    let signed_in = format!("Cookie: __Host-gm_session={session}\r\nOrigin: \r\nSec-Fetch-Site: same-origin\r\n");
    for path in ["/download?bytes=1", "/probe"] {
        let (answer, _) = h.request("GET", path, &signed_in, "").await;
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        assert!(
            !answer.contains("access-control-") && !answer.contains("timing-allow-origin") && !answer.contains("vary"),
            "{answer}"
        );
    }
    h.stop().await;
}

#[tokio::test]
async fn native_listeners_authorize_routes_they_do_not_mount_before_404() {
    let h = Harness::start(Vec::new()).await;
    let (session, _) = h.login("").await;
    let (mut client, driver) = h.h2(65_535).await;
    // The native HTTP/2 listener mounts neither WebSockets nor WebTransport.
    for path in ["/ws/ping", "/wt/download"] {
        for session in [None, Some(session.as_str())] {
            let (response, _) = client.send_request(h2_request("GET", path, session), true).unwrap();
            let response = response.await.unwrap();
            let expected = if session.is_some() { 404 } else { 403 };
            assert_eq!(response.status(), expected, "{path}");
            let hsts = &response.headers()["strict-transport-security"];
            assert_eq!(hsts, "max-age=31536000", "{path}");
        }
    }
    drop(client);
    driver.abort();
    h.stop().await;
}

#[tokio::test]
async fn socket_tickets_are_minted_only_where_mounted_and_only_for_post() {
    let h = Harness::start(Vec::new()).await;
    let (session, csrf) = h.login("").await;
    let targets = [
        ("/ws/session", "https://localhost/ws/ping"),
        ("/wt/session", "https://localhost:8443/wt/ping"),
    ];
    // The UI listener mounts both, for POST alone; as Go's "/" pattern, its app answers GET.
    let signed_in = credentials(&session, &csrf);
    for (route, target) in targets {
        let path = format!("{route}?target={target}");
        let (minted, body) = h.request("POST", &path, &signed_in, "").await;
        assert!(minted.starts_with("HTTP/1.1 200"));
        assert!(minted.contains("access-control-allow-origin: https://localhost\r\n"));
        assert!(minted.contains("access-control-allow-credentials: true\r\n"));
        assert!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["token"].is_string());
        let (refused, _) = h.request("GET", &path, &signed_in, "").await;
        assert!(refused.starts_with("HTTP/1.1 404"), "{route}: {refused}");
    }
    let (mut client, driver) = h.h2(65_535).await;
    // As in Go, the native HTTP/2 listener mounts only the WebTransport ticket; WebSockets live on the UI listeners.
    for (method, (route, target), expected) in [
        ("GET", targets[0], 404),
        ("HEAD", targets[0], 404),
        ("POST", targets[0], 404),
        ("GET", targets[1], 405),
        ("POST", targets[1], 200),
    ] {
        let mut request = h2_request(method, &format!("{route}?target={target}"), Some(&session));
        request.headers_mut().insert("x-csrf-token", csrf.parse().unwrap());
        client = client.ready().await.unwrap();
        let (response, _) = client.send_request(request, true).unwrap();
        let response = response.await.unwrap();
        assert_eq!(response.status(), expected, "{method} {route}");
        let bytes = http2::body(response.into_body()).await;
        let minted =
            serde_json::from_slice::<serde_json::Value>(&bytes).is_ok_and(|ticket| ticket["token"].is_string());
        assert_eq!(minted, expected == 200, "{method} {route}");
    }
    drop(client);
    driver.abort();
    h.stop().await;
}
