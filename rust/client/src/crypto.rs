use rustls::CipherSuite::{
    TLS13_AES_128_GCM_SHA256 as AES_128, TLS13_AES_256_GCM_SHA384 as AES_256, TLS13_CHACHA20_POLY1305_SHA256 as CHACHA,
};

/// Go's `defaultCipherSuitesTLS13` (defaults.go:98-102).
const AES_FIRST: [rustls::CipherSuite; 3] = [AES_128, AES_256, CHACHA];
/// Go's `defaultCipherSuitesTLS13NoAES` (defaults.go:114-118).
const CHACHA_FIRST: [rustls::CipherSuite; 3] = [CHACHA, AES_128, AES_256];

/// ring's suites, with TLS 1.3's in the order Go's client offers them (handshake_client.go:136-139).
pub fn provider() -> rustls::crypto::CryptoProvider {
    let order = if aes_hardware() { AES_FIRST } else { CHACHA_FIRST };
    let mut provider = rustls::crypto::ring::default_provider();
    provider.cipher_suites.sort_by_key(|suite| {
        let rank = order.iter().position(|&id| id == suite.suite());
        rank.unwrap_or(order.len())
    });
    provider
}

/// Go's `hasAESGCMHardwareSupport` (cipher_suites.go:362-368), whose x86 check also runs on 386. Go also counts
/// s390x and ppc64, which report none here.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls13_suites_come_first_in_go_s_order() {
        let suites = provider().cipher_suites;
        let offered: Vec<_> = suites[..3].iter().map(|suite| suite.suite()).collect();
        assert_eq!(offered, if aes_hardware() { AES_FIRST } else { CHACHA_FIRST });
    }
}
