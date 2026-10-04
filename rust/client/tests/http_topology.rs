use graphite_meter_client::{
    Error,
    net::{Http, bounded_body},
};
use graphite_meter_core::discovery::Protocol;
use http::{Method, Response};
use http_body_util::Full;
use hyper::{body::Bytes, service::service_fn};
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::net::TcpListener;

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
        warmup: Duration::ZERO,
        bidirectional_duration: Duration::from_secs(1),
        streams: 1,
        ..Config::default()
    };
    let (snapshots, _) = tokio::sync::watch::channel(Snapshot::default());
    let (cancel, cancelled) = tokio::sync::watch::channel(false);
    let http = Http::new(false)?;
    let run = tokio::spawn(graphite_meter_client::runner::run(config, http, snapshots, cancelled, None));
    let started = tokio::time::timeout(Duration::from_secs(5), uploaded.wait_for(|up| *up)).await;
    cancel.send_replace(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), run).await;
    server.abort();
    started.map_err(|_| "upload lanes waited for download readiness")??;
    assert!(downloads.load(Ordering::SeqCst) > 0);
    Ok(())
}

#[tokio::test]
async fn pooled_connections_expire_without_another_request_and_preserve_active_bodies() -> Result<(), Error> {
    use http_body_util::{BodyExt, StreamBody};
    use hyper::body::Frame;
    use std::convert::Infallible;

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
            assert_eq!(response.body_mut().frame().await.unwrap()?.into_data().unwrap(), b"first"[..]);
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
