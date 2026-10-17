//! The TLS certificate: PEM pair loading and checks, renewal re-reads, TLS 1.3 config, the bounded handshake.

use super::PEER_FAILURES;
use crate::{
    clock,
    config::{Config, TlsFiles, could_not},
    exchange::EXCHANGE_BOUND,
    log,
};
use graphite_meter_proto::origin::{Host, Scheme};
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
    time::{Duration, SystemTime},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, BufReader},
    time::{Instant, MissedTickBehavior, interval_at, timeout},
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

/// Renewed files are read this often.
const RENEWAL_CHECK: Duration = Duration::from_mins(1);
/// A certificate expiring sooner is logged with a warning.
const EXPIRY_WARNING: Duration = Duration::from_hours(30 * 24);

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
fn handshake_bytes(chain: &[CertificateDer<'_>]) -> usize {
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

    /// Re-reads the pair every minute on a blocking thread.
    pub async fn watch(self: Arc<Self>) {
        let mut ticks = interval_at(Instant::now() + RENEWAL_CHECK, RENEWAL_CHECK);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let certificates = self.clone();
            let reloaded = tokio::task::spawn_blocking(move || certificates.reload(SystemTime::now())).await;
            if let Ok(Err(error)) = reloaded {
                log!(Warn, "tls", "certificate renewal rejected: {error}; keeping the current certificate");
            }
        }
    }

    /// A TLS 1.3 configuration offering `alpn`, serving the current pair; without AES hardware ChaCha20-Poly1305 wins.
    pub fn server_config(self: &Arc<Self>, alpn: &[u8]) -> ServerConfig {
        let mut config = ServerConfig::builder_with_provider(graphite_meter_net::provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("ring supports TLS 1.3")
            .with_no_client_auth()
            .with_cert_resolver(self.clone());
        config.alpn_protocols = vec![alpn.to_vec()];
        config.ignore_client_order = !graphite_meter_net::aes_hardware();
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
    let socket = BufReader::with_capacity(graphite_meter_net::TLS_READ_BYTES, socket);
    let error = match timeout(EXCHANGE_BOUND, acceptor.accept(socket)).await {
        Ok(Ok(stream)) => return Some(stream),
        Ok(Err(error)) => error.to_string(),
        Err(_) => "timed out".into(),
    };
    PEER_FAILURES.write(format_args!("TLS handshake from {peer} failed: {error}"));
    None
}

/// The pair, its leaf current and covering `hosts`, and when that leaf expires.
fn read(files: &TlsFiles, hosts: &[Host], now: SystemTime) -> Result<(CertifiedKey, SystemTime), String> {
    let pair = pair(files).map_err(|error| format!("TLS certificate and key unusable: {error}"))?;
    let leaf = &pair.cert[0];
    let (not_before, not_after) =
        validity(leaf).ok_or("TLS certificate unreadable: its validity dates are malformed")?;
    if now < not_before {
        return Err(format!("TLS certificate is not valid before {}", clock::rfc3339(not_before)));
    }
    if now >= not_after {
        return Err(format!("TLS certificate expired at {}", clock::rfc3339(not_after)));
    }
    let parsed = ParsedCertificate::try_from(leaf).map_err(|error| format!("TLS certificate unreadable: {error}"))?;
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

/// Both files read and paired before the leaf is checked.
fn pair(files: &TlsFiles) -> Result<CertifiedKey, String> {
    let read =
        |path: &Path| std::fs::read(path).map_err(|error| could_not(&format!("read {}", path.display()), &error));
    let (certificate, key) = (read(&files.cert)?, read(&files.key)?);
    let chain: Vec<_> = CertificateDer::pem_slice_iter(&certificate)
        .collect::<Result<_, _>>()
        .map_err(|error| format!("certificate file unreadable: {error}"))?;
    if chain.is_empty() {
        return Err("certificate file has no PEM certificate".into());
    }
    let key = PrivateKeyDer::from_pem_slice(&key).map_err(|_| "key file has no PEM private key")?;
    CertifiedKey::from_der(chain, key, &graphite_meter_net::provider()).map_err(|error| match error {
        rustls::Error::InconsistentKeys(_) => "the private key does not match the certificate".into(),
        error => error.to_string(),
    })
}

fn log_loaded(expires: SystemTime, now: SystemTime) {
    log!(Info, "tls", "certificate loaded: expires {}", clock::rfc3339(expires));
    let remaining = expires.duration_since(now).unwrap_or_default();
    if remaining < EXPIRY_WARNING {
        // Whole days from two days on, else whole hours.
        let hours = (remaining + Duration::from_mins(30)).as_secs() / 3600;
        let left = if hours >= 48 {
            format!("{} days", (hours + 12) / 24)
        } else {
            format!("{hours} h")
        };
        log!(Warn, "tls", "certificate expires in {left}; renew it");
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
        log!(Warn, "tls", "private key {key} has permissions {mode:04o}; remove group and other access");
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
    (rest[10] == b'Z').then_some(())?;
    clock::utc([year, month?, day?, hour?, minute?, second?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn der_times_read_both_forms_and_refuse_impossible_dates() {
        let at = |tag, text: &str| der_time(tag, text.as_bytes()).map(clock::rfc3339);
        assert_eq!(at(0x17, "491231235959Z").as_deref(), Some("2049-12-31T23:59:59Z"));
        assert_eq!(at(0x18, "20000229120000Z").as_deref(), Some("2000-02-29T12:00:00Z"));
        for (tag, text) in [(0x18, "21000229000000Z"), (0x17, "2610042359590"), (0x18, "261004235959Z")] {
            assert_eq!(at(tag, text), None, "{text}");
        }
    }
}
