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
    let (certificate, key) = test_identity::generate_identity()?;
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
