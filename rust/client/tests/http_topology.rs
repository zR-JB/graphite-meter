use graphite_meter_client::{
    Error,
    net::{Http, bounded_body},
};
use graphite_meter_core::discovery::Protocol;
use http::{Method, Response};
use http_body_util::Full;
use hyper::{body::Bytes, service::service_fn};
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{net::TcpListener, sync::Barrier};

#[path = "../../test_identity.rs"]
mod test_identity;

#[tokio::test]
async fn http1_reuses_connections_and_http2_multiplexes_cold_requests() -> Result<(), Error> {
    let _ = graphite_meter_client::crypto::provider().install_default();
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
    let (certificate, key) = test_identity::generate_identity("localhost")?;
    let certificate = CertificateDer::from_pem_slice(certificate.as_bytes())?;
    let key = PrivateKeyDer::from_pem_slice(key.as_bytes())?;
    let server_tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)?;
    for (protocol, secure) in [
        (Protocol::Http1, false),
        (Protocol::Http2, false),
        (Protocol::Http1, true),
        (Protocol::Http2, true),
        (Protocol::Negotiated, true),
    ] {
        let mut server_tls = server_tls.clone();
        server_tls.alpn_protocols = vec![if protocol == Protocol::Http1 {
            b"http/1.1".to_vec()
        } else {
            b"h2".to_vec()
        }];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_tls));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let scheme = if secure { "https" } else { "http" };
        let target = format!("{scheme}://{}/probe", listener.local_addr()?);
        let accepted = Arc::new(AtomicUsize::new(0));
        let count = accepted.clone();
        let barrier = Arc::new(Barrier::new(4));
        let server = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                count.fetch_add(1, Ordering::SeqCst);
                let barrier = barrier.clone();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let socket: Box<dyn graphite_meter_net::Stream> = if secure {
                        Box::new(acceptor.accept(socket).await.unwrap())
                    } else {
                        Box::new(socket)
                    };
                    let service = service_fn(move |_| {
                        let barrier = barrier.clone();
                        async move {
                            if protocol != Protocol::Http1 {
                                barrier.wait().await;
                            }
                            Ok::<_, std::convert::Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
                        }
                    });
                    if protocol != Protocol::Http1 {
                        let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                            .serve_connection(TokioIo::new(socket), service)
                            .await;
                    } else {
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(socket), service)
                            .await;
                    }
                });
            }
        });
        let client = Http::new(secure)?;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            if protocol == Protocol::Http1 {
                for _ in 0..4 {
                    let response = client.request(Method::GET, &target, protocol).await?;
                    assert_eq!(
                        response.version(),
                        if protocol == Protocol::Http1 {
                            http::Version::HTTP_11
                        } else {
                            http::Version::HTTP_2
                        }
                    );
                    assert_eq!(bounded_body(response).await?, b"ok");
                }
            } else {
                let requests = (0..4).map(|_| async {
                    let response = client.request(Method::GET, &target, protocol).await?;
                    assert_eq!(
                        response.version(),
                        if protocol == Protocol::Http1 {
                            http::Version::HTTP_11
                        } else {
                            http::Version::HTTP_2
                        }
                    );
                    assert_eq!(bounded_body(response).await?, b"ok");
                    Ok::<_, Error>(())
                });
                futures_util::future::try_join_all(requests).await?;
            }
            Ok::<_, Error>(())
        })
        .await??;
        server.abort();
        assert_eq!(accepted.load(Ordering::SeqCst), 1, "{protocol:?}");
    }
    Ok(())
}

type Payload = http_body_util::combinators::BoxBody<Bytes, std::convert::Infallible>;

fn json(value: serde_json::Value) -> Payload {
    use http_body_util::BodyExt;
    Full::new(Bytes::from(value.to_string())).boxed()
}

/// A stream that sends `first`, then stays open.
fn open(first: &'static [u8]) -> Payload {
    use http_body_util::{BodyExt, StreamBody};
    let chunks = futures_util::StreamExt::chain(
        futures_util::stream::once(async move { Ok(hyper::body::Frame::data(Bytes::from_static(first))) }),
        futures_util::stream::pending(),
    );
    StreamBody::new(chunks).boxed()
}

