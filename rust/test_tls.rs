//! A fresh identity and its matching server/client trust pair for transport tests.
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use std::sync::Arc;

#[path = "test_identity.rs"]
pub(crate) mod test_identity;

pub fn configs(
    host: &str,
    versions: &[&'static rustls::SupportedProtocolVersion],
    alpn: &[&[u8]],
) -> Result<(ServerConfig, ClientConfig), Box<dyn std::error::Error + Send + Sync>> {
    let (certificate, key) = test_identity::generate_identity(host)?;
    let certificate = CertificateDer::from_pem_slice(certificate.as_bytes())?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut server = ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(versions)?
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], PrivateKeyDer::from_pem_slice(key.as_bytes())?)?;
    let mut roots = RootCertStore::empty();
    roots.add(certificate)?;
    let mut client = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    server.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
    client.alpn_protocols = server.alpn_protocols.clone();
    Ok((server, client))
}
