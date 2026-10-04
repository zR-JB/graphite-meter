//! Disposable TLS identities from the openssl CLI, so no key lives in source.
use crate::Error;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

const NEW_KEY: &str = "req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 1";
const CA: &str = "-subj /CN=graphite-meter-test-ca -addext basicConstraints=critical,CA:TRUE \
                  -addext keyUsage=critical,keyCertSign";
const LEAF: &str = "-subj /CN=localhost -addext subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1 \
                    -addext basicConstraints=critical,CA:FALSE -addext extendedKeyUsage=serverAuth";

/// A CA and the leaf it signed for `localhost`, `127.0.0.1` and `::1`, as PEM.
pub struct Identity {
    pub ca: String,
    pub certificate: String,
    pub key: String,
}

impl Identity {
    pub fn generate() -> Result<Self, Error> {
        let scratch = Scratch::new()?;
        let [ca, ca_key, leaf, key] = ["ca.pem", "ca.key", "leaf.pem", "leaf.key"].map(|name| scratch.0.join(name));
        openssl(CA, &[("-keyout", &ca_key), ("-out", &ca)])?;
        openssl(LEAF, &[("-CA", &ca), ("-CAkey", &ca_key), ("-keyout", &key), ("-out", &leaf)])?;
        let read = std::fs::read_to_string;
        Ok(Self { ca: read(ca)?, certificate: read(leaf)?, key: read(key)? })
    }

    /// A TLS 1.3 server config presenting the leaf.
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

    /// A TLS 1.3 client config trusting only the CA.
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

    /// A QUIC server config offering `h3`, with noq's default transport.
    pub fn quic_server(&self) -> noq::ServerConfig {
        let crypto = noq::crypto::rustls::QuicServerConfig::try_from(self.server(&[b"h3"]));
        noq::ServerConfig::with_crypto(Arc::new(crypto.expect("QUIC's initial suite")))
    }

    /// A QUIC client config offering `h3`, with noq's default transport.
    pub fn quic_client(&self) -> noq::ClientConfig {
        let crypto = noq::crypto::rustls::QuicClientConfig::try_from(self.client(&[b"h3"]));
        noq::ClientConfig::new(Arc::new(crypto.expect("QUIC's initial suite")))
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn openssl(subject: &str, files: &[(&str, &Path)]) -> Result<(), Error> {
    let mut command = Command::new("openssl");
    command
        .args(NEW_KEY.split_whitespace())
        .args(subject.split_whitespace());
    for (option, path) in files {
        command.arg(option).arg(path);
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!("openssl failed: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(())
}

/// A private directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> std::io::Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let name = format!("graphite-meter-identity-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let path = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
