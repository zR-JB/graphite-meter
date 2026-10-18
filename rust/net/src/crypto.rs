//! Provides the one crypto provider, ring, and the client TLS configurations built on it.
use crate::trust::{self, Trust};
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

/// Puts ring's TLS 1.3 suites first, cheapest first: AES-128-GCM with AES hardware, ChaCha20-Poly1305 without.
pub fn provider() -> Arc<CryptoProvider> {
    static PROVIDER: LazyLock<Arc<CryptoProvider>> = LazyLock::new(|| {
        let order = if aes_hardware() {
            [AES_128, AES_256, CHACHA]
        } else {
            [CHACHA, AES_128, AES_256]
        };
        let mut provider = rustls::crypto::ring::default_provider();
        provider.cipher_suites.sort_by_key(|suite| {
            order
                .iter()
                .position(|&id| id == suite.suite())
                .unwrap_or(order.len())
        });
        Arc::new(provider)
    });
    PROVIDER.clone()
}

/// Tells whether AES-GCM runs in hardware on the architectures the binaries ship for.
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

/// Says how a client checks server certificates.
#[derive(Clone, Debug)]
pub enum Verify {
    /// The client checks against these roots, which their first configuration loads.
    Trusted(Arc<Trust>),
    /// The client does not check them. Handshake signatures are still checked.
    Insecure,
}

impl Verify {
    /// Checks against the process's roots, which `SSL_CERT_FILE` and `SSL_CERT_DIR` adjust.
    pub fn trusted() -> Self {
        Self::Trusted(Trust::system())
    }
}

/// Each trust keeps this many client configurations, one offering no ALPN and one per protocol in declaration order.
pub(crate) const CONFIGS: usize = Protocol::ALL.len() + 1;

/// Builds the client configuration for `verify` offering `alpn` once, so its connections share one session cache.
pub async fn client_config(verify: &Verify, alpn: Option<Protocol>) -> Arc<ClientConfig> {
    static INSECURE: [OnceCell<Arc<ClientConfig>>; CONFIGS] =
        [const { OnceCell::const_new() }; CONFIGS];
    let build = || async {
        trust::verifier(verify)
            .await
            .map(|verifier| configure(verifier, alpn))
    };
    let index = alpn.map_or(0, |protocol| protocol as usize + 1);
    let configs = match verify {
        Verify::Trusted(trust) => &trust.configs,
        Verify::Insecure => &INSECURE,
    };
    match configs[index].get_or_try_init(build).await {
        Ok(config) => config.clone(),
        // A trust load that did not finish refuses this connection. The next call loads again.
        Err(cut_short) => configure(trust::refusing(cut_short), alpn),
    }
}

/// A missing `alpn` offers nothing, for an HTTPS proxy's hop, so the proxy answers CONNECT in HTTP/1.1.
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
