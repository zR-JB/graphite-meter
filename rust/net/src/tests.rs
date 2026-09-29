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
        concat!(
            "corp.example, .sub.example, *.wild.example, pinned.example:8443, 10.0.0.0/8, 011.0.0.0/8, 192.0.2.7, ",
            "[2001:db8::1]:443, bad:port, ::ffff:198.51.100.1, ::ffff:203.0.113.0/120, [2001:db8::2], ",
            "[2001:db8::3]:, *star.example, BÜCHER.example, padded.example:080, 192.0.2.8:080, .",
        ),
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
        // Go's ParseCIDR refuses a leading zero, so that entry names no network.
        ("http://11.1.2.3", false),
        ("http://192.0.2.7:81", true),
        ("https://[2001:db8::1]", true),
        ("https://[2001:db8::1]:8443", false),
        ("https://localhost:9", true),
        ("https://127.0.0.2", true),
        ("https://[::1]", true),
        ("https://meter.example", false),
        // Go matches an IPv4-mapped address as IPv4, a target's and an entry's alike.
        ("http://[::ffff:127.0.0.1]", true),
        ("https://[::ffff:192.0.2.7]", true),
        ("http://[::ffff:10.1.2.3]", true),
        ("https://198.51.100.1", true),
        ("https://203.0.113.9", true),
        // To Go, a bracketed entry without its port is a host name no target matches.
        ("https://[2001:db8::2]", false),
        ("https://[2001:db8::3]:8443", true),
        // Go drops only a leading "*.", compares ports as written, matches an international
        // name in punycode, and takes "." as the suffix of every fully qualified name.
        ("https://star.example", false),
        ("https://a.star.example", false),
        ("https://xn--bcher-kva.example", true),
        ("https://a.xn--bcher-kva.example", true),
        ("http://padded.example", false),
        ("http://padded.example:80", false),
        ("http://padded.example:080", true),
        ("http://192.0.2.8", false),
        ("http://192.0.2.8:080", true),
        ("https://meter.example.", true),
    ] {
        assert_eq!(proxy.bypassed(&origin(raw)), bypassed, "{raw}");
    }
    assert!(Proxy::new("", "", "*").bypassed(&origin("https://meter.example")));
}

#[test]
fn upstreams_default_to_http_carry_decoded_credentials_and_refuse_unknown_schemes() {
    let proxy = Proxy::new("proxy.example:3128", "http://u%3Ar:p%40ss@[2001:db8::2]", "");
    let http = proxy.route(&origin("http://meter.example")).unwrap().as_ref().unwrap();
    assert_eq!(
        (http.origin.key().as_str(), http.authorization.as_deref()),
        ("http://proxy.example:3128", None)
    );
    let https = proxy.route(&origin("https://meter.example")).unwrap().as_ref().unwrap();
    assert_eq!(https.origin.authority(), "[2001:db8::2]");
    assert_eq!(
        https.authorization.as_deref(),
        Some(format!("Basic {}", STANDARD.encode("u:r:p@ss")).as_str())
    );
    let socks = Proxy::new("", "SOCKS5H://u%3Ar:p%40ss%FF@proxy.example", "");
    let socks = socks.route(&origin("https://meter.example")).unwrap().as_ref().unwrap();
    assert_eq!(socks.origin.authority(), "proxy.example:1080");
    assert!(socks.authorization.is_none());
    assert_eq!(
        socks.socks.as_ref().unwrap().user,
        Some((b"u:r".to_vec(), b"p@ss\xff".to_vec()))
    );
    let socks4 = Proxy::new("", "socks4://proxy.example:1080", "");
    assert!(socks4.route(&origin("https://meter.example")).unwrap().is_err());
    assert!(Proxy::new("", "", "").route(&origin("https://meter.example")).is_none());
}

