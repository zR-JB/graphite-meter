//! Client TLS configurations, built once on first use; cleartext never builds them.
use crate::Error;
use quinn::crypto::rustls::QuicClientConfig;
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use std::sync::Arc;
use tokio::sync::OnceCell;
use tokio_rustls::TlsConnector;

/// The protocols a TCP connection offers.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Alpn {
    Http1,
    Http2,
    Negotiated,
    /// None, for an HTTPS proxy's hop, so a proxy that speaks HTTP/2 answers CONNECT in HTTP/1.1.
    Proxy,
}

/// TCP takes TLS 1.2 and 1.3, as Go's client does; QUIC requires 1.3.
struct Configs {
    http1: TlsConnector,
    http2: TlsConnector,
    negotiated: TlsConnector,
    proxy: TlsConnector,
    quic: Arc<QuicClientConfig>,
}

impl Configs {
    fn new(verifier: Arc<dyn ServerCertVerifier>) -> Result<Self, Error> {
        let provider = Arc::new(crate::crypto::provider());
        let build = |versions: &[&'static rustls::SupportedProtocolVersion], alpn: &[&[u8]]| {
            let mut tls = rustls::ClientConfig::builder_with_provider(provider.clone())
                .with_protocol_versions(versions)?
                .dangerous()
                .with_custom_certificate_verifier(verifier.clone())
                .with_no_client_auth();
            tls.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
            Ok::<_, Error>(tls)
        };
        let tcp = |alpn: &[&[u8]]| Ok::<_, Error>(TlsConnector::from(Arc::new(build(rustls::DEFAULT_VERSIONS, alpn)?)));
        Ok(Self {
            http1: tcp(&[b"http/1.1"])?,
            http2: tcp(&[b"h2"])?,
            negotiated: tcp(&[b"h2", b"http/1.1"])?,
            proxy: tcp(&[])?,
            quic: Arc::new(QuicClientConfig::try_from(build(&[&rustls::version::TLS13], &[b"h3"])?)?),
        })
    }
}

/// Verified configurations wait for the process's trust store, which loads on first use and
/// fails each connection it cannot verify; skipping verification loads none.
async fn configs(insecure: bool) -> Result<&'static Configs, Error> {
    static VERIFIED: OnceCell<Configs> = OnceCell::const_new();
    static INSECURE: OnceCell<Configs> = OnceCell::const_new();
    if insecure {
        let provider = Arc::new(crate::crypto::provider());
        INSECURE
            .get_or_try_init(|| async { Configs::new(Arc::new(InsecureVerifier { provider })) })
            .await
    } else {
        VERIFIED
            .get_or_try_init(|| async { Configs::new(graphite_meter_net::trust::verifier().await) })
            .await
    }
}

pub(crate) async fn tcp(insecure: bool, alpn: Alpn) -> Result<TlsConnector, Error> {
    let configs = configs(insecure).await?;
    Ok(match alpn {
        Alpn::Http1 => &configs.http1,
        Alpn::Http2 => &configs.http2,
        Alpn::Negotiated => &configs.negotiated,
        Alpn::Proxy => &configs.proxy,
    }
    .clone())
}

pub(crate) async fn quic(insecure: bool) -> Result<Arc<QuicClientConfig>, Error> {
    Ok(configs(insecure).await?.quic.clone())
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
        verify_tls12_signature(message, certificate, signature, &self.provider.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, certificate, signature, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}
