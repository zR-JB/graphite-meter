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
                            Ok::<_, std::convert::Infallible>(Response::new(Full::new(
                                Bytes::from_static(b"ok"),
                            )))
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

#[tokio::test]
async fn pooled_connections_expire_without_another_request_and_preserve_active_bodies()
-> Result<(), Error> {
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
                response
                    .body_mut()
                    .frame()
                    .await
                    .unwrap()?
                    .into_data()
                    .unwrap(),
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
                assert!(
                    !server.is_finished(),
                    "{protocol:?} closed an active response"
                );
                chunks.send(Bytes::from_static(b"last"))?;
                drop(chunks);
                assert_eq!(bounded_body(response).await?, b"last");
            } else {
                drop(response);
            }
            tokio::time::timeout(Duration::from_secs(5), server).await???;
            drop(client);
        }
    }
    Ok(())
}
