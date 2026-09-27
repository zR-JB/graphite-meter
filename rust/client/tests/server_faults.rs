use bytes::Bytes;
use futures_util::StreamExt;
use graphite_meter_client::{Error, download::Download, net::Http, transport::Transport};
use graphite_meter_core::discovery::{Protocol, ThroughputTarget, ThroughputTransport};
use http::Response;
use http_body_util::StreamBody;
use hyper::{body::Frame, service::service_fn};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{sync::Arc, time::Duration};
use tokio::{sync::watch, task::JoinSet, time::Instant};

#[path = "../../test_identity.rs"]
mod test_identity;

#[derive(Clone, Copy, Debug)]
enum Peer {
    H2,
    H3,
    WebTransport,
}

#[tokio::test]
async fn silent_download_lanes_stay_open_and_repeated_resets_keep_their_cause() -> Result<(), Error> {
    let _ = graphite_meter_client::crypto::provider().install_default();
    let mut cases = JoinSet::new();
    for peer in [Peer::H2, Peer::H3, Peer::WebTransport] {
        for reset in [false, true] {
            cases.spawn(exercise(peer, reset));
        }
    }
    while let Some(result) = cases.join_next().await {
        result??;
    }
    Ok(())
}

async fn exercise(peer: Peer, reset: bool) -> Result<(), Error> {
    let (fault, trigger) = watch::channel(false);
    let mut servers = JoinSet::new();
    let mut endpoint = None;
    let origin = match peer {
        Peer::H2 => {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let origin = format!("http://{}", listener.local_addr()?);
            servers.spawn(async move {
                let (socket, _) = listener.accept().await?;
                let first = std::sync::atomic::AtomicBool::new(true);
                let service = service_fn(move |_| {
                    let first = first.swap(false, std::sync::atomic::Ordering::Relaxed);
                    let mut trigger = trigger.clone();
                    async move {
                        let chunks = futures_util::stream::once(async move {
                            Ok::<_, std::io::Error>(Frame::data(if first {
                                Bytes::from_static(b"progress")
                            } else {
                                Bytes::new()
                            }))
                        })
                        .chain(futures_util::stream::once(async move {
                            trigger.wait_for(|value| *value).await.unwrap();
                            if reset {
                                Err(std::io::Error::from(std::io::ErrorKind::ConnectionReset))
                            } else {
                                std::future::pending().await
                            }
                        }));
                        Ok::<_, std::convert::Infallible>(Response::new(StreamBody::new(Box::pin(chunks))))
                    }
                });
                hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(socket), service)
                    .await?;
                Ok::<_, Error>(())
            });
            origin
        }
        Peer::H3 | Peer::WebTransport => {
            let (certificate, key) = test_identity::generate_identity("localhost")?;
            let mut tls = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(
                vec![CertificateDer::from_pem_slice(certificate.as_bytes())?],
                PrivateKeyDer::from_pem_slice(key.as_bytes())?,
            )?;
            tls.alpn_protocols = vec![b"h3".to_vec()];
            let config =
                quinn::ServerConfig::with_crypto(Arc::new(quinn::crypto::rustls::QuicServerConfig::try_from(tls)?));
            let server = quinn::Endpoint::server(config, "127.0.0.1:0".parse()?)?;
            let origin = format!("https://{}", server.local_addr()?);
            endpoint = Some(server.clone());
            servers.spawn(async move {
                let quic = server.accept().await.ok_or("endpoint closed")?.await?;
                let mut connection = h3::server::builder()
                    .enable_extended_connect(true)
                    .enable_datagram(true)
                    .enable_webtransport(true)
                    .max_webtransport_sessions(1)
                    .build(h3_noq::Connection::new(quic.clone()))
                    .await?;
                let mut requests = JoinSet::new();
                let mut first = true;
                loop {
                    tokio::select! {
                        result = connection.accept() => {
                            let Some(request) = result? else { break; };
                            let mut trigger = trigger.clone();
                            let quic = quic.clone();
                            let progress = std::mem::replace(&mut first, false);
                            requests.spawn(async move {
                                let (_, mut stream) = request.resolve_request().await?;
                                stream.send_response(Response::new(())).await?;
                                if matches!(peer, Peer::WebTransport) {
                                    let (queue, mut cleanup) = graphite_meter_webtransport::ResetQueue::new(1);
                                    let code = quinn::VarInt::from_u64(0x52e4a40fa8db)?;
                                    let mut data = queue.open(&quic, stream.send_id().into_inner(), code).await?;
                                    data.write_all(b"progress").await?;
                                    trigger.wait_for(|value| *value).await?;
                                    if reset {
                                        data.reset(code);
                                        if let Ok(reset) = cleanup.try_recv() { reset.complete().await?; }
                                    }
                                    std::future::pending::<()>().await;
                                } else {
                                    if progress { stream.send_data(Bytes::from_static(b"progress")).await?; }
                                    trigger.wait_for(|value| *value).await?;
                                    if reset {
                                        stream.stop_stream(h3::error::Code::H3_REQUEST_CANCELLED);
                                    } else {
                                        std::future::pending::<()>().await;
                                    }
                                }
                                Ok::<_, Error>(())
                            });
                        }
                        Some(result) = requests.join_next() => { result??; }
                    }
                }
                Ok(())
            });
            origin
        }
    };
    let http = Http::new(true)?;
    let (_cancel, cancelled) = watch::channel(false);
    let mut download = if matches!(peer, Peer::WebTransport) {
        Download::start_webtransport(
            &http,
            &ThroughputTarget {
                base_url: origin,
                protocol: Protocol::Http3,
                transport: ThroughputTransport::WebTransport,
            },
            1,
            Duration::from_secs(30),
            true,
            cancelled,
        )
        .await?
    } else {
        let transport = Transport::connect(
            http,
            &origin,
            if matches!(peer, Peer::H2) {
                Protocol::Http2
            } else {
                Protocol::Http3
            },
            true,
        )
        .await?;
        Download::start(Arc::new(transport), 1, Duration::from_secs(30), cancelled).await?
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while download.bytes() == 0 {
            download.health()?;
            tokio::task::yield_now().await;
        }
        Ok::<_, Error>(())
    })
    .await??;
    let measured = download.bytes();
    assert_eq!(measured, 8, "{peer:?} counted framing as payload");
    let started = Instant::now();
    fault.send(true)?;
    let retries_end_lane = reset && !matches!(peer, Peer::WebTransport);
    let ended = tokio::time::timeout(Duration::from_millis(3500), async {
        loop {
            if let Err(error) = download.health() {
                break error;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert_eq!(download.bytes(), measured, "{peer:?} counted bytes after the fault");
    match ended {
        Err(_) => assert!(!retries_end_lane, "{peer:?} reset={reset}: lane outlived its retries"),
        Ok(error) => {
            assert!(retries_end_lane, "{peer:?} reset={reset}: {error}");
            assert!(started.elapsed() >= Duration::from_millis(1800), "{peer:?}: {error}");
            let cause = match peer {
                Peer::H2 => error.is::<hyper::Error>(),
                Peer::H3 => matches!(error.downcast_ref::<h3::error::StreamError>(),
                    Some(h3::error::StreamError::RemoteTerminate { code }) if *code == h3::error::Code::H3_REQUEST_CANCELLED),
                Peer::WebTransport => false,
            };
            assert!(cause, "{peer:?}: {error:?}");
        }
    }
    download.stop().await;
    servers.shutdown().await;
    drop(endpoint);
    Ok(())
}