/// Go's ProxyFromEnvironment: uppercase before lowercase, empty values skipped, no ALL_PROXY, and
/// under CGI no HTTP_PROXY for cleartext requests while HTTPS_PROXY still applies.
#[test]
fn environment_proxies_follow_go_variables() {
    fn from(pairs: &'static [(&'static str, &'static str)]) -> Proxy {
        Proxy::from_variables(|name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        })
    }
    let (http, https) = (origin("http://meter.example"), origin("https://meter.example"));
    let via = |proxy: &Proxy, target: &Origin| proxy.route(target).map(|route| route.as_ref().unwrap().origin.key());
    let everything = from(&[
        ("ALL_PROXY", "http://all.example:3128"),
        ("all_proxy", "http://all.example:3128"),
    ]);
    assert_eq!((via(&everything, &http), via(&everything, &https)), (None, None));
    let spelled = from(&[
        ("HTTP_PROXY", "http://upper.example:3128"),
        ("http_proxy", "http://lower.example:3128"),
        ("HTTPS_PROXY", ""),
        ("https_proxy", "lower.example:3129"),
        ("no_proxy", "meter.example"),
    ]);
    assert_eq!(via(&spelled, &http), None);
    let spelled = from(&[
        ("HTTP_PROXY", "http://upper.example:3128"),
        ("http_proxy", "http://lower.example:3128"),
        ("HTTPS_PROXY", ""),
        ("https_proxy", "lower.example:3129"),
    ]);
    assert_eq!(via(&spelled, &http).as_deref(), Some("http://upper.example:3128"));
    assert_eq!(via(&spelled, &https).as_deref(), Some("http://lower.example:3129"));
    let cgi = from(&[
        ("REQUEST_METHOD", "GET"),
        ("HTTP_PROXY", "http://attacker.example"),
        ("HTTPS_PROXY", "http://proxy.example:3128"),
    ]);
    let refused = cgi.route(&http).unwrap().as_ref().err().unwrap();
    assert_eq!(
        refused.to_string(),
        "HTTP_PROXY is not a usable proxy: a CGI request's Proxy header can set it"
    );
    assert_eq!(via(&cgi, &https).as_deref(), Some("http://proxy.example:3128"));
    // Go refuses it before it looks at NO_PROXY or loopback.
    let listed = from(&[
        ("REQUEST_METHOD", "GET"),
        ("HTTP_PROXY", "http://attacker.example"),
        ("NO_PROXY", "meter.example"),
    ]);
    for target in [&http, &origin("http://127.0.0.1:8080")] {
        assert!(listed.route(target).is_some_and(Result::is_err), "{}", target.key());
    }
    let unusable = from(&[("https_proxy", "ftp://proxy.example")]);
    let refused = unusable.route(&https).unwrap().as_ref().err().unwrap();
    assert_eq!(
        refused.to_string(),
        "https_proxy is not a usable proxy: only HTTP, HTTPS and SOCKS5 proxies are supported"
    );
}

async fn proxy(status: &'static str, service: SocketAddr) -> (SocketAddr, tokio::task::JoinHandle<String>) {
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
    let (address, head) = proxy("HTTP/1.1 200 Connection established\r\n\r\n", service_address).await;
    let socket = Box::new(TcpStream::connect(address).await.unwrap());
    let authorization = format!("Basic {}", STANDARD.encode("user:secret"));
    let mut connection = tunnel(socket, "meter.test:8080", Some(&authorization)).await.unwrap();
    connection.write_all(b"ping").await.unwrap();
    let mut echoed = [0; 4];
    connection.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"ping");
    drop(connection);
    let head = head.await.unwrap();
    assert!(head.starts_with("CONNECT meter.test:8080 HTTP/1.1\r\n"), "{head}");
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
    let connection = connect(&proxy, &origin("http://meter.test"), None).await.unwrap();
    assert!(connection.absolute_form);
    let (mut sender, driver) = hyper::client::conn::http1::handshake(TokioIo::new(connection.stream))
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
    assert!(sender.send_request(request).await.unwrap().status().is_success());
    let head = peer.await.unwrap();
    assert!(head.starts_with("GET http://meter.test/probe HTTP/1.1\r\n"), "{head}");
    assert!(head.contains("proxy-authorization: Basic dXNlcjpzZWNyZXQ="), "{head}");
}

