//! The TLS certificate: PEM pair loading and checks, renewal re-reads, TLS 1.3 config, the bounded handshake.

use super::PEER_FAILURES;
use crate::{
    config::{Config, TlsFiles, path_error},
    exchange::EXCHANGE_BOUND,
    log,
    log::rfc3339,
};
use graphite_meter_proto::{
    duration,
    origin::{Host, Scheme},
};
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject},
    server::{ClientHello, ParsedCertificate, ResolvesServerCert},
    sign::CertifiedKey,
};
use std::{
    fmt,
    net::SocketAddr,
    path::Path,
    sync::{
        Arc, PoisonError, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, BufReader},
    time::{Instant, MissedTickBehavior, interval_at, timeout},
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use tokio_util::sync::CancellationToken;

/// Renewed files are read this often.
const RENEWAL_CHECK: Duration = Duration::from_secs(60);
/// A certificate expiring sooner is logged with a warning.
const EXPIRY_WARNING: Duration = Duration::from_secs(30 * 24 * 3600);
/// Socket reads beneath rustls, whose own 4 KiB reads keep Linux receive autotuning from growing the window.
const READ_BYTES: usize = 64 << 10;

/// Whether the buffer budget covers every QUIC connection's floor with a handshake of this many bytes.
type Fits = Box<dyn Fn(usize) -> Result<(), String> + Send + Sync>;

/// The PEM pair every TLS listener serves, replaced only by a complete and valid renewal that the budget covers.
pub struct Certificates {
    files: TlsFiles,
    /// The hosts of the TLS listeners' public origins, which the leaf must cover.
    hosts: Vec<Host>,
    fits: Fits,
    current: RwLock<Arc<CertifiedKey>>,
    /// The current chain's `handshake_bytes`.
    handshake: AtomicUsize,
}

/// A QUIC handshake's hold for a C-byte, N-certificate chain: five flights of C + 5N + 609 bytes, a copy of C + 48N.
pub fn handshake_bytes(chain: &[CertificateDer<'_>]) -> usize {
    let (bytes, count) = (chain.iter().map(|certificate| certificate.len()).sum::<usize>(), chain.len());
    5 * (bytes + 5 * count + 609) + bytes + 48 * count
}

impl fmt::Debug for Certificates {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Certificates")
            .field("files", &self.files)
            .finish()
    }
}

/// The hosts of the public origins configured for TLS listeners.
pub fn covered_hosts(config: &Config) -> Vec<Host> {
    let listeners = config.listeners.iter();
    let tls = listeners.filter(|listener| listener.kind.scheme() == Scheme::Https);
    tls.filter_map(|listener| Some(listener.public_origin.as_ref()?.host.clone()))
        .collect()
}

impl Certificates {
    /// Reads and validates the pair, which must cover `hosts` and whose handshake must `fit`; blocking file reads.
    pub fn load(
        files: TlsFiles,
        hosts: Vec<Host>,
        now: SystemTime,
        fits: impl Fn(usize) -> Result<(), String> + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let (pair, expires) = read(&files, &hosts, now)?;
        let handshake = handshake_bytes(&pair.cert);
        fits(handshake)?;
        log_loaded(expires, now);
        #[cfg(unix)]
        warn_readable(&files.key);
        Ok(Self {
            files,
            hosts,
            fits: Box::new(fits),
            current: RwLock::new(Arc::new(pair)),
            handshake: AtomicUsize::new(handshake),
        })
    }

    /// Re-reads the pair, keeping the current one over a bad or unbudgeted renewal; whether the leaf changed.
    pub fn reload(&self, now: SystemTime) -> Result<bool, String> {
        let (pair, expires) = read(&self.files, &self.hosts, now)?;
        let handshake = handshake_bytes(&pair.cert);
        (self.fits)(handshake)?;
        let mut current = self.current.write().unwrap_or_else(PoisonError::into_inner);
        let changed = current.cert.first() != pair.cert.first();
        *current = Arc::new(pair);
        self.handshake.store(handshake, Ordering::Relaxed);
        drop(current);
        if changed {
            log_loaded(expires, now);
        }
        Ok(changed)
    }

    /// The bytes a QUIC handshake with the current chain holds.
    pub fn handshake_bytes(&self) -> usize {
        self.handshake.load(Ordering::Relaxed)
    }

    /// Re-reads the pair every minute on a blocking thread until `shutdown`.
    pub async fn watch(self: Arc<Self>, shutdown: CancellationToken) {
        let mut ticks = interval_at(Instant::now() + RENEWAL_CHECK, RENEWAL_CHECK);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => return,
                _ = ticks.tick() => {}
            }
            let certificates = self.clone();
            let reloaded = tokio::task::spawn_blocking(move || certificates.reload(SystemTime::now())).await;
            if let Ok(Err(error)) = reloaded {
                log!("[gm:tls] renewal rejected; keeping last valid certificate: {error}");
            }
        }
    }

    /// A TLS 1.3 configuration offering `alpn`, serving the current pair and following the client's suite order.
    pub fn server_config(self: &Arc<Self>, alpn: &[u8]) -> ServerConfig {
        let mut config = ServerConfig::builder_with_provider(graphite_meter_net::provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("ring supports TLS 1.3")
            .with_no_client_auth()
            .with_cert_resolver(self.clone());
        config.alpn_protocols = vec![alpn.to_vec()];
        config
    }

    pub fn acceptor(self: &Arc<Self>, alpn: &[u8]) -> TlsAcceptor {
        TlsAcceptor::from(Arc::new(self.server_config(alpn)))
    }
}

impl ResolvesServerCert for Certificates {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.current.read().unwrap_or_else(PoisonError::into_inner).clone())
    }
}

