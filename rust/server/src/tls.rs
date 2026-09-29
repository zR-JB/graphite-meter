//! Validate a complete identity before publishing it to concurrent handshakes.

use crate::{
    config::{Config, ConfigError, NativeKind},
    log::rfc3339,
    sync,
};
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

/// Renewed certificate files are read this often.
const RENEWAL_CHECK: Duration = Duration::from_secs(60);
/// A certificate this close to expiry is logged with a warning.
const EXPIRY_WARNING: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// With chain bytes C in N certificates: five server flights of C + 5N + 609 plus a C + 48N clone.
pub fn handshake_bytes(chain: &[CertificateDer<'_>]) -> usize {
    let (bytes, count) = (
        chain.iter().map(|certificate| certificate.len()).sum::<usize>(),
        chain.len(),
    );
    5 * (bytes + 5 * count + 609) + bytes + 48 * count
}

type Budget = Box<dyn Fn(usize) -> Result<(), ConfigError> + Send + Sync>;

pub struct Certificates {
    certificate_path: PathBuf,
    key_path: PathBuf,
    advertised_names: Vec<ServerName<'static>>,
    budget: Budget,
    current: RwLock<Arc<CertifiedKey>>,
}

impl fmt::Debug for Certificates {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Certificates").finish_non_exhaustive()
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
        let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + RENEWAL_CHECK, RENEWAL_CHECK);
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

    pub fn load(
        config: &Config,
        now: SystemTime,
        budget: impl Fn(usize) -> Result<(), ConfigError> + Send + Sync + 'static,
    ) -> Result<Arc<Self>, ConfigError> {
        let mut names = Vec::new();
        for kind in [NativeKind::H1Tls, NativeKind::H2, NativeKind::H3] {
            let listener = config.listener(kind);
            if listener.address.is_empty() || listener.public_origin.is_empty() {
                continue;
            }
            let origin = target_origin(&listener.public_origin)?.ok_or("expected TLS origin")?;
            names.push(ServerName::try_from(origin.host)?);
        }
        let current = read_identity(config.tls_cert.as_ref(), config.tls_key.as_ref(), &names, now)?;
        budget(handshake_bytes(&current.cert))?;
        log_certificate(&current, now);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::metadata(&config.tls_key) {
                let permissions = metadata.permissions().mode() & 0o777;
                if permissions & 0o77 != 0 {
                    crate::log!(
                        "[gm:tls] warning: private key {} permissions are {permissions:04o}; remove group/other access",
                        config.tls_key
                    );
                }
            }
        }
        Ok(Arc::new(Self {
            certificate_path: config.tls_cert.clone().into(),
            key_path: config.tls_key.clone().into(),
            advertised_names: names,
            budget: Box::new(budget),
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
        (self.budget)(handshake_bytes(&replacement.cert))?;
        let mut current = sync::write(&self.current);
        let changed = current.cert != replacement.cert;
        if changed {
            log_certificate(&replacement, now);
        }
        *current = replacement;
        Ok(changed)
    }

    /// The listener that serves a protocol sets its ALPN.
    pub fn config(self: &Arc<Self>) -> Result<Arc<ServerConfig>, ConfigError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        Ok(Arc::new(
            ServerConfig::builder_with_provider(provider)
                .with_protocol_versions(&[&rustls::version::TLS13])?
                .with_no_client_auth()
                .with_cert_resolver(self.clone()),
        ))
    }
}

impl ResolvesServerCert for Certificates {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(sync::read(&self.current).clone())
    }
}

fn log_certificate(identity: &CertifiedKey, now: SystemTime) {
    let (_, expires) = validity(&identity.cert[0]).expect("validated certificate");
    crate::log!("[gm:tls] certificate loaded; expires at {}", rfc3339(expires));
    let remaining = expires.duration_since(now).expect("validated certificate validity");
    if remaining < EXPIRY_WARNING {
        let hours = Duration::from_secs(remaining.as_secs().saturating_add(1800) / 3600 * 3600);
        crate::log!(
            "[gm:tls] warning: certificate expires in {}",
            crate::config::go_duration(hours)
        );
    }
}

fn read_identity(
    certificate_path: &std::path::Path,
    key_path: &std::path::Path,
    names: &[ServerName<'static>],
    now: SystemTime,
) -> Result<CertifiedKey, ConfigError> {
    // Go's LoadX509KeyPair reads both files and pairs them before the leaf is checked.
    let pair = || -> Result<CertifiedKey, ConfigError> {
        let read = |path: &std::path::Path| {
            std::fs::read(path).map_err(|error| crate::config::path_error("open", path.display(), &error))
        };
        let (certificate, key) = (read(certificate_path)?, read(key_path)?);
        let chain: Vec<_> = CertificateDer::pem_slice_iter(&certificate).collect::<Result<_, _>>()?;
        if chain.is_empty() {
            return Err("tls: failed to find any PEM data in certificate input".into());
        }
        let key = PrivateKeyDer::from_pem_slice(&key)?;
        CertifiedKey::from_der(chain, key, &rustls::crypto::ring::default_provider()).map_err(|error| match error {
            rustls::Error::InconsistentKeys(_) => "tls: private key does not match public key".into(),
            error => error.into(),
        })
    };
    let identity = pair().map_err(|error| format!("load matching TLS certificate/key: {error}"))?;
    let leaf = &identity.cert[0];
    let (not_before, not_after) = validity(leaf).ok_or("TLS certificate is malformed")?;
    if now < not_before {
        return Err(format!("TLS certificate is not valid before {}", rfc3339(not_before)).into());
    }
    if now >= not_after {
        return Err(format!("TLS certificate expired at {}", rfc3339(not_after)).into());
    }
    let parsed = ParsedCertificate::try_from(leaf)?;
    for name in names {
        rustls::client::verify_server_name(&parsed, name)
            .map_err(|error| format!("TLS certificate incompatible with {}: {error}", name.to_str()))?;
    }
    Ok(identity)
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
            digit.is_ascii_digit().then(|| total * 10 + u64::from(digit - b'0'))
        })
    };
    let (year, rest) = match (tag, value.len()) {
        (0x17, 13) => {
            let year = number(&value[..2])?;
            (if year >= 50 { 1900 + year } else { 2000 + year }, &value[2..])
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
    let days = (year / 400) * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let seconds = days.checked_sub(719_468)? * 86_400 + hour * 3600 + minute * 60 + second;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
}