/// One HTTP/1.1 measurement server whose downloads answer only once an upload lane arrives.
async fn serve_bidirectional(
    socket: tokio::net::TcpStream,
    uploading: Arc<tokio::sync::watch::Sender<bool>>,
    downloads: Arc<AtomicUsize>,
) {
    use futures_util::StreamExt;
    use http_body_util::BodyExt;
    let service = service_fn(move |request: http::Request<hyper::body::Incoming>| {
        let (uploading, downloads) = (uploading.clone(), downloads.clone());
        async move {
            let route = (request.method().clone(), request.uri().path().to_owned());
            let body = match (route.0.as_str(), route.1.as_str()) {
                ("GET", "/servers") => json(serde_json::json!({
                    "defaultSelection": ["self"],
                    "servers": [{"id": "self", "url": ".", "name": "fixture"}]
                })),
                ("GET", "/preflight") => json(serde_json::json!({
                    "generation": "fixture",
                    "capabilities": {
                        "uploadCheckpoint": true,
                        "throughput": [{"baseUrl": ".", "transport": "fetch-stream", "protocol": "http1"}],
                        "latency": []
                    }
                })),
                ("GET", "/probe") => json(serde_json::json!({
                    "clientIp": "127.0.0.1", "clientIpVersion": 4, "clientIpSource": "socket",
                    "protocolNegotiated": "http/1.1"
                })),
                ("GET", "/download") => {
                    downloads.fetch_add(1, Ordering::SeqCst);
                    let _ = uploading.subscribe().wait_for(|uploading| *uploading).await;
                    open(&[0; 1024])
                }
                ("POST", "/upload/session") => json(serde_json::json!({"uploadId": "fixture"})),
                ("GET", "/upload/progress") => {
                    open(b"{\"type\":\"ready\"}\n{\"type\":\"progress\",\"bytes\":1,\"nanos\":1}\n")
                }
                ("POST", "/upload") => {
                    uploading.send_replace(true);
                    let mut body = request.into_body().into_data_stream();
                    while let Some(Ok(_)) = body.next().await {}
                    json(serde_json::json!({}))
                }
                ("POST", "/upload/checkpoint") => json(serde_json::json!({"bytes": 1, "nanos": 1})),
                _ => json(serde_json::json!({})),
            };
            Ok::<_, std::convert::Infallible>(Response::new(body))
        }
    });
    let _ = hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(socket), service)
        .await;
}

/// A bidirectional stage starts its upload lanes while its download lanes wait for their response
/// headers, as Go's roles start together.
#[tokio::test]
async fn bidirectional_upload_lanes_start_while_downloads_wait_for_headers() -> Result<(), Error> {
    use graphite_meter_client::{
        config::Config,
        model::{Snapshot, Stage},
    };
    let _ = graphite_meter_client::crypto::provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let uploading = Arc::new(tokio::sync::watch::Sender::new(false));
    let mut uploaded = uploading.subscribe();
    let downloads = Arc::new(AtomicUsize::new(0));
    let requested = downloads.clone();
    let server = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(serve_bidirectional(socket, uploading.clone(), requested.clone()));
        }
    });
    let config = Config {
        url: origin,
        stages: vec![Stage::Bidirectional],
        loaded_latency: false,
        warmup: std::time::Duration::ZERO,
        bidirectional_duration: std::time::Duration::from_secs(1),
        streams: 1,
        ..Config::default()
    };
    let (snapshots, _) = tokio::sync::watch::channel(Snapshot::default());
    let (cancel, cancelled) = tokio::sync::watch::channel(false);
    let run = tokio::spawn(graphite_meter_client::runner::run(
        config,
        Http::new(false)?,
        snapshots,
        cancelled,
        None,
    ));
    let started = tokio::time::timeout(std::time::Duration::from_secs(5), uploaded.wait_for(|up| *up)).await;
    cancel.send_replace(true);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), run).await;
    server.abort();
    started.map_err(|_| "upload lanes waited for download readiness")??;
    assert!(downloads.load(Ordering::SeqCst) > 0);
    Ok(())
}

