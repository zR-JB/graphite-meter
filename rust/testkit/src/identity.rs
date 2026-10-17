//! Generates disposable TLS identities in memory, so no key lives in source.
use crate::Error;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{sync::Arc, time::SystemTime};

/// Holds a CA and the certificate it signed as PEM, for `localhost`, `127.0.0.1` and `::1` unless self-signed.
pub struct Identity {
    pub ca: String,
    pub certificate: String,
    pub key: String,
}

/// The certificate is valid from the start of last year to the end of next year.
fn params(names: &[&str], common_name: &str, ca: bool) -> Result<CertificateParams, Error> {
    let mut params = CertificateParams::new(names.iter().map(|name| name.to_string()).collect::<Vec<_>>())?;
    params.distinguished_name.push(DnType::CommonName, common_name);
    let year = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_secs() / 31_556_952 + 1970;
    let year = i32::try_from(year)?;
    (params.not_before, params.not_after) =
        (rcgen::date_time_ymd(year - 1, 1, 1), rcgen::date_time_ymd(year + 1, 12, 31));
    if ca {
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    } else {
        params.is_ca = IsCa::ExplicitNoCa;
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    }
    Ok(params)
}

const LOCAL: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

impl Identity {
    /// Generates a CA and a server certificate it signed.
    pub fn generate() -> Result<Self, Error> {
        Self::signed(params(&LOCAL, "localhost", false)?)
    }

    /// Generates a CA and a CA certificate it signed, which no TLS client takes as a server's own.
    pub fn intermediate() -> Result<Self, Error> {
        Self::signed(params(&LOCAL, "localhost", true)?)
    }

    fn signed(leaf: CertificateParams) -> Result<Self, Error> {
        let ca = CertifiedIssuer::self_signed(params(&[], "graphite-meter-test-ca", true)?, KeyPair::generate()?)?;
        let key = KeyPair::generate()?;
        let certificate = leaf.signed_by(&key, &ca)?;
        Ok(Self {
            ca: ca.pem(),
            certificate: certificate.pem(),
            key: key.serialize_pem(),
        })
    }

    /// Generates a certificate for the DNS name `host` that is its own CA.
    pub fn self_signed(host: &str) -> Result<Self, Error> {
        let key = KeyPair::generate()?;
        let mut params = params(&[host], host, true)?;
        params.key_usages.clear();
        let certificate = params.self_signed(&key)?.pem();
        Ok(Self {
            ca: certificate.clone(),
            certificate,
            key: key.serialize_pem(),
        })
    }
    /// Builds a TLS 1.3 server config presenting the leaf.
    pub fn server(&self, alpn: &[&[u8]]) -> rustls::ServerConfig {
        let chain = CertificateDer::pem_slice_iter(self.certificate.as_bytes()).collect::<Result<_, _>>();
        let key = PrivateKeyDer::from_pem_slice(self.key.as_bytes()).expect("generated key");
        let mut config = rustls::ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("ring supports TLS 1.3")
            .with_no_client_auth()
            .with_single_cert(chain.expect("generated certificate"), key)
            .expect("matching pair");
        config.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
        config
    }

    /// Builds a TLS 1.3 client config trusting only the CA.
    pub fn client(&self, alpn: &[&[u8]]) -> rustls::ClientConfig {
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from_pem_slice(self.ca.as_bytes()).expect("generated CA"))
            .expect("valid CA");
        let mut config = rustls::ClientConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("ring supports TLS 1.3")
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
        config
    }

    /// Builds a QUIC server config offering `h3`, with noq's default transport.
    pub fn quic_server(&self) -> noq::ServerConfig {
        let crypto = noq::crypto::rustls::QuicServerConfig::try_from(self.server(&[b"h3"]));
        noq::ServerConfig::with_crypto(Arc::new(crypto.expect("QUIC's initial suite")))
    }

    /// Builds a QUIC client config offering `h3`, with noq's default transport.
    pub fn quic_client(&self) -> noq::ClientConfig {
        let crypto = noq::crypto::rustls::QuicClientConfig::try_from(self.client(&[b"h3"]));
        noq::ClientConfig::new(Arc::new(crypto.expect("QUIC's initial suite")))
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}
