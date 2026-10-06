//! Connections through CONNECT tunnels, HTTP proxies in absolute form and SOCKS5, against in-test peers.
use graphite_meter_net::{Alpn, ConnectError, Connection, Connector, Proxy, RequestForm, Verify};
use graphite_meter_proto::origin::Origin;
use graphite_meter_testkit::Identity;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::Instant,
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

fn connector(variable: &str, proxy: &str, verify: Verify) -> Connector {
    let (variable, proxy) = (variable.to_owned(), proxy.to_owned());
    Connector::new(Proxy::from_lookup(move |name| (name == variable).then(|| proxy.clone())), verify)
}

async fn connect(connector: &Connector, target: &str) -> Result<Connection, ConnectError> {
    connector
        .connect(&Origin::parse(target).unwrap(), Alpn::Negotiated)
        .await
}

/// A peer on a loopback port that runs `serve` on its one connection.
async fn peer<F, T>(serve: impl FnOnce(TcpStream) -> F + Send + 'static) -> (SocketAddr, JoinHandle<T>)
where
    F: Future<Output = T> + Send,
    T: Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (address, tokio::spawn(async move { serve(listener.accept().await.unwrap().0).await }))
}

/// An HTTP/1.1 head, read byte by byte so nothing after it is consumed.
async fn read_head(stream: &mut (impl AsyncRead + Unpin)) -> String {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(stream.read_u8().await.unwrap());
    }
    String::from_utf8(head).unwrap()
}

/// Accepts TLS offering `alpn` and returns what the client offered and the stream.
async fn accept<S: AsyncRead + AsyncWrite + Unpin>(stream: S, alpn: &[&[u8]]) -> (Option<Vec<u8>>, TlsStream<S>) {
    let acceptor = TlsAcceptor::from(Arc::new(Identity::generate().unwrap().server(alpn)));
    let stream = acceptor.accept(stream).await.unwrap();
    (stream.get_ref().1.alpn_protocol().map(<[u8]>::to_vec), stream)
}

