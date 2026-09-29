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

    /// A local HTTP/3 endpoint.
    pub(crate) fn h3_endpoint() -> Result<(quinn::Endpoint, String), crate::Error> {
        let tls = server_tls(&[&rustls::version::TLS13], &[b"h3"])?;
        let tls = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
        let endpoint =
            quinn::Endpoint::server(quinn::ServerConfig::with_crypto(Arc::new(tls)), "127.0.0.1:0".parse()?)?;
        let origin = format!("https://{}", endpoint.local_addr()?);
        Ok((endpoint, origin))
    }
}
