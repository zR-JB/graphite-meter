use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn origin(raw: &str) -> Origin {
    target_origin(raw).unwrap().unwrap()
}

#[test]
fn bypass_follows_go_no_proxy_rules_and_never_proxies_loopback() {
    let proxy = Proxy::new(
        "http://proxy.example:3128",
        "http://proxy.example:3128",
        "corp.example, .sub.example, *.wild.example, pinned.example:8443, 10.0.0.0/8, 192.0.2.7, [2001:db8::1]:443, bad:port",
    );
    for (raw, bypassed) in [
        ("https://corp.example", true),
        ("https://a.corp.example", true),
        ("https://notcorp.example", false),
        ("https://sub.example", false),
        ("https://a.sub.example", true),
        ("https://a.wild.example", true),
        ("https://wild.example", false),
        ("https://pinned.example:8443", true),
        ("https://pinned.example", false),
        ("http://10.1.2.3", true),
        ("http://192.0.2.7:81", true),
        ("https://[2001:db8::1]", true),
        ("https://[2001:db8::1]:8443", false),
        ("https://localhost:9", true),
        ("https://127.0.0.2", true),
        ("https://[::1]", true),
        ("https://meter.example", false),
    ] {
        assert_eq!(proxy.bypassed(&origin(raw)), bypassed, "{raw}");
    }
    assert!(Proxy::new("", "", "*").bypassed(&origin("https://meter.example")));
}

#[test]
fn upstreams_default_to_http_carry_decoded_credentials_and_refuse_socks() {
    let proxy = Proxy::new(
        "proxy.example:3128",
        "http://u%3Ar:p%40ss@[2001:db8::2]",
        "",
    );
    let http = proxy
        .route(&origin("http://meter.example"))
        .unwrap()
        .as_ref()
        .unwrap();
    assert_eq!(
        (http.origin.key().as_str(), http.authorization.as_deref()),
        ("http://proxy.example:3128", None)
    );
    let https = proxy
        .route(&origin("https://meter.example"))
        .unwrap()
        .as_ref()
        .unwrap();
    assert_eq!(https.origin.authority(), "[2001:db8::2]");
    assert_eq!(
        https.authorization.as_deref(),
        Some(format!("Basic {}", STANDARD.encode("u:r:p@ss")).as_str())
    );
    let socks = Proxy::new("", "socks5://proxy.example:1080", "");
    assert!(
        socks
            .route(&origin("https://meter.example"))
            .unwrap()
            .is_err()
    );
    assert!(
        Proxy::new("", "", "")
            .route(&origin("https://meter.example"))
            .is_none()
    );
}

async fn proxy(
    status: &'static str,
    service: SocketAddr,
) -> (SocketAddr, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(client.read_u8().await.unwrap());
        }
        client.write_all(status.as_bytes()).await.unwrap();
        if status.starts_with("HTTP/1.1 200") {
            let mut upstream = TcpStream::connect(service).await.unwrap();
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        }
        String::from_utf8(head).unwrap()
    });
    (address, task)
}

#[tokio::test]
async fn connect_tunnels_through_the_proxy_with_credentials() {
    let service = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let service_address = service.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = service.accept().await.unwrap();
        let mut buffer = [0; 4];
        socket.read_exact(&mut buffer).await.unwrap();
        socket.write_all(&buffer).await.unwrap();
    });
    let (address, head) = proxy(
        "HTTP/1.1 200 Connection established\r\n\r\n",
        service_address,
    )
    .await;
    let socket = Box::new(TcpStream::connect(address).await.unwrap());
    let authorization = format!("Basic {}", STANDARD.encode("user:secret"));
    let mut connection = tunnel(socket, "meter.test:8080", Some(&authorization))
        .await
        .unwrap();
    connection.write_all(b"ping").await.unwrap();
    let mut echoed = [0; 4];
    connection.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"ping");
    drop(connection);
    let head = head.await.unwrap();
    assert!(
        head.starts_with("CONNECT meter.test:8080 HTTP/1.1\r\n"),
        "{head}"
    );
    assert!(
        head.contains(&format!(
            "proxy-authorization: Basic {}\r\n",
            STANDARD.encode("user:secret")
        )),
        "{head}"
    );
}

#[tokio::test]
async fn refused_connect_fails_closed() {
    let unused = "127.0.0.1:9".parse().unwrap();
    let (address, _) = proxy(
        "HTTP/1.1 407 Proxy Authentication Required\r\ncontent-length: 0\r\n\r\n",
        unused,
    )
    .await;
    let socket = Box::new(TcpStream::connect(address).await.unwrap());
    let error = tunnel(socket, "meter.test:443", None).await.err().unwrap();
    assert!(error.to_string().contains("407"), "{error}");
}

#[tokio::test]
async fn cleartext_proxy_uses_absolute_form_without_connect() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(stream.read_u8().await.unwrap());
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
            .await
            .unwrap();
        String::from_utf8(head).unwrap()
    });
    let proxy = Proxy::new(&format!("http://user:secret@{address}"), "", "");
    let connection = connect(&proxy, &origin("http://meter.test"), None)
        .await
        .unwrap();
    assert!(connection.absolute_form);
    let (mut sender, driver) =
        hyper::client::conn::http1::handshake(TokioIo::new(connection.stream))
            .await
            .unwrap();
    tokio::spawn(driver);
    let request = http::Request::get("http://meter.test/probe")
        .header(http::header::HOST, "meter.test")
        .header(
            http::header::PROXY_AUTHORIZATION,
            connection.proxy_authorization.unwrap(),
        )
        .body(String::new())
        .unwrap();
    assert!(
        sender
            .send_request(request)
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let head = peer.await.unwrap();
    assert!(
        head.starts_with("GET http://meter.test/probe HTTP/1.1\r\n"),
        "{head}"
    );
    assert!(
        head.contains("proxy-authorization: Basic dXNlcjpzZWNyZXQ="),
        "{head}"
    );
}
