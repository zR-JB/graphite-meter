//! The one crypto provider, ring, and the client TLS configurations built on it.
use crate::trust;
use graphite_meter_proto::discovery::Protocol;
use rustls::{
    CipherSuite::{
        TLS13_AES_128_GCM_SHA256 as AES_128, TLS13_AES_256_GCM_SHA384 as AES_256,
        TLS13_CHACHA20_POLY1305_SHA256 as CHACHA,
    },
    ClientConfig, SupportedProtocolVersion,
    client::danger::ServerCertVerifier,
    crypto::CryptoProvider,
};
use std::sync::{Arc, LazyLock};
use tokio::sync::OnceCell;

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

/// The client configuration for `verify` offering `alpn`, built once so its connections share one session cache.
pub async fn client_config(verify: Verify, alpn: Option<Protocol>) -> Arc<ClientConfig> {
    static CONFIGS: [[OnceCell<Arc<ClientConfig>>; 5]; 2] = [const { [const { OnceCell::const_new() }; 5] }; 2];
    let build = || async { trust::verifier(verify).await.map(|verifier| configure(verifier, alpn)) };
    let index = alpn.map_or(0, |protocol| protocol as usize + 1);
    match CONFIGS[verify as usize][index].get_or_try_init(build).await {
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
