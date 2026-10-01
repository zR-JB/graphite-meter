//! One fresh localhost identity and TLS 1.3 trust pair for transport tests.
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use std::sync::Arc;

#[path = "../test_identity.rs"]
pub(crate) mod test_identity;

pub fn configs(alpn: &[u8]) -> (ServerConfig, ClientConfig) {
    let (certificate, key) = test_identity::generate_identity("localhost").unwrap();
    let certificate = CertificateDer::from_pem_slice(certificate.as_bytes()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut server = ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.clone()],
            PrivateKeyDer::from_pem_slice(key.as_bytes()).unwrap(),
        )
        .unwrap();
    let mut roots = RootCertStore::empty();
    roots.add(certificate).unwrap();
    let mut client = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    server.alpn_protocols = vec![alpn.to_vec()];
    client.alpn_protocols = server.alpn_protocols.clone();
    (server, client)
}
