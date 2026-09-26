mod support;

use bytes::Bytes;
use graphite_meter_server::{config::Config, http_server::HttpServer};
use http::{Request, Version};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject},
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};
use std::{
    error::Error,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tokio_rustls::TlsConnector;

type TestError = Box<dyn Error + Send + Sync>;

struct Tls {
    _identity: support::Identity,
    certificate: CertificateDer<'static>,
    key: Arc<CertifiedKey>,
}

impl Tls {
    fn new() -> Self {
        let identity = support::Identity::generate();
        let certificate =
            CertificateDer::from_pem_file(identity.directory().join("identity.pem")).unwrap();
        let key = PrivateKeyDer::from_pem_file(identity.directory().join("identity.key")).unwrap();
        let provider = graphite_meter_server::crypto::provider();
        let key =
            Arc::new(CertifiedKey::from_der(vec![certificate.clone()], key, &provider).unwrap());
        Self {
            _identity: identity,
            certificate,
            key,
        }
    }

    fn server(&self, resolver: Arc<dyn ResolvesServerCert>) -> ServerConfig {
        let provider = Arc::new(graphite_meter_server::crypto::provider());
        let mut tls = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(resolver);
        tls.alpn_protocols = vec![b"h2".to_vec()];
        tls
    }

    fn client(&self) -> TlsConnector {
        let provider = Arc::new(graphite_meter_server::crypto::provider());
        let mut roots = RootCertStore::empty();
        roots.add(self.certificate.clone()).unwrap();
        let mut tls = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        tls.alpn_protocols = vec![b"h2".to_vec()];
        TlsConnector::from(Arc::new(tls))
    }
}

#[derive(Debug)]
struct PanicOnce(AtomicBool, Arc<CertifiedKey>);
impl ResolvesServerCert for PanicOnce {
    fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        if !self.0.swap(true, Ordering::SeqCst) {
            panic!("injected connection fault");
        }
        Some(self.1.clone())
    }
}

async fn h2_client(
    tls: &Tls,
    address: SocketAddr,
) -> Result<h2::client::SendRequest<Bytes>, TestError> {
    let stream = tls
        .client()
        .connect(
            ServerName::try_from("localhost")?,
            TcpStream::connect(address).await?,
        )
        .await?;
    let (client, connection) = h2::client::handshake(stream).await?;
    tokio::spawn(connection);
    Ok(client)
}

fn request(method: &str, path: &str) -> Request<()> {
    Request::builder()
        .method(method)
        .uri(format!("https://localhost{path}"))
        .version(Version::HTTP_2)
        .body(())
        .unwrap()
}

async fn serve_h2(
    tls: ServerConfig,
    config: Config,
) -> (
    SocketAddr,
    tokio::task::JoinHandle<Result<(), graphite_meter_server::config::ConfigError>>,
    oneshot::Sender<()>,
) {
    let server = Arc::new(HttpServer::new(Arc::new(config)).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.serve_http2(listener, Arc::new(tls), async {
        let _ = stopped.await;
    }));
    (address, task, stop)
}

#[tokio::test]
async fn one_connection_panic_leaves_the_listener_serving() -> Result<(), TestError> {
    let tls = Tls::new();
    let (address, server, stop) = serve_h2(
        tls.server(Arc::new(PanicOnce(AtomicBool::new(false), tls.key.clone()))),
        Config {
            max_connections_per_client: 1,
            ..Config::default()
        },
    )
    .await;
    assert!(
        h2_client(&tls, address).await.is_err(),
        "first handshake must fail"
    );
    let mut client = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(client) = h2_client(&tls, address).await {
                break client;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| "listener stopped after one connection panicked")?;
    client = client.ready().await?;
    let (response, _) = client.send_request(request("GET", "/probe"), true)?;
    assert_eq!(response.await?.status(), 200);
    assert!(!server.is_finished(), "listener ended");
    stop.send(()).ok();
    server.await??;
    Ok(())
}