#[tokio::test]
async fn https_tunnels_through_connect_with_credentials_and_tls_inside() {
    let (address, proxy) = peer(|mut stream| async move {
        let head = read_head(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await
            .unwrap();
        let (alpn, mut tls) = accept(stream, &[b"h2"]).await;
        tls.write_all(b"inside").await.unwrap();
        (head, alpn)
    })
    .await;
    let connector = connector("HTTPS_PROXY", &format!("http://user:s%40cret@{address}"), Verify::Insecure);
    let mut connection = connect(&connector, "https://meter.test:8443").await.unwrap();
    assert_eq!((connection.alpn.as_deref(), &connection.form), (Some(&b"h2"[..]), &RequestForm::Origin));
    let mut inside = [0; 6];
    connection.stream.read_exact(&mut inside).await.unwrap();
    let (head, alpn) = proxy.await.unwrap();
    assert_eq!(
        head,
        "CONNECT meter.test:8443 HTTP/1.1\r\nHost: meter.test:8443\r\nProxy-Authorization: Basic dXNlcjpzQGNyZXQ=\r\n\r\n"
    );
    assert_eq!((&inside, alpn.as_deref()), (b"inside", Some(&b"h2"[..])));
}

/// A proxy that answers the CONNECT head it reads with `response` and returns the head.
async fn answering(response: Vec<u8>) -> (SocketAddr, JoinHandle<String>) {
    peer(|mut stream| async move {
        let head = read_head(&mut stream).await;
        let _ = stream.write_all(&response).await;
        head
    })
    .await
}

#[tokio::test]
async fn a_connect_response_with_bare_line_feeds_opens_the_tunnel() {
    let (address, _proxy) = peer(|mut stream| async move {
        read_head(&mut stream).await;
        stream.write_all(b"HTTP/1.1 200 OK\nVia: tiny\n\n").await.unwrap();
        accept(stream, &[]).await
    })
    .await;
    let connector = connector("HTTPS_PROXY", &address.to_string(), Verify::Insecure);
    connect(&connector, "https://meter.test").await.unwrap();
}

#[tokio::test]
async fn connect_answers_other_than_200_are_refused() {
    let oversized = [&b"HTTP/1.1 200 OK\r\nX-Padding: "[..], &[b'a'; 64 * 1024]].concat();
    let malformed = "proxy sent a malformed CONNECT response";
    for (authority, response, reason) in [
        (
            "meter.test:443",
            b"HTTP/1.1 407 Proxy Authentication Required\r\ncontent-length: 0\r\n\r\n".to_vec(),
            "proxy refused CONNECT with 407 Proxy Authentication Required",
        ),
        (
            "[2001:db8::1]:8443",
            b"HTTP/1.1 201 Created\r\n\r\n".to_vec(),
            "proxy refused CONNECT with 201 Created",
        ),
        ("meter.test:443", b"HTTP/1.1 OK\r\n\r\n".to_vec(), malformed),
        ("meter.test:443", oversized, "proxy sent an oversized CONNECT response"),
    ] {
        let (address, proxy) = answering(response).await;
        let connector = connector("HTTPS_PROXY", &address.to_string(), Verify::Trusted);
        let refused = connect(&connector, &format!("https://{authority}"))
            .await
            .err()
            .unwrap();
        assert!(matches!(&refused, ConnectError::Refused(text) if text == reason), "{refused:?}");
        let head = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
        assert_eq!(proxy.await.unwrap(), head);
    }
}

#[tokio::test(start_paused = true)]
async fn a_proxy_that_accepts_and_stays_silent_fails_after_a_minute() {
    for scheme in ["http", "https", "socks5"] {
        let (address, _proxy) = peer(|stream| async move {
            std::future::pending::<()>().await;
            drop(stream);
        })
        .await;
        let connector = connector("HTTPS_PROXY", &format!("{scheme}://{address}"), Verify::Trusted);
        let started = Instant::now();
        let refused = connect(&connector, "https://meter.test").await.err().unwrap();
        let ConnectError::Unreachable(error) = &refused else {
            panic!("{scheme}: {refused:?}");
        };
        // Paused time may jump while the loopback connect is in flight, though never past the dial's 9 s.
        let elapsed = started.elapsed();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{scheme}");
        assert!(
            (Duration::from_secs(60)..Duration::from_secs(69)).contains(&elapsed),
            "{scheme}: {elapsed:?}"
        );
    }
}

#[tokio::test]
async fn cleartext_goes_to_an_http_proxy_in_absolute_form_carrying_its_credentials() {
    let (address, proxy) = peer(|mut stream| async move { read_head(&mut stream).await }).await;
    let connector = connector("http_proxy", &format!("user:secret@{address}"), Verify::Trusted);
    let mut connection = connect(&connector, "http://meter.test:8080").await.unwrap();
    let authorization = Some("Basic dXNlcjpzZWNyZXQ=".to_owned());
    assert_eq!((connection.alpn, &connection.form), (None, &RequestForm::Absolute { authorization }));
    let proxy_variable = |name: &str| (name == "HTTP_PROXY").then(|| format!("user:secret@{address}"));
    let printed = format!("{:?} {:?}", connection.form, Proxy::from_lookup(proxy_variable));
    assert!(!printed.contains("dXNlcjpzZWNyZXQ") && !printed.contains("secret"), "{printed}");
    connection
        .stream
        .write_all(b"GET http://meter.test:8080/ HTTP/1.1\r\n\r\n")
        .await
        .unwrap();
    assert!(proxy.await.unwrap().starts_with("GET http://meter.test:8080/ "));
}

/// Answers a SOCKS5 greeting with `method` and a CONNECT with `status`, returning what it was sent.
async fn socks(method: u8, status: u8) -> (SocketAddr, JoinHandle<Vec<u8>>) {
    peer(move |mut stream| async move {
        let mut sent = vec![0; 3];
        stream.read_exact(&mut sent).await.unwrap();
        stream.write_all(&[5, method]).await.unwrap();
        if method == 0 {
            let mut request = vec![0; 5];
            stream.read_exact(&mut request).await.unwrap();
            let rest = match request[3] {
                1 => 3,
                4 => 15,
                _ => usize::from(request[4]),
            };
            request.resize(5 + rest + 2, 0);
            stream.read_exact(&mut request[5..]).await.unwrap();
            sent.extend(request);
            stream.write_all(&[5, status, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
        }
        sent
    })
    .await
}

#[tokio::test]
async fn socks5h_sends_addresses_as_ip_and_names_as_names_and_fails_closed() {
    let v6 = "2001:db8::1".parse::<std::net::Ipv6Addr>().unwrap().octets();
    for (target, address) in [
        ("http://192.0.2.1", vec![1, 192, 0, 2, 1, 0, 80]),
        ("http://[::ffff:192.0.2.1]:81", vec![1, 192, 0, 2, 1, 0, 81]),
        ("http://[2001:db8::1]", [&[4][..], &v6[..], &[0, 80][..]].concat()),
        ("http://meter.test", [&[3, 10][..], b"meter.test", &[0, 80][..]].concat()),
    ] {
        let (proxy, sent) = socks(0, 0).await;
        connect(&connector("HTTP_PROXY", &format!("socks5h://{proxy}"), Verify::Trusted), target)
            .await
            .unwrap();
        assert_eq!(sent.await.unwrap(), [&[5, 1, 0, 5, 1, 0][..], &address[..]].concat(), "{target}");
    }
    for (method, status, reason) in [
        (0xff, 0, "no acceptable authentication methods"),
        (2, 0, "unsupported authentication method 2"),
        (0, 5, "unknown error connection refused"),
    ] {
        let (proxy, _) = socks(method, status).await;
        let connector = connector("HTTP_PROXY", &format!("socks5://{proxy}"), Verify::Trusted);
        let refused = connect(&connector, "http://meter.test").await.err().unwrap();
        assert_eq!(refused.to_string(), format!("socks connect: {reason}"));
    }
}
