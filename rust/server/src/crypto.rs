pub fn provider() -> rustls::crypto::CryptoProvider {
    rustls::crypto::ring::default_provider()
}