#[path = "../../test_link.rs"]
mod test_link;
async fn body(response: http::Response<h2::RecvStream>) -> Result<Vec<u8>, TestError> {
    let mut body = response.into_body();
    let mut bytes = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk?;
        body.flow_control().release_capacity(chunk.len())?;
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[tokio::test]
async fn h2_upload_is_not_window_bound_on_a_delayed_link() -> Result<(), TestError> {
    let tls = Tls::new();
    let (address, server, stop) = serve_h2(
        tls.server(Arc::new(Fixed(tls.key.clone()))),
        Config {
            max_operation_duration: Duration::from_secs(10),
            ..Config::default()
        },
    )
    .await;
    let one_way = Duration::from_millis(20);
    let link = test_link::Link::tcp(address, one_way).await?;
    let mut client = h2_client(&tls, link.address).await?;
    client = client.ready().await?;
    let (session, _) = client.send_request(request("POST", "/upload/session"), true)?;
    let session: serde_json::Value = serde_json::from_slice(&body(session.await?).await?)?;
    let id = session["uploadId"].as_str().ok_or("upload id")?.to_owned();

    let mut remaining = Bytes::from(vec![7_u8; 2 * 1024 * 1024]);
    client = client.ready().await?;
    let started = tokio::time::Instant::now();
    let (response, mut upload) =
        client.send_request(request("POST", &format!("/upload?id={id}")), false)?;
    while !remaining.is_empty() {
        upload.reserve_capacity(remaining.len().min(256 * 1024));
        let capacity = std::future::poll_fn(|cx| upload.poll_capacity(cx))
            .await
            .ok_or("upload stream closed")??;
        let chunk = remaining.split_to(capacity.min(remaining.len()));
        upload.send_data(chunk, remaining.is_empty())?;
    }
    let response = response.await?;
    let elapsed = started.elapsed();
    let reply: serde_json::Value = serde_json::from_slice(&body(response).await?)?;
    assert_eq!(reply["bytes"], 2 * 1024 * 1024);
    let round_trips = elapsed.as_secs_f64() / (2.0 * one_way.as_secs_f64());
    eprintln!("h2 2 MiB upload: {elapsed:?} = {round_trips:.1} RTT");
    assert!(
        round_trips < 10.0,
        "window-bound upload: {round_trips:.1} RTT"
    );
    stop.send(()).ok();
    server.await??;
    Ok(())
}

#[derive(Debug)]
struct Fixed(Arc<CertifiedKey>);
impl ResolvesServerCert for Fixed {
    fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }
}
async fn quic_server(
    tls: &Tls,
    config: Config,
) -> Result<(SocketAddr, tokio::task::JoinHandle<()>, oneshot::Sender<()>), TestError> {
    let provider = Arc::new(graphite_meter_server::crypto::provider());
    let mut server_tls = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(Fixed(tls.key.clone())));
    server_tls.alpn_protocols = vec![b"h3".to_vec()];
    let server = Arc::new(HttpServer::new(Arc::new(config))?);
    let endpoint = quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(server.quic_config(Arc::new(server_tls))?),
        graphite_meter_core::socket::udp_socket("127.0.0.1:0".parse()?)?,
        quinn::default_runtime().unwrap(),
    )?;
    let address = endpoint.local_addr()?;
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _ = server
            .serve_quic(endpoint, async {
                let _ = stopped.await;
            })
            .await;
    });
    Ok((address, task, stop))
}

fn quic_client(tls: &Tls) -> Result<quinn::Endpoint, TestError> {
    let provider = Arc::new(graphite_meter_server::crypto::provider());
    let mut roots = RootCertStore::empty();
    roots.add(tls.certificate.clone())?;
    let mut client_tls = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_tls.alpn_protocols = vec![b"h3".to_vec()];
    let endpoint = quinn::Endpoint::client("127.0.0.1:0".parse()?)?;
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_tls)?,
    )));
    Ok(endpoint)
}

