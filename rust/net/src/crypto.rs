//! The one crypto provider, ring, and the client TLS configurations built on it.
use crate::trust;
use rustls::{
    CipherSuite::{
        TLS13_AES_128_GCM_SHA256 as AES_128, TLS13_AES_256_GCM_SHA384 as AES_256,
        TLS13_CHACHA20_POLY1305_SHA256 as CHACHA,
    },
    ClientConfig, DigitallySignedStruct, SignatureScheme, SupportedProtocolVersion,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use std::sync::{Arc, LazyLock};

/// Go's TLS 1.3 client order with AES hardware.
const AES_FIRST: [rustls::CipherSuite; 3] = [AES_128, AES_256, CHACHA];
/// Go's TLS 1.3 client order without it.
const CHACHA_FIRST: [rustls::CipherSuite; 3] = [CHACHA, AES_128, AES_256];

/// ring with TLS 1.3's suites first, in the order Go's client offers them.
pub fn provider() -> Arc<CryptoProvider> {
    static PROVIDER: LazyLock<Arc<CryptoProvider>> = LazyLock::new(|| {
        let order = if aes_hardware() { AES_FIRST } else { CHACHA_FIRST };
        let mut provider = rustls::crypto::ring::default_provider();
        provider.cipher_suites.sort_by_key(|suite| {
            let rank = order.iter().position(|&id| id == suite.suite());
            rank.unwrap_or(order.len())
        });
        Arc::new(provider)
    });
    PROVIDER.clone()
}

/// Go's hasAESGCMHardwareSupport on the architectures the binaries ship for.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn aes_hardware() -> bool {
    use std::arch::is_x86_feature_detected as has;
    has!("aes") && has!("pclmulqdq") && has!("sse4.1") && has!("ssse3")
}
#[cfg(target_arch = "aarch64")]
fn aes_hardware() -> bool {
    std::arch::is_aarch64_feature_detected!("aes") && std::arch::is_aarch64_feature_detected!("pmull")
}
#[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
fn aes_hardware() -> bool {
    false
}

/// How a client checks server certificates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verify {
    /// Against the trust store, which the first such configuration loads.
    Trusted,
    /// Not at all; handshake signatures are still checked.
    Insecure,
}

/// A client configuration offering `versions` and `alpn`.
pub async fn client_config(
    verify: Verify,
    versions: &[&'static SupportedProtocolVersion],
    alpn: &[&[u8]],
) -> ClientConfig {
    let verifier = match verify {
        Verify::Trusted => trust::verifier().await,
        Verify::Insecure => Arc::new(Insecure),
    };
    let mut config = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(versions)
        .expect("ring supports every TLS version rustls does")
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    config.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
    config
}

#[derive(Debug)]
struct Insecure;

impl ServerCertVerifier for Insecure {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, certificate, signature, &provider().signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, certificate, signature, &provider().signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider().signature_verification_algorithms.supported_schemes()
    }
}