#[tokio::test]
async fn pooled_connections_expire_without_another_request_and_preserve_active_bodies() -> Result<(), Error> {
    use http_body_util::{BodyExt, StreamBody};
    use hyper::body::Frame;
    use std::{convert::Infallible, time::Duration};

    let _ = graphite_meter_client::crypto::provider().install_default();
    for protocol in [Protocol::Http1, Protocol::Http2] {
        for active in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let target = format!("http://{}/probe", listener.local_addr()?);
            let (chunks, receiver) = tokio::sync::mpsc::unbounded_channel();
            chunks.send(Bytes::from_static(b"first"))?;
            let receiver = std::sync::Mutex::new(Some(receiver));
            let server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let service = service_fn(move |_| {
                    let receiver = receiver.lock().unwrap().take().unwrap();
                    async move {
                        let stream = futures_util::stream::unfold(receiver, |mut receiver| async {
                            receiver
                                .recv()
                                .await
                                .map(|chunk| (Ok::<_, Infallible>(Frame::data(chunk)), receiver))
                        });
                        Ok::<_, Infallible>(Response::new(StreamBody::new(Box::pin(stream))))
                    }
                });
                if protocol == Protocol::Http1 {
                    hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(socket), service)
                        .await
                } else {
                    hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(socket), service)
                        .await
                }
            });
            let client = Http::new(false)?;
            let mut response = client.request(Method::GET, &target, protocol).await?;
            assert_eq!(
                response.body_mut().frame().await.unwrap()?.into_data().unwrap(),
                b"first"[..]
            );
            let chunks = if active {
                Some(chunks)
            } else {
                drop(chunks);
                assert!(response.body_mut().collect().await?.to_bytes().is_empty());
                None
            };
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(91)).await;
            tokio::task::yield_now().await;
            tokio::time::resume();
            if let Some(chunks) = chunks {
                assert!(!server.is_finished(), "{protocol:?} closed an active response");
                chunks.send(Bytes::from_static(b"last"))?;
                drop(chunks);
                assert_eq!(bounded_body(response).await?, b"last");
            } else {
                drop(response);
            }
            // A close with bytes still in flight reaches the server as a reset on macOS; either way it ended.
            if let Err(error) = tokio::time::timeout(Duration::from_secs(5), server).await?? {
                let reset = std::error::Error::source(&error)
                    .and_then(|source| source.downcast_ref::<std::io::Error>())
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::ConnectionReset);
                if !reset {
                    return Err(error.into());
                }
            }
            drop(client);
        }
    }
    Ok(())
}

/// Reads one request head.
async fn head(stream: &mut tokio::net::TcpStream) -> Result<(), Error> {
    use tokio::io::AsyncReadExt;
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(stream.read_u8().await?);
    }
    Ok(())
}

/// A bodyless request whose reused connection closes before answering goes once more over a new
/// connection, as Go's transport retries a dead reused connection.
#[tokio::test]
async fn a_bodyless_request_is_replayed_once_when_its_reused_connection_closes() -> Result<(), Error> {
    use tokio::io::AsyncWriteExt;
    let _ = graphite_meter_client::crypto::provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let target = format!("http://{}/probe", listener.local_addr()?);
    let server = tokio::spawn(async move {
        const OK: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok";
        // The first connection answers once, then closes on the next request, as a peer whose
        // idle timeout raced it does.
        let (mut first, _) = listener.accept().await?;
        head(&mut first).await?;
        first.write_all(OK).await?;
        head(&mut first).await?;
        drop(first);
        let (mut second, _) = listener.accept().await?;
        head(&mut second).await?;
        second.write_all(OK).await?;
        Ok::<_, Error>(second)
    });
    let client = Http::new(false)?;
    let answers = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        for _ in 0..2 {
            let response = client.request(Method::GET, &target, Protocol::Http1).await?;
            assert_eq!(bounded_body(response).await?, b"ok");
        }
        Ok::<_, Error>(())
    })
    .await;
    server.abort();
    answers??;
    Ok(())
}