#[tokio::test]
async fn quic_retry_only_under_load() -> Result<(), TestError> {
    let tls = Tls::new();
    let (address, server, stop) = quic_server(
        &tls,
        Config {
            max_connections: 8,
            max_connections_per_client: 8,
            ..Config::default()
        },
    )
    .await?;
    let one_way = Duration::from_millis(50);
    let rtt = 2.0 * one_way.as_secs_f64();
    let client = quic_client(&tls)?;
    let mut held = Vec::new();
    for load in 0..3 {
        let link = test_link::Link::udp(address, one_way).await?;
        let started = tokio::time::Instant::now();
        let connection = client.connect(link.address, "localhost")?.await?;
        let round_trips = started.elapsed().as_secs_f64() / rtt;
        eprintln!("handshake with {load} held connections: {round_trips:.2} RTT");
        if load < 2 {
            assert!(round_trips < 1.5, "Retry below a quarter of the limit");
        } else {
            assert!(round_trips >= 1.8, "no Retry at a quarter of the limit");
        }
        held.push((link, connection));
    }
    stop.send(()).ok();
    server.await?;
    Ok(())
}

use tokio::io::{AsyncReadExt, AsyncWriteExt};
async fn echo() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0; 1024];
                while let Ok(count @ 1..) = socket.read(&mut buffer).await {
                    if socket.write_all(&buffer[..count]).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    address
}

#[tokio::test]
async fn tcp_delay_stall_and_reset() {
    let link = test_link::Link::tcp(echo().await, Duration::from_millis(20))
        .await
        .unwrap();
    let mut socket = TcpStream::connect(link.address).await.unwrap();
    let started = tokio::time::Instant::now();
    socket.write_all(b"ping").await.unwrap();
    let mut reply = [0; 4];
    socket.read_exact(&mut reply).await.unwrap();
    let rtt = started.elapsed();
    assert!(
        rtt >= Duration::from_millis(40) && rtt < Duration::from_secs(2),
        "{rtt:?}"
    );
    link.inject(test_link::Fault::Stall);
    socket.write_all(b"ping").await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), socket.read_exact(&mut reply))
            .await
            .is_err()
    );
    link.inject(test_link::Fault::Reset);
    let error = tokio::time::timeout(Duration::from_secs(1), socket.read(&mut reply))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset, "{error}");
    let refused = TcpStream::connect(link.address).await.unwrap();
    let mut refused = refused;
    let result = refused.read(&mut reply).await;
    assert!(
        matches!(result, Err(ref e) if e.kind() == std::io::ErrorKind::ConnectionReset)
            || matches!(result, Ok(0)),
        "{result:?}"
    );
}

#[tokio::test]
async fn udp_delay_and_blackhole() {
    let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let target = server.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buffer = [0; 1500];
        while let Ok((count, from)) = server.recv_from(&mut buffer).await {
            let _ = server.send_to(&buffer[..count], from).await;
        }
    });
    let link = test_link::Link::udp(target, Duration::from_millis(20))
        .await
        .unwrap();
    let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.connect(link.address).await.unwrap();
    let started = tokio::time::Instant::now();
    client.send(b"ping").await.unwrap();
    let mut reply = [0; 16];
    client.recv(&mut reply).await.unwrap();
    let rtt = started.elapsed();
    assert!(
        rtt >= Duration::from_millis(40) && rtt < Duration::from_secs(2),
        "{rtt:?}"
    );
    link.inject(test_link::Fault::Stall);
    client.send(b"ping").await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), client.recv(&mut reply))
            .await
            .is_err()
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "release-only delayed-link throughput gate"]
async fn quic_downloads_exceed_the_old_window_limit() -> Result<(), TestError> {
    if cfg!(debug_assertions) {
        return Err("run this gate with --release".into());
    }
    tokio::time::timeout(Duration::from_secs(60), async {
        for webtransport in [false, true] {
            let baseline = download_rate(webtransport, Duration::ZERO).await?;
            if baseline < 671.0 {
                eprintln!(
                    "inconclusive: webtransport={webtransport}, loopback={baseline:.0} Mbit/s"
                );
                continue;
            }
            let delayed = download_rate(webtransport, Duration::from_millis(50)).await?;
            assert!(
                delayed > 419.0,
                "webtransport={webtransport}: delayed={delayed:.0}, baseline={baseline:.0} Mbit/s"
            );
        }
        Ok::<_, TestError>(())
    })
    .await?
}

