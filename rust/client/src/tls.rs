//! Shared client TLS trust and signature verification.
use crate::Error;
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use rustls_platform_verifier::BuilderVerifierExt;
use std::sync::Arc;
pub(crate) fn config(insecure: bool) -> Result<rustls::ClientConfig, Error> {
    let mut tls = build(insecure, &[&rustls::version::TLS13])?;
    tls.alpn_protocols = vec![b"h3".to_vec()];
    Ok(tls)
}

pub(crate) fn tcp_config(insecure: bool, alpn: &[&[u8]]) -> Result<rustls::ClientConfig, Error> {
    let mut tls = build(insecure, rustls::DEFAULT_VERSIONS)?;
    tls.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
    Ok(tls)
}

fn build(
    insecure: bool,
    versions: &[&'static rustls::SupportedProtocolVersion],
) -> Result<rustls::ClientConfig, Error> {
    let provider = Arc::new(crate::crypto::provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(versions)?;
    let tls = if insecure {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(InsecureVerifier { provider }))
            .with_no_client_auth()
    } else {
        builder.with_platform_verifier()?.with_no_client_auth()
    };
    Ok(tls)
}

/// Explicit opt-in bypasses certificate trust/name checks only. Handshake
/// signatures still use the configured provider's supported verification schemes.
#[derive(Debug)]
struct InsecureVerifier {
    provider: Arc<CryptoProvider>,
}
impl ServerCertVerifier for InsecureVerifier {
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
        verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
