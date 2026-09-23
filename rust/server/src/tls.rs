//! Validate a complete identity before publishing it to concurrent handshakes.

use crate::config::{Config, ConfigError, NativeKind};
use graphite_meter_core::origin::target_origin;
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject},
    server::{ClientHello, ParsedCertificate, ResolvesServerCert},
    sign::CertifiedKey,
};
use std::{
    fmt,
    future::Future,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::{Duration, SystemTime},
};
use x509_cert::der::Decode;

pub struct Certificates {
    certificate_path: PathBuf,
    key_path: PathBuf,
    advertised_names: Vec<ServerName<'static>>,
    current: RwLock<Arc<CertifiedKey>>,
}

impl fmt::Debug for Certificates {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Certificates")
            .finish_non_exhaustive()
    }
}

impl Certificates {
    /// Poll renewed certificate files without blocking the async executor.
    /// A rejected replacement is reported and retried on the next tick. On
    /// shutdown, any in-flight file read completes before this future returns.
    pub async fn watch(
        self: Arc<Self>,
        shutdown: impl Future<Output = ()>,
        mut report: impl FnMut(Result<bool, ConfigError>),
    ) -> Result<(), ConfigError> {
        let period = Duration::from_secs(60);
        let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => return Ok(()),
                _ = ticks.tick() => {
                    let certificates = self.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        certificates.reload(SystemTime::now())
                    }).await?;
                    report(result);
                }
            }
        }
    }

    pub fn load(config: &Config, now: SystemTime) -> Result<Arc<Self>, ConfigError> {
        let mut names = Vec::new();
        for kind in [NativeKind::H1Tls, NativeKind::H2, NativeKind::H3] {
            let listener = config.listener(kind);
            if listener.address.is_empty() || listener.public_origin.is_empty() {
                continue;
            }
            let origin = target_origin(&listener.public_origin)?.ok_or("expected TLS origin")?;
            names.push(ServerName::try_from(origin.host)?);
        }
        let current = read_identity(&config.tls_cert, &config.tls_key, &names, now)?;
        Ok(Arc::new(Self {
            certificate_path: config.tls_cert.clone().into(),
            key_path: config.tls_key.clone().into(),
            advertised_names: names,
            current: RwLock::new(Arc::new(current)),
        }))
    }

    /// Synchronous file IO: run periodic renewal from an owned blocking worker.
    /// Failure leaves the previous identity intact, including across partial
    /// certificate/key replacement. Existing TLS connections are unaffected.
    pub fn reload(&self, now: SystemTime) -> Result<bool, ConfigError> {
        let replacement = Arc::new(read_identity(
            &self.certificate_path,
            &self.key_path,
            &self.advertised_names,
            now,
        )?);
        let mut current = self.current.write().expect("certificate state poisoned");
        let changed = current.cert != replacement.cert;
        *current = replacement;
        Ok(changed)
    }

    pub fn config(
        self: &Arc<Self>,
        protocols: Vec<Vec<u8>>,
    ) -> Result<Arc<ServerConfig>, ConfigError> {
        let provider = Arc::new(crate::crypto::provider());
        let mut config = ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_cert_resolver(self.clone());
        config.alpn_protocols = protocols;
        Ok(Arc::new(config))
    }
}

impl ResolvesServerCert for Certificates {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(
            self.current
                .read()
                .expect("certificate state poisoned")
                .clone(),
        )
    }
}

fn read_identity(
    certificate_path: impl AsRef<std::path::Path>,
    key_path: impl AsRef<std::path::Path>,
    names: &[ServerName<'static>],
    now: SystemTime,
) -> Result<CertifiedKey, ConfigError> {
    let chain = CertificateDer::pem_file_iter(certificate_path)?.collect::<Result<Vec<_>, _>>()?;
    let leaf = chain.first().ok_or("TLS certificate chain is empty")?;
    let certificate = x509_cert::Certificate::from_der(leaf.as_ref())?;
    let validity = certificate.tbs_certificate().validity();
    if now < validity.not_before.to_system_time() {
        return Err("TLS certificate is not valid yet".into());
    }
    if now >= validity.not_after.to_system_time() {
        return Err("TLS certificate has expired".into());
    }
    let parsed = ParsedCertificate::try_from(leaf)?;
    for name in names {
        rustls::client::verify_server_name(&parsed, name)?;
    }
    let key = PrivateKeyDer::from_pem_file(key_path)?;
    Ok(CertifiedKey::from_der(
        chain,
        key,
        &crate::crypto::provider(),
    )?)
}