async fn download_rate(webtransport: bool, one_way: Duration) -> Result<f64, TestError> {
    use bytes::Buf;
    use h3::ConnectionState;
    use h3::quic::RecvStream as _;
    use std::{future::poll_fn, task::Poll};

    let tls = Tls::new();
    let (address, server, stop) = quic_server(&tls, Config::default()).await?;
    let link = test_link::Link::udp(address, one_way).await?;
    let target = if one_way.is_zero() {
        address
    } else {
        link.address
    };
    let provider = Arc::new(graphite_meter_server::crypto::provider());
    let mut roots = RootCertStore::empty();
    roots.add(tls.certificate.clone())?;
    let mut client_tls = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_tls.alpn_protocols = vec![b"h3".to_vec()];
    let mut config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_tls)?,
    ));
    let mut transport = quinn::TransportConfig::default();
    transport.stream_receive_window((64_u32 << 20).into());
    transport.receive_window((64_u32 << 20).into());
    config.transport_config(Arc::new(transport));
    let endpoint = quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        None,
        graphite_meter_core::socket::udp_socket("127.0.0.1:0".parse()?)?,
        quinn::default_runtime().unwrap(),
    )?;
    let quic = endpoint.connect_with(config, target, "localhost")?.await?;
    let (mut http, mut sender) = h3::client::builder()
        .enable_extended_connect(true)
        .enable_datagram(true)
        .enable_webtransport(true)
        .max_webtransport_sessions(1)
        .build::<_, _, Bytes>(h3_noq::Connection::new(quic.clone()))
        .await?;
    let (ready, ready_rx) = oneshot::channel();
    let (incoming, incoming_rx) = oneshot::channel();
    let mut drivers = tokio::task::JoinSet::new();
    drivers.spawn(async move {
        let (mut ready, mut incoming) = (Some(ready), Some(incoming));
        poll_fn(|cx| {
            if let Poll::Ready(error) = http.poll_close(cx) {
                return Poll::Ready(error);
            }
            if http.settings().enable_webtransport()
                && let Some(ready) = ready.take()
            {
                let _ = ready.send(());
            }
            if let Some((_, stream)) = http.inner.accepted_streams_mut().wt_uni_streams.pop()
                && let Some(incoming) = incoming.take()
            {
                let _ = incoming.send(stream);
            }
            Poll::Pending
        })
        .await
    });
    ready_rx.await?;
    let path = if webtransport {
        "wt/download"
    } else {
        "download"
    };
    let mut request = Request::builder()
        .uri(format!("https://localhost/{path}?bytes=4294967296"))
        .body(())?;
    if webtransport {
        *request.method_mut() = http::Method::CONNECT;
        request
            .extensions_mut()
            .insert(h3::ext::Protocol::WEB_TRANSPORT);
    }
    let mut stream = sender.send_request(request).await?;
    if !webtransport {
        stream.finish().await?;
    }
    assert_eq!(stream.recv_response().await?.status(), 200);
    let mut wt_stream = if webtransport {
        Some(incoming_rx.await?)
    } else {
        None
    };
    let started = tokio::time::Instant::now();
    let mut marks = Vec::new();
    let mut total = 0_u64;
    let mut next = Duration::from_millis(500);
    while started.elapsed() < Duration::from_secs(4) {
        let count = if let Some(stream) = &mut wt_stream {
            poll_fn(|cx| stream.poll_data(cx))
                .await?
                .map(|chunk| chunk.len())
        } else {
            stream.recv_data().await?.map(|chunk| chunk.remaining())
        };
        let Some(count) = count else { break };
        total += count as u64;
        if started.elapsed() >= next {
            marks.push((started.elapsed(), total));
            next = started.elapsed() + Duration::from_millis(500);
        }
    }
    let mut rates = Vec::new();
    for pair in marks.windows(2) {
        let ((before, a), (at, b)) = (pair[0], pair[1]);
        let rate = (b - a) as f64 * 8.0 / (at - before).as_secs_f64() / 1e6;
        eprintln!("webtransport={webtransport} one_way={one_way:?} at={at:?}: {rate:.0} Mbit/s");
        if at >= Duration::from_secs(2) {
            rates.push(rate);
        }
    }
    assert!(rates.len() >= 3, "too few steady-state samples");
    rates.sort_by(f64::total_cmp);
    quic.close(0_u32.into(), b"done");
    stop.send(()).ok();
    server.await?;
    Ok(rates[rates.len() / 2])
}
