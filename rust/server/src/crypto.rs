//! Select the same TLS and QUIC crypto provider for one server build.

#[cfg(not(any(feature = "crypto-ring", feature = "crypto-aws-lc")))]
compile_error!("enable exactly one server crypto provider");
#[cfg(all(feature = "crypto-ring", feature = "crypto-aws-lc"))]
compile_error!("server crypto providers are mutually exclusive");

#[cfg(feature = "crypto-aws-lc")]
pub fn provider() -> rustls::crypto::CryptoProvider {
    rustls::crypto::aws_lc_rs::default_provider()
}

#[cfg(not(feature = "crypto-aws-lc"))]
pub fn provider() -> rustls::crypto::CryptoProvider {
    rustls::crypto::ring::default_provider()
}
