mod support;

use bytes::Bytes;
use futures_util::StreamExt;
use graphite_meter_server::{
    config::{AuthConfig, AuthMode, Config},
    http_server::HttpServer,
};
use http::Request;
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject},
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tokio_rustls::{TlsConnector, client::TlsStream};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

const HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0";
struct Harness {
    h1: SocketAddr,
    h2: SocketAddr,
    connector: TlsConnector,
    h2_connector: TlsConnector,
    stop: Vec<oneshot::Sender<()>>,
    tasks: Vec<tokio::task::JoinHandle<Result<(), graphite_meter_server::config::ConfigError>>>,
}
impl Harness {
    async fn start() -> Self {
        let identity = support::Identity::generate().unwrap();
        let cert =
            CertificateDer::from_pem_file(identity.directory().join("identity.pem")).unwrap();
        let key = PrivateKeyDer::from_pem_file(identity.directory().join("identity.key")).unwrap();
        let provider = Arc::new(graphite_meter_server::crypto::provider());
        let mut tls = ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.clone()], key)
            .unwrap();
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        let mut roots = RootCertStore::empty();
        roots.add(cert).unwrap();
        let mut client = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        client.alpn_protocols = vec![b"http/1.1".to_vec()];
        let connector = TlsConnector::from(Arc::new(client.clone()));
        client.alpn_protocols = vec![b"h2".to_vec()];
        let h2_connector = TlsConnector::from(Arc::new(client));
        let config = Config {
            advertised_native: Some(Default::default()),
            public: graphite_meter_server::config::PublicOrigins {
                both: vec!["self".into()],
                ..Default::default()
            },
            auth: AuthConfig {
                mode: AuthMode::Password,
                public_url: "https://localhost".into(),
                password_hash: HASH.into(),
                ..AuthConfig::default()
            },
            ..Config::default()
        };
        let server = Arc::new(HttpServer::new(Arc::new(config)).unwrap());
        let l1 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let h1 = l1.local_addr().unwrap();
        let l2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let h2 = l2.local_addr().unwrap();
        let (s1, r1) = oneshot::channel();
        let (s2, r2) = oneshot::channel();
        let t1 = tokio::spawn(
            server
                .clone()
                .serve_https1(l1, Arc::new(tls.clone()), async {
                    let _ = r1.await;
                }),
        );
        tls.alpn_protocols = vec![b"h2".to_vec()];
        let t2 = tokio::spawn(server.serve_http2(l2, Arc::new(tls), async {
            let _ = r2.await;
        }));
        Self {
            h1,
            h2,
            connector,
            h2_connector,
            stop: vec![s1, s2],
            tasks: vec![t1, t2],
        }
    }
    async fn connect(&self) -> TlsStream<TcpStream> {
        self.connector
            .connect(
                ServerName::try_from("localhost").unwrap(),
                TcpStream::connect(self.h1).await.unwrap(),
            )
            .await
            .unwrap()
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &str,
        body: &str,
    ) -> (String, Vec<u8>) {
        let mut stream = self.connect().await;
        stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",body.len()).as_bytes()).await.unwrap();
        let mut output = Vec::new();
        stream.read_to_end(&mut output).await.unwrap();
        let end = output.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        (
            String::from_utf8(output[..end].to_vec()).unwrap(),
            output[end..].to_vec(),
        )
    }
    async fn login(&self) -> (String, String) {
        let (headers, _) = self.request("GET", "/login", "", "").await;
        assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
        let nonce = cookie(&headers, "__Host-gm_login");
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("csrf", &nonce)
            .append_pair("password", "correct horse battery staple")
            .finish();
        let (headers,_)=self.request("POST","/auth/password",&format!("Cookie: __Host-gm_login={nonce}\r\nOrigin: https://localhost\r\nContent-Type: application/x-www-form-urlencoded\r\n"),&body).await;
        assert!(headers.starts_with("HTTP/1.1 303"), "{headers}");
        (
            cookie(&headers, "__Host-gm_session"),
            cookie(&headers, "__Host-gm_csrf"),
        )
    }
    async fn stop(self) {
        for stop in self.stop {
            let _ = stop.send(());
        }
        for task in self.tasks {
            task.await.unwrap().unwrap();
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
fn credentials(session: &str, csrf: &str) -> String {
    format!(
        "Cookie: __Host-gm_session={session}\r\nOrigin: https://localhost\r\nX-CSRF-Token: {csrf}\r\n"
    )
}
async fn read_until(stream: &mut TlsStream<TcpStream>, needle: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    while !result.windows(needle.len()).any(|w| w == needle) {
        let mut block = [0; 4096];
        let n = stream.read(&mut block).await.unwrap();
        assert!(n > 0);
        result.extend_from_slice(&block[..n]);
    }
    result
}

#[tokio::test]
async fn password_tls_upload_h2_and_websocket_are_bound_to_login_lifetime() {
    tokio::time::timeout(Duration::from_secs(20), password_flow())
        .await
        .unwrap();
}

async fn password_flow() {
    let h = Harness::start().await;
    let (denied, _) = h.request("GET", "/download?bytes=1", "", "").await;
    assert!(denied.starts_with("HTTP/1.1 403"));
    assert!(denied.contains("graphite-meter-auth: required"));
    assert!(!denied.contains("access-control-allow-origin: *"));
    let (session, csrf) = h.login().await;
    let headers = credentials(&session, &csrf);
    for (route, target) in [
        ("/ws/session", "https://localhost/ws/ping"),
        ("/wt/session", "https://localhost:8443/wt/ping"),
    ] {
        let (minted, body) = h
            .request("POST", &format!("{route}?target={target}"), &headers, "")
            .await;
        assert!(minted.starts_with("HTTP/1.1 200"));
        assert!(minted.contains("access-control-allow-origin: https://localhost\r\n"));
        assert!(minted.contains("access-control-allow-credentials: true\r\n"));
        assert!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["token"].is_string());
    }
    let (other, other_csrf) = h.login().await;
    let (ok, body) = h.request("GET", "/download?bytes=1000", &headers, "").await;
    assert!(ok.starts_with("HTTP/1.1 200"));
    assert_eq!(body.len(), 1000);
    assert!(ok.contains("access-control-allow-credentials: true"));
    assert!(!ok.contains("access-control-allow-origin: *"));
    let (bad, _) = h
        .request(
            "GET",
            "/download?bytes=1",
            &format!("{headers}Authorization: Bearer invalid\r\n"),
            "",
        )
        .await;
    assert!(bad.starts_with("HTTP/1.1 403"));
    let (_, body) = h.request("POST", "/upload/session", &headers, "").await;
    let id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["uploadId"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut progress = h.connect().await;
    progress
        .write_all(
            format!("GET /upload/progress?id={id} HTTP/1.1\r\nHost: localhost\r\n{headers}\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    read_until(&mut progress, b"ready").await;
    let (_, body) = h
        .request("POST", &format!("/upload?id={id}"), &headers, "hello auth")
        .await;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["bytes"],
        10
    );
    let (checkpoint, _) = h
        .request("POST", &format!("/upload/checkpoint?id={id}"), &headers, "")
        .await;
    assert!(checkpoint.starts_with("HTTP/1.1 200"));
    // Leave a second upload body incomplete: revocation must cancel reads,
    // not wait for the next frame or the 120-second upload bound.
    let mut uploading = h.connect().await;
    uploading.write_all(format!("POST /upload?id={id} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100000\r\n{headers}\r\nx").as_bytes()).await.unwrap();
    let stream = h
        .h2_connector
        .connect(
            ServerName::try_from("localhost").unwrap(),
            TcpStream::connect(h.h2).await.unwrap(),
        )
        .await
        .unwrap();
    let (mut client, connection) = h2::client::Builder::new()
        .initial_window_size(0)
        .handshake::<_, Bytes>(stream)
        .await
        .unwrap();
    let driver = tokio::spawn(connection);
    let request = |path: &str, token: &str| {
        Request::builder()
            .uri(format!("https://localhost{path}"))
            .header("cookie", format!("__Host-gm_session={token}"))
            .header("origin", "https://localhost")
            .body(())
            .unwrap()
    };
    let (response, _) = client
        .send_request(request("/download?bytes=1000000", &session), true)
        .unwrap();
    let mut stalled = response.await.unwrap().into_body();
    let (response, _) = client
        .send_request(request("/login", &session), true)
        .unwrap();
    assert_eq!(response.await.unwrap().status(), 403); // native listener has no UI authority
    let mut ws_request = "wss://localhost/ws/ping".into_client_request().unwrap();
    ws_request.headers_mut().insert(
        "cookie",
        format!("__Host-gm_session={session}").parse().unwrap(),
    );
    ws_request
        .headers_mut()
        .insert("origin", "https://localhost".parse().unwrap());
    let (mut websocket, _) = tokio_tungstenite::client_async(ws_request, h.connect().await)
        .await
        .unwrap();
    let logout = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("csrf", &csrf)
        .finish();
    let (logged_out, _) = h
        .request(
            "POST",
            "/auth/logout",
            &format!("{headers}Content-Type: application/x-www-form-urlencoded\r\n"),
            &logout,
        )
        .await;
    assert!(logged_out.starts_with("HTTP/1.1 303"), "{logged_out}");
    assert!(logged_out.contains("__Host-gm_session=;"));
    let revoked = async {
        assert!(stalled.data().await.unwrap().is_err());
        match websocket.next().await.unwrap().unwrap() {
            Message::Close(Some(frame)) => assert_eq!(frame.reason, "authentication required"),
            message => panic!("unexpected {message:?}"),
        }
        let mut bytes = Vec::new();
        let _ = progress.read_to_end(&mut bytes).await;
        let mut bytes = Vec::new();
        let _ = uploading.read_to_end(&mut bytes).await;
    };
    tokio::time::timeout(Duration::from_secs(1), revoked)
        .await
        .expect("revocation cancels every owned transport promptly");
    let (response, _) = client
        .send_request(request("/download?bytes=0", &other), true)
        .unwrap();
    assert_eq!(response.await.unwrap().status(), 200);
    let (ok, _) = h
        .request("GET", "/probe", &credentials(&other, &other_csrf), "")
        .await;
    assert!(ok.starts_with("HTTP/1.1 200"));
    let (denied, _) = h.request("GET", "/probe", &headers, "").await;
    assert!(denied.starts_with("HTTP/1.1 403"));
    driver.abort();
    let _ = driver.await;
    h.stop().await;
}
