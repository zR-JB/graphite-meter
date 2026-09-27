//! An interop peer for the Go clients, not a measurement server: `interop ADDRESS CERT_PEM KEY_PEM`.
use bytes::Bytes;
use graphite_meter_http3::{Error, RequestStream, server};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::sync::Arc;
use tokio::task::JoinSet;

type Failure = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Failure> {
    let [_, address, certificate, key]: [String; 4] = std::env::args()
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "usage: interop ADDRESS CERT_PEM KEY_PEM")?;
    let certificates = CertificateDer::pem_file_iter(certificate)?.collect::<Result<Vec<_>, _>>()?;
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(certificates, PrivateKeyDer::from_pem_file(key)?)?;
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let config = noq::ServerConfig::with_crypto(Arc::new(noq::crypto::rustls::QuicServerConfig::try_from(tls)?));
    let endpoint = noq::Endpoint::server(config, address.parse()?)?;
    println!("listening {}", endpoint.local_addr()?);
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            Some(incoming) = endpoint.accept() => {
                connections.spawn(async move {
                    if let Err(error) = serve(incoming).await {
                        eprintln!("connection: {error}");
                    }
                });
            }
        }
    }
    endpoint.close(0_u32.into(), b"probe complete");
    connections.shutdown().await;
    endpoint.wait_idle().await;
    Ok(())
}

async fn serve(incoming: noq::Incoming) -> Result<(), Failure> {
    let mut connection = server::Connection::new(incoming.await?, None);
    let mut requests = JoinSet::new();
    while let Some(request) = connection.next().await? {
        requests.spawn(async move {
            let (request, stream) = request.resolve().await?;
            respond(request, stream).await
        });
    }
    Ok(())
}

/// Echoes a request body, or answers `transport probe`.
async fn respond(request: http::Request<()>, stream: RequestStream) -> Result<(), Error> {
    let (mut send, mut recv) = stream.split();
    let mut body = Vec::new();
    while let Some(chunk) = recv.data().await? {
        body.extend_from_slice(&chunk);
    }
    send.send_response(http::Response::new(())).await?;
    if request.method() != http::Method::HEAD {
        let body = if body.is_empty() {
            Bytes::from_static(b"transport probe\n")
        } else {
            body.into()
        };
        send.send_data(body).await?;
    }
    send.finish().await
}
