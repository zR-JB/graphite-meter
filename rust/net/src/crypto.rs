//! The one crypto provider, ring, and the client TLS configurations built on it.
use crate::trust::{self, Trust};
use graphite_meter_proto::discovery::Protocol;
use rustls::{
    CipherSuite::TLS13_CHACHA20_POLY1305_SHA256, ClientConfig, SupportedProtocolVersion,
    client::danger::ServerCertVerifier, crypto::CryptoProvider,
};
use std::sync::{Arc, LazyLock};
use tokio::sync::OnceCell;

/// ring, preferring ChaCha20-Poly1305 where AES-GCM has no hardware support and runs slower.
pub fn provider() -> Arc<CryptoProvider> {
    static PROVIDER: LazyLock<Arc<CryptoProvider>> = LazyLock::new(|| {
        let mut provider = rustls::crypto::ring::default_provider();
        if !aes_hardware() {
            provider
                .cipher_suites
                .sort_by_key(|suite| suite.suite() != TLS13_CHACHA20_POLY1305_SHA256);
        }
        Arc::new(provider)
    });
    PROVIDER.clone()
}

/// Whether AES-GCM runs in hardware on the architectures the binaries ship for.
pub fn aes_hardware() -> bool {
    cfg_select! {
        any(target_arch = "x86", target_arch = "x86_64") => {
            use std::arch::is_x86_feature_detected as has;
            has!("aes") && has!("pclmulqdq") && has!("sse4.1") && has!("ssse3")
        }
        target_arch = "aarch64" => {
            std::arch::is_aarch64_feature_detected!("aes") && std::arch::is_aarch64_feature_detected!("pmull")
        }
        _ => false,
    }
}

/// How a client checks server certificates.
#[derive(Clone, Debug)]
pub enum Verify {
    /// Against these roots, which their first configuration loads.
    Trusted(Arc<Trust>),
    /// Not at all; handshake signatures are still checked.
    Insecure,
}

impl Verify {
    /// Against the process's roots, which `SSL_CERT_FILE` and `SSL_CERT_DIR` adjust.
    pub fn trusted() -> Self {
        Self::Trusted(Trust::system())
    }
}

/// Client configurations kept per trust: one offering no ALPN, then one per protocol in declaration order.
pub(crate) const CONFIGS: usize = Protocol::ALL.len() + 1;

/// The client configuration for `verify` offering `alpn`, built once so its connections share one session cache.
pub async fn client_config(verify: &Verify, alpn: Option<Protocol>) -> Arc<ClientConfig> {
    static INSECURE: [OnceCell<Arc<ClientConfig>>; CONFIGS] = [const { OnceCell::const_new() }; CONFIGS];
    let build = || async { trust::verifier(verify).await.map(|verifier| configure(verifier, alpn)) };
    let index = alpn.map_or(0, |protocol| protocol as usize + 1);
    let configs = match verify {
        Verify::Trusted(trust) => &trust.configs,
        Verify::Insecure => &INSECURE,
    };
    match configs[index].get_or_try_init(build).await {
        Ok(config) => config.clone(),
        // A trust load that did not finish refuses this connection; the next call loads again.
        Err(cut_short) => configure(trust::refusing(cut_short), alpn),
    }
}

/// `alpn` none offers nothing, for an HTTPS proxy's hop, so the proxy answers CONNECT in HTTP/1.1.
fn configure(verifier: Arc<dyn ServerCertVerifier>, alpn: Option<Protocol>) -> Arc<ClientConfig> {
    let (versions, protocols): (&[&SupportedProtocolVersion], &[&[u8]]) = match alpn {
        None => (rustls::DEFAULT_VERSIONS, &[]),
        Some(Protocol::Http1) => (rustls::DEFAULT_VERSIONS, &[b"http/1.1"]),
        Some(Protocol::Http2) => (rustls::DEFAULT_VERSIONS, &[b"h2"]),
        Some(Protocol::Negotiated) => (rustls::DEFAULT_VERSIONS, &[b"h2", b"http/1.1"]),
        Some(Protocol::Http3) => (&[&rustls::version::TLS13], &[b"h3"]),
    };
    let mut config = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(versions)
        .expect("ring supports every TLS version rustls does")
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    config.alpn_protocols = protocols.iter().map(|protocol| protocol.to_vec()).collect();
    Arc::new(config)
}