/// An HTTP/2 connection whose request timed out leaves the pool: the next request dials rather
/// than queue on a connection that may be dead.
#[tokio::test]
async fn an_http2_connection_whose_request_timed_out_is_not_reused() -> Result<(), Error> {
    let _ = graphite_meter_client::crypto::provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let target = format!("http://{}/probe", listener.local_addr()?);
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = accepted.clone();
    let server = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            // The first connection answers its first request only.
            let first = count.fetch_add(1, Ordering::SeqCst) == 0;
            let answered = Arc::new(AtomicUsize::new(0));
            let service = service_fn(move |_| {
                let silent = first && answered.fetch_add(1, Ordering::SeqCst) > 0;
                async move {
                    if silent {
                        std::future::pending::<()>().await;
                    }
                    Ok::<_, std::convert::Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
                }
            });
            tokio::spawn(
                hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(socket), service),
            );
        }
    });
    let client = Http::new(false)?;
    let response = client.request(Method::GET, &target, Protocol::Http2).await?;
    assert_eq!(bounded_body(response).await?, b"ok");
    tokio::time::pause();
    let timed_out = client.request(Method::GET, &target, Protocol::Http2).await;
    tokio::time::resume();
    assert!(timed_out.is_err());
    let next = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.request(Method::GET, &target, Protocol::Http2),
    )
    .await;
    server.abort();
    let next = next.map_err(|_| "the timed-out connection was reused")??;
    assert_eq!(bounded_body(next).await?, b"ok");
    assert_eq!(accepted.load(Ordering::SeqCst), 2);
    Ok(())
}

/// A connection that stops reading once `silent` is set.
struct Silenceable {
    inner: tokio::net::TcpStream,
    silent: Arc<std::sync::atomic::AtomicBool>,
}
impl tokio::io::AsyncRead for Silenceable {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.silent.load(Ordering::SeqCst) {
            return std::task::Poll::Pending;
        }
        std::pin::Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}
impl tokio::io::AsyncWrite for Silenceable {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        data: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(context, data)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(context)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

/// An idle HTTP/2 connection whose peer went silent, as after a sleep or a network change, is
/// pinged and closed, so the next request dials instead of waiting on it.
#[tokio::test]
async fn an_idle_http2_connection_whose_peer_went_silent_is_replaced() -> Result<(), Error> {
    let _ = graphite_meter_client::crypto::provider().install_default();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let target = format!("http://{}/probe", listener.local_addr()?);
    let silent = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let accepted = Arc::new(AtomicUsize::new(0));
    let (count, silence) = (accepted.clone(), silent.clone());
    let server = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let first = count.fetch_add(1, Ordering::SeqCst) == 0;
            let io = Silenceable {
                inner: socket,
                silent: if first { silence.clone() } else { Arc::default() },
            };
            let service = service_fn(|_| async {
                Ok::<_, std::convert::Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
            });
            tokio::spawn(
                hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(io), service),
            );
        }
    });
    let client = Http::new(false)?;
    let response = client.request(Method::GET, &target, Protocol::Http2).await?;
    assert_eq!(bounded_body(response).await?, b"ok");
    silent.store(true, Ordering::SeqCst);
    tokio::time::pause();
    for idle in [31, 21] {
        tokio::time::advance(std::time::Duration::from_secs(idle)).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }
    tokio::time::resume();
    let next = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.request(Method::GET, &target, Protocol::Http2),
    )
    .await;
    server.abort();
    let next = next.map_err(|_| "the silent connection was reused")??;
    assert_eq!(bounded_body(next).await?, b"ok");
    assert_eq!(accepted.load(Ordering::SeqCst), 2);
    Ok(())
}