#[tokio::test]
async fn socks5_logs_in_and_connects_by_name_then_speaks_origin_form() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let peer = tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        for (expected, reply) in [
            (&b"\x05\x02\x00\x02"[..], &b"\x05\x02"[..]),
            (&b"\x01\x04user\x04p@ss"[..], &b"\x01\x00"[..]),
            (
                &b"\x05\x01\x00\x03\x0ameter.test\x1f\x90"[..],
                &b"\x05\x00\x00\x03\x05proxy\x04\x38"[..],
            ),
        ] {
            let mut sent = vec![0; expected.len()];
            client.read_exact(&mut sent).await.unwrap();
            assert_eq!(sent, expected);
            client.write_all(reply).await.unwrap();
        }
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(client.read_u8().await.unwrap());
        }
        client
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
            .await
            .unwrap();
        String::from_utf8(head).unwrap()
    });
    let proxy = Proxy::new(&format!("socks5://user:p%40ss@{address}"), "", "");
    let connection = connect(&proxy, &origin("http://meter.test:8080"), None).await.unwrap();
    assert!(!connection.absolute_form);
    assert!(connection.proxy_authorization.is_none());
    let (mut sender, driver) = hyper::client::conn::http1::handshake(TokioIo::new(connection.stream))
        .await
        .unwrap();
    tokio::spawn(driver);
    let request = http::Request::get("/probe")
        .header(http::header::HOST, "meter.test:8080")
        .body(String::new())
        .unwrap();
    assert!(sender.send_request(request).await.unwrap().status().is_success());
    let head = peer.await.unwrap();
    assert!(head.starts_with("GET /probe HTTP/1.1\r\n"), "{head}");
    assert!(!head.contains("proxy-authorization"), "{head}");
}

/// Answers a credential-less SOCKS5 greeting with `method` and a CONNECT with `status`.
async fn socks_peer(method: u8, status: u8) -> (SocketAddr, tokio::task::JoinHandle<Vec<u8>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        let mut sent = vec![0; 3];
        client.read_exact(&mut sent).await.unwrap();
        client.write_all(&[5, method]).await.unwrap();
        if method == 0 {
            let mut request = vec![0; 5];
            client.read_exact(&mut request).await.unwrap();
            let rest = match request[3] {
                1 => 3,
                4 => 15,
                _ => usize::from(request[4]),
            };
            request.resize(5 + rest + 2, 0);
            client.read_exact(&mut request[5..]).await.unwrap();
            sent.extend(request);
            client.write_all(&[5, status, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
        }
        sent
    });
    (address, task)
}

#[tokio::test]
async fn socks5_sends_addresses_as_go_does_and_fails_closed() {
    let v6 = "2001:db8::1".parse::<std::net::Ipv6Addr>().unwrap().octets();
    for (target, address) in [
        ("http://192.0.2.1", vec![1, 192, 0, 2, 1, 0, 80]),
        ("http://[::ffff:192.0.2.1]:81", vec![1, 192, 0, 2, 1, 0, 81]),
        ("http://[2001:db8::1]", [&[4][..], &v6[..], &[0, 80][..]].concat()),
    ] {
        let (proxy, sent) = socks_peer(0, 0).await;
        let proxy = Proxy::new(&format!("socks5h://{proxy}"), "", "");
        connect(&proxy, &origin(target), None).await.unwrap();
        assert_eq!(
            sent.await.unwrap(),
            [&[5, 1, 0, 5, 1, 0][..], &address[..]].concat(),
            "{target}"
        );
    }
    for (method, status, reason) in [
        (0xff, 0, "no acceptable authentication methods"),
        (2, 0, "unsupported authentication method 2"),
        (0, 5, "unknown error connection refused"),
    ] {
        let (proxy, _) = socks_peer(method, status).await;
        let proxy = Proxy::new(&format!("socks5://{proxy}"), "", "");
        let error = connect(&proxy, &origin("http://meter.test"), None).await.err().unwrap();
        assert_eq!(error.to_string(), format!("socks connect: {reason}"));
    }
}

