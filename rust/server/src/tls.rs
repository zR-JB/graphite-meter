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
                    }).await;
                    report(result.unwrap_or_else(|error| Err(error.into())));
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
    let (not_before, not_after) = validity(leaf).ok_or("TLS certificate is malformed")?;
    if now < not_before {
        return Err("TLS certificate is not valid yet".into());
    }
    if now >= not_after {
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

fn validity(certificate: &[u8]) -> Option<(SystemTime, SystemTime)> {
    let (0x30, certificate, _) = der(certificate)? else {
        return None;
    };
    let (0x30, mut fields, _) = der(certificate)? else {
        return None;
    };
    if fields.first() == Some(&0xa0) {
        fields = der(fields)?.2;
    }
    for expected in [0x02, 0x30, 0x30] {
        let (tag, _, rest) = der(fields)?;
        (tag == expected).then_some(())?;
        fields = rest;
    }
    let (0x30, validity, _) = der(fields)? else {
        return None;
    };
    let (first, not_before, rest) = der(validity)?;
    let (second, not_after, rest) = der(rest)?;
    rest.is_empty().then_some(())?;
    Some((der_time(first, not_before)?, der_time(second, not_after)?))
}

fn der(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (length, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let octets = usize::from(first & 0x7f);
        if !(1..=3).contains(&octets) || rest.len() < octets || rest[0] == 0 {
            return None;
        }
        let length = rest[..octets]
            .iter()
            .fold(0, |length, octet| length << 8 | usize::from(*octet));
        (length >= 0x80).then_some(())?;
        (length, &rest[octets..])
    };
    (rest.len() >= length).then(|| (tag, &rest[..length], &rest[length..]))
}

fn der_time(tag: u8, value: &[u8]) -> Option<SystemTime> {
    let number = |digits: &[u8]| {
        digits.iter().try_fold(0_u64, |total, digit| {
            digit
                .is_ascii_digit()
                .then(|| total * 10 + u64::from(digit - b'0'))
        })
    };
    let (year, rest) = match (tag, value.len()) {
        (0x17, 13) => {
            let year = number(&value[..2])?;
            (
                if year >= 50 { 1900 + year } else { 2000 + year },
                &value[2..],
            )
        }
        (0x18, 15) => (number(&value[..4])?, &value[4..]),
        _ => return None,
    };
    let [month, day, hour, minute, second] = [0, 2, 4, 6, 8].map(|at| number(&rest[at..at + 2]));
    let (month, day, hour, minute, second) = (month?, day?, hour?, minute?, second?);
    if rest[10] != b'Z'
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        2 => 28 + u64::from(leap),
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day > month_days {
        return None;
    }
    let year = year.checked_sub(u64::from(month <= 2))?;
    let year_of_era = year % 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let days = (year / 400) * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100
        + day_of_year;
    let seconds = days.checked_sub(719_468)? * 86_400 + hour * 3600 + minute * 60 + second;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
}
