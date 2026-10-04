//! Native client orchestration, separate from terminal rendering.
#![forbid(unsafe_code)]

pub mod cli;
pub mod config;
pub mod crypto;
pub mod download;
pub mod failure;
pub mod latency;
pub mod model;
pub mod net;
pub mod quic;
pub mod report;
pub mod runner;
pub mod selection;
pub mod transport;
pub mod upload;
pub mod vocabulary;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// The release this build reports, as `-version` prints it.
pub const VERSION: &str = match option_env!("GM_ENGINE_VERSION") {
    Some(version) => version,
    None => concat!(env!("CARGO_PKG_VERSION"), "-rust-dev"),
};

mod theme;
mod tls;
pub mod webtransport;

pub mod ui;

pub mod controller;

#[cfg(test)]
#[path = "../../test_identity.rs"]
mod test_identity;

#[cfg(test)]
mod fixtures {
    use std::sync::Arc;

    /// A loopback listener and its `http://` origin, with the crypto provider the client needs installed.
    pub(crate) async fn listener() -> std::io::Result<(tokio::net::TcpListener, String)> {
        let _ = crate::crypto::provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        Ok((listener, origin))
    }

    /// The request head `stream` sends, through its blank line.
    pub(crate) async fn read_head(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> std::io::Result<String> {
        use tokio::io::AsyncReadExt;
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(stream.read_u8().await?);
        }
        Ok(String::from_utf8_lossy(&head).into_owned())
    }

    /// An HTTP/1.1 `200 OK` carrying `body`.
    pub(crate) fn ok(body: &str) -> String {
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len())
    }

    /// A `localhost` server's TLS over `versions`, offering `alpn`; the client takes it only when insecure.
    pub(crate) fn server_tls(
        versions: &[&'static rustls::SupportedProtocolVersion],
        alpn: &[&[u8]],
    ) -> Result<rustls::ServerConfig, crate::Error> {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
        let (certificate, key) = crate::test_identity::generate_identity("localhost")?;
        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(crate::crypto::provider()))
            .with_protocol_versions(versions)?
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from_pem_slice(certificate.as_bytes())?],
                PrivateKeyDer::from_pem_slice(key.as_bytes())?,
            )?;
        tls.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
        Ok(tls)
    }

    /// A local HTTP/3 endpoint, with the crypto provider the client needs installed.
    pub(crate) fn h3_endpoint() -> Result<(quinn::Endpoint, String), crate::Error> {
        let _ = crate::crypto::provider().install_default();
        let tls = server_tls(&[&rustls::version::TLS13], &[b"h3"])?;
        let tls = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
        let endpoint =
            quinn::Endpoint::server(quinn::ServerConfig::with_crypto(Arc::new(tls)), "127.0.0.1:0".parse()?)?;
        let origin = format!("https://{}", endpoint.local_addr()?);
        Ok((endpoint, origin))
    }

    /// The next HTTP/3 connection `endpoint` accepts.
    pub(crate) async fn h3_connection(
        endpoint: &quinn::Endpoint,
    ) -> Result<graphite_meter_http3::server::Connection, crate::Error> {
        let quic = endpoint.accept().await.ok_or("endpoint closed")?.await?;
        Ok(graphite_meter_http3::server::Connection::new(quic, None))
    }

    /// Serves the first WebTransport session `endpoint` accepts with `serve`, driving its connection meanwhile.
    pub(crate) async fn webtransport_peer<F: Future<Output = Result<(), crate::Error>>>(
        endpoint: quinn::Endpoint,
        serve: impl FnOnce(graphite_meter_http3::webtransport::Session) -> F,
    ) -> Result<(), crate::Error> {
        use graphite_meter_http3::webtransport::Session;
        let mut connection = h3_connection(&endpoint).await?;
        let (_, stream) = connection.next().await?.ok_or("no CONNECT")?.resolve().await?;
        let session = async { serve(Session::accept(stream, http::HeaderMap::new()).await?).await };
        let (served, ()) = tokio::join!(session, async { while let Ok(Some(_)) = connection.next().await {} });
        served
    }
}