/// As Go's dialer, a connection probes an idle peer after 30 s and then every 30 s, so one that
/// died during a sleep or a network change closes instead of hanging the next request.
#[tokio::test]
async fn dialled_connections_probe_idle_peers_like_go() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stream = tcp("127.0.0.1", listener.local_addr().unwrap().port()).await.unwrap();
    let socket = socket2::SockRef::from(&stream);
    assert!(socket.keepalive().unwrap());
    #[cfg(target_os = "linux")]
    {
        assert_eq!(socket.tcp_keepalive_time().unwrap(), Duration::from_secs(30));
        assert_eq!(socket.tcp_keepalive_interval().unwrap(), Duration::from_secs(30));
    }
}

#[path = "../../test_identity.rs"]
mod test_identity;

/// A directory of its own under the system's temporary directory, removed on drop.
struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("graphite-meter-trust-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str, contents: &str) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, contents).unwrap();
        path.to_str().unwrap().to_owned()
    }
    fn dir(&self, name: &str) -> String {
        let path = self.0.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path.to_str().unwrap().to_owned()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn certificate() -> (String, rustls::pki_types::CertificateDer<'static>) {
    use rustls::pki_types::{CertificateDer, pem::PemObject};
    let (pem, _) = test_identity::generate_identity("localhost").unwrap();
    let der = CertificateDer::from_pem_slice(pem.as_bytes()).unwrap();
    (pem, der)
}

/// Go's loadOnDiskRoots: SSL_CERT_FILE replaces the file list alone and SSL_CERT_DIR the
/// directory list alone, so the other defaults still load; missing paths are skipped.
#[test]
fn trust_roots_follow_go_ssl_cert_rules() {
    use std::ffi::OsStr;
    let scratch = Scratch::new("rules");
    let [
        (file_pem, file_root),
        (bundle_pem, bundle_root),
        (dir_pem, dir_root),
        (listed_pem, listed_root),
    ] = [certificate(), certificate(), certificate(), certificate()];
    let named = scratch.file("named.pem", &file_pem);
    let bundle = scratch.file("bundle.pem", &bundle_pem);
    let defaults = scratch.dir("certs");
    std::fs::write(format!("{defaults}/root.pem"), &dir_pem).unwrap();
    let listed = scratch.dir("listed");
    std::fs::write(format!("{listed}/root.pem"), &listed_pem).unwrap();
    let missing = format!("{}/missing", scratch.0.display());
    let files = [missing.as_str(), bundle.as_str(), named.as_str()];
    let roots = |file: Option<&str>, dirs: Option<&str>| {
        let (roots, error) = trust::on_disk_roots(file.map(OsStr::new), dirs.map(OsStr::new), &files, &[&defaults]);
        assert!(error.is_none(), "{error:?}");
        roots
    };
    // Without either variable: the first file that reads, and every default directory.
    assert_eq!(roots(None, None), [bundle_root.clone(), dir_root.clone()]);
    // SSL_CERT_FILE keeps the default directories; SSL_CERT_DIR keeps the default files.
    assert_eq!(roots(Some(&named), None), [file_root.clone(), dir_root.clone()]);
    let both = format!("{listed}:{missing}");
    assert_eq!(roots(None, Some(&both)), [bundle_root, listed_root.clone()]);
    // A missing SSL_CERT_FILE loads no file, yet the directories still load.
    assert_eq!(roots(Some(&missing), None), [dir_root]);
    assert_eq!(roots(Some(&named), Some(&listed)), [file_root, listed_root]);
}

/// A block cut short before its END line loses no root after it: Go's pem.Decode starts over at
/// the next BEGIN line.
#[test]
fn trust_roots_survive_a_block_cut_short() {
    use std::ffi::OsStr;
    let scratch = Scratch::new("cut");
    let [(cut, _), (pem, root)] = [certificate(), certificate()];
    let cut: String = cut.lines().take(3).map(|line| format!("{line}\n")).collect();
    let bundle = scratch.file("bundle.pem", &format!("{cut}{pem}"));
    let (roots, error) = trust::on_disk_roots(Some(OsStr::new(&bundle)), None, &[], &[]);
    assert!(error.is_none(), "{error:?}");
    assert_eq!(roots, [root]);
}

/// c_rehash's links within the directory are skipped, as Go skips them, so each root loads once;
/// a directory that cannot be read is reported beside whatever did load.
#[cfg(unix)]
#[test]
fn trust_roots_skip_hash_links_and_keep_the_first_failure() {
    use std::ffi::OsStr;
    let scratch = Scratch::new("links");
    let (pem, root) = certificate();
    let directory = scratch.dir("certs");
    std::fs::write(format!("{directory}/root.pem"), &pem).unwrap();
    std::os::unix::fs::symlink("root.pem", format!("{directory}/0123abcd.0")).unwrap();
    let (roots, error) = trust::on_disk_roots(None, None, &[], &[&directory]);
    assert_eq!(roots, std::slice::from_ref(&root));
    assert!(error.is_none());
    let not_a_directory = scratch.file("plain", &pem);
    let listed = format!("{not_a_directory}:{directory}");
    let (roots, error) = trust::on_disk_roots(None, Some(OsStr::new(&listed)), &[], &[]);
    assert_eq!(roots, [root]);
    assert!(error.is_some());
}

#[tokio::test]
async fn https_targets_verify_tls_inside_http_and_https_proxy_tunnels() {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
    let (certificate, key) = test_identity::generate_identity("localhost").unwrap();
    let certificate = CertificateDer::from_pem_slice(certificate.as_bytes()).unwrap();
    let key = PrivateKeyDer::from_pem_slice(key.as_bytes()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], key)
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    // Trusting only the test certificate, which no system store holds, it verifies the proxy too.
    let tls = TlsConnector::from(Arc::new(client));
    for secure_proxy in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let acceptor = acceptor.clone();
        let peer = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut stream: Box<dyn Stream> = if secure_proxy {
                Box::new(acceptor.accept(socket).await.unwrap())
            } else {
                Box::new(socket)
            };
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(stream.read_u8().await.unwrap());
            }
            let head = String::from_utf8(head).unwrap();
            assert!(head.starts_with("CONNECT localhost.:443 HTTP/1.1\r\n"), "{head}");
            assert!(head.contains("proxy-authorization: Basic dXNlcjpzZWNyZXQ="));
            stream
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .unwrap();
            let mut stream = acceptor.accept(stream).await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(stream.read_u8().await.unwrap());
            }
            let head = String::from_utf8(head).unwrap();
            assert!(head.starts_with("GET /probe HTTP/1.1\r\n"));
            assert!(!head.contains("proxy-authorization"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let scheme = if secure_proxy { "https" } else { "http" };
        let proxy = Proxy::new("", &format!("{scheme}://user:secret@localhost:{port}"), "");
        tokio::time::timeout(Duration::from_secs(5), async {
            let connection = connect(&proxy, &origin("https://localhost."), Some(&tls))
                .await
                .unwrap();
            assert!(!connection.absolute_form);
            assert!(connection.proxy_authorization.is_none());
            let (mut sender, driver) = hyper::client::conn::http1::handshake(TokioIo::new(connection.stream))
                .await
                .unwrap();
            tokio::spawn(driver);
            let request = http::Request::get("/probe")
                .header(http::header::HOST, "localhost.")
                .body(String::new())
                .unwrap();
            assert!(sender.send_request(request).await.unwrap().status().is_success());
            peer.await.unwrap();
        })
        .await
        .unwrap();
    }
}
