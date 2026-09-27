//! An interop peer for the Go clients, not a measurement server: `interop ADDRESS CERT_PEM KEY_PEM`.
use bytes::Bytes;
use graphite_meter_http3::{Code, Error, RequestStream, WtCode, server, webtransport::Session};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::sync::Arc;
use tokio::{sync::watch, task::JoinSet};

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
    let mut config = noq::ServerConfig::with_crypto(Arc::new(noq::crypto::rustls::QuicServerConfig::try_from(tls)?));
    let mut transport = noq::TransportConfig::default();
    transport.datagram_receive_buffer_size(Some(64 * 1024));
    config.transport_config(Arc::new(transport));
    let endpoint = noq::Endpoint::server(config, address.parse()?)?;
    println!("listening {}", endpoint.local_addr()?);
    let (stop, stopping) = watch::channel(false);
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            Some(incoming) = endpoint.accept() => {
                let stopping = stopping.clone();
                connections.spawn(async move {
                    if let Err(error) = serve(incoming, stopping).await {
                        eprintln!("connection: {error}");
                    }
                });
            }
        }
    }
    // Each connection ends its session with its CLOSE, then closes itself.
    let _ = stop.send(true);
    while connections.join_next().await.is_some() {}
    endpoint.close(Code::H3_NO_ERROR.into(), b"");
    endpoint.wait_idle().await;
    Ok(())
}

async fn serve(incoming: noq::Incoming, mut stopping: watch::Receiver<bool>) -> Result<(), Failure> {
    let mut connection = server::Connection::new(incoming.await?, None);
    let mut requests = JoinSet::new();
    let mut stopped = false;
    loop {
        tokio::select! {
            request = connection.next() => {
                let Some(request) = request? else { return Ok(()) };
                requests.spawn(async move {
                    let (request, stream) = request.resolve().await?;
                    respond(request, stream).await
                });
            }
            // Level-triggered, so a connection that arrives while stopping shuts down too.
            _ = stopping.wait_for(|stop| *stop), if !stopped => {
                stopped = true;
                connection.shutdown(4, "shutdown");
            }
        }
    }
}

/// Echoes a request body, or answers `transport probe`; CONNECT opens a probe session.
async fn respond(request: http::Request<()>, stream: RequestStream) -> Result<(), Error> {
    if request.method() == http::Method::CONNECT {
        return probe(request.uri().path(), Arc::new(Session::accept(stream).await?)).await;
    }
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

/// `/wt/download`, `/wt/reset` and `/wt/close` act at once; every session answers pings and
/// echoes each uploaded stream on a new one.
async fn probe(path: &str, session: Arc<Session>) -> Result<(), Error> {
    match path {
        "/wt/download" => {
            let mut stream = session.open_uni().await?;
            stream.write_all(b"webtransport download\n").await?;
            stream.finish()?;
        }
        "/wt/reset" => session.open_uni().await?.reset(WtCode(7)),
        "/wt/close" => {
            session.close(17, "probe closed").await;
            return Ok(());
        }
        _ => {}
    }
    let mut echoes = JoinSet::new();
    loop {
        tokio::select! {
            closed = session.closed() => {
                closed?;
                break;
            }
            Some(ping) = session.read_datagram() => {
                if let Some(id) = ping.strip_prefix(b"PING,") {
                    session.send_datagram(&[&b"PONG,"[..], id, b",0"].concat())?;
                }
            }
            Some(mut upload) = session.accept_uni() => {
                let session = session.clone();
                echoes.spawn(async move {
                    let mut body = Vec::new();
                    while let Some(chunk) = upload.read_chunk().await? {
                        body.extend_from_slice(&chunk);
                    }
                    let mut echo = session.open_uni().await?;
                    echo.write_all(&body).await?;
                    echo.finish()
                });
            }
        }
    }
    echoes.shutdown().await;
    session.close(0, "").await;
    Ok(())
}
