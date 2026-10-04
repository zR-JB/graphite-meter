//! Disposable TLS identities from the openssl CLI, so no key lives in source.
use crate::{Error, Scratch};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{path::Path, process::Command, sync::Arc};

const NEW_KEY: &str = "req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 1";
const CA: &str = "-subj /CN=graphite-meter-test-ca -addext basicConstraints=critical,CA:TRUE \
                  -addext keyUsage=critical,keyCertSign";
const LEAF: &str = "-subj /CN=localhost -addext subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1 \
                    -addext basicConstraints=critical,CA:FALSE -addext extendedKeyUsage=serverAuth";
const INTERMEDIATE: &str = "-subj /CN=localhost -addext subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1 \
                            -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign";

/// A CA and the certificate it signed, as PEM: for `localhost`, `127.0.0.1` and `::1` unless self-signed.
pub struct Identity {
    pub ca: String,
    pub certificate: String,
    pub key: String,
}

impl Identity {
    /// A CA and a server certificate it signed.
    pub fn generate() -> Result<Self, Error> {
        Self::signed(LEAF)
    }

    /// A CA and a CA certificate it signed, which no TLS client takes as a server's own.
    pub fn intermediate() -> Result<Self, Error> {
        Self::signed(INTERMEDIATE)
    }

    fn signed(subject: &str) -> Result<Self, Error> {
        let scratch = Scratch::new()?;
        let [ca, ca_key, leaf, key] =
            ["ca.pem", "ca.key", "leaf.pem", "leaf.key"].map(|name| scratch.path().join(name));
        openssl(CA, &[("-keyout", &ca_key), ("-out", &ca)])?;
        openssl(subject, &[("-CA", &ca), ("-CAkey", &ca_key), ("-keyout", &key), ("-out", &leaf)])?;
        let read = std::fs::read_to_string;
        Ok(Self { ca: read(ca)?, certificate: read(leaf)?, key: read(key)? })
    }

    /// A certificate for the DNS name `host` that is its own CA, as `openssl req -x509` makes one.
    pub fn self_signed(host: &str) -> Result<Self, Error> {
        let scratch = Scratch::new()?;
        let [certificate, key] = ["self.pem", "self.key"].map(|name| scratch.path().join(name));
        let subject =
            format!("-subj /CN={host} -addext subjectAltName=DNS:{host} -addext basicConstraints=critical,CA:TRUE");
        openssl(&subject, &[("-keyout", &key), ("-out", &certificate)])?;
        let certificate = std::fs::read_to_string(certificate)?;
        Ok(Self {
            ca: certificate.clone(),
            certificate,
            key: std::fs::read_to_string(key)?,
        })
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