/// Completes a handshake within the exchange bound; a failure is the peer's, logged at most once a minute.
pub async fn accept<S>(acceptor: &TlsAcceptor, socket: S, peer: SocketAddr) -> Option<TlsStream<BufReader<S>>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let socket = BufReader::with_capacity(READ_BYTES, socket);
    let error = match timeout(EXCHANGE_BOUND, acceptor.accept(socket)).await {
        Ok(Ok(stream)) => return Some(stream),
        Ok(Err(error)) => error.to_string(),
        Err(_) => "timed out".into(),
    };
    PEER_FAILURES.write(format_args!("[gm:http] http: TLS handshake error from {peer}: {error}"));
    None
}

/// The pair, checked as Go's server checks it, and when its leaf expires.
fn read(files: &TlsFiles, hosts: &[Host], now: SystemTime) -> Result<(CertifiedKey, SystemTime), String> {
    let pair = pair(files).map_err(|error| format!("load matching TLS certificate/key: {error}"))?;
    let leaf = &pair.cert[0];
    let (not_before, not_after) = validity(leaf).ok_or("parse TLS leaf certificate: malformed validity")?;
    if now < not_before {
        return Err(format!("TLS certificate is not valid before {}", rfc3339(not_before)));
    }
    if now >= not_after {
        return Err(format!("TLS certificate expired at {}", rfc3339(not_after)));
    }
    let parsed = ParsedCertificate::try_from(leaf).map_err(|error| format!("parse TLS leaf certificate: {error}"))?;
    for host in hosts {
        let (text, name) = match host {
            Host::Name(name) => (name.clone(), ServerName::try_from(name.clone()).ok()),
            Host::Ip(ip) => (ip.to_string(), Some(ServerName::IpAddress((*ip).into()))),
        };
        let covered = name
            .ok_or_else(|| "invalid DNS name".to_string())
            .and_then(|name| rustls::client::verify_server_name(&parsed, &name).map_err(|error| error.to_string()));
        covered.map_err(|error| format!("TLS certificate incompatible with {text}: {error}"))?;
    }
    Ok((pair, not_after))
}

/// Both files read and paired before the leaf is checked, with Go's messages.
fn pair(files: &TlsFiles) -> Result<CertifiedKey, String> {
    let read =
        |path: &Path| std::fs::read(path).map_err(|error| path_error("open", &path.display().to_string(), &error));
    let (certificate, key) = (read(&files.cert)?, read(&files.key)?);
    let chain: Vec<_> = CertificateDer::pem_slice_iter(&certificate)
        .collect::<Result<_, _>>()
        .map_err(|error| format!("tls: {error}"))?;
    if chain.is_empty() {
        return Err("tls: failed to find any PEM data in certificate input".into());
    }
    let key = PrivateKeyDer::from_pem_slice(&key).map_err(|_| "tls: failed to find any PEM data in key input")?;
    CertifiedKey::from_der(chain, key, &graphite_meter_net::provider()).map_err(|error| match error {
        rustls::Error::InconsistentKeys(_) => "tls: private key does not match public key".into(),
        error => format!("tls: {error}"),
    })
}

fn log_loaded(expires: SystemTime, now: SystemTime) {
    log!("[gm:tls] certificate loaded; expires at {}", rfc3339(expires));
    let remaining = expires.duration_since(now).unwrap_or_default();
    if remaining < EXPIRY_WARNING {
        let hours = (remaining + Duration::from_secs(1800)).as_secs() / 3600;
        let rounded = duration::format(Duration::from_secs(hours * 3600));
        log!("[gm:tls] warning: certificate expires in {rounded}");
    }
}

#[cfg(unix)]
fn warn_readable(key: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(metadata) = std::fs::metadata(key) else {
        return;
    };
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        let key = key.display();
        log!("[gm:tls] warning: private key {key} permissions are {mode:04o}; remove group/other access");
    }
}

/// A certificate's notBefore and notAfter, read from its DER.
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
    // The serial number, signature algorithm and issuer precede the validity.
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

/// One DER element: its tag, contents and what follows.
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

/// A UTCTime (`YYMMDDHHMMSSZ`) or GeneralizedTime (`YYYYMMDDHHMMSSZ`).
fn der_time(tag: u8, value: &[u8]) -> Option<SystemTime> {
    let number = |digits: &[u8]| {
        digits
            .iter()
            .try_fold(0_u64, |total, digit| digit.is_ascii_digit().then(|| total * 10 + u64::from(digit - b'0')))
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
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        2 => 28 + u64::from(leap),
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if rest[10] != b'Z' || !(1..=12).contains(&month) || !(1..=month_days).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let year = year.checked_sub(u64::from(month <= 2))?;
    let year_of_era = year % 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let days = (year / 400) * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let seconds = days.checked_sub(719_468)? * 86_400 + hour * 3600 + minute * 60 + second;
    Some(UNIX_EPOCH + Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn der_times_read_both_forms_and_refuse_impossible_dates() {
        let at = |tag, text: &str| der_time(tag, text.as_bytes()).map(rfc3339);
        assert_eq!(at(0x17, "261004235959Z").as_deref(), Some("2026-10-04T23:59:59Z"));
        assert_eq!(at(0x17, "491231235959Z").as_deref(), Some("2049-12-31T23:59:59Z"));
        assert_eq!(at(0x18, "20000229120000Z").as_deref(), Some("2000-02-29T12:00:00Z"));
        for (tag, text) in [
            (0x18, "21000229000000Z"),
            (0x17, "261304000000Z"),
            (0x17, "261004240000Z"),
            (0x17, "2610042359590"),
        ] {
            assert_eq!(at(tag, text), None, "{text}");
        }
    }
}
