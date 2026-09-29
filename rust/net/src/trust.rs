//! Server certificate trust, loaded once on first use, as Go's crypto/x509 loads its roots.
use rustls::{
    CertificateError, DigitallySignedStruct, OtherError, RootCertStore, SignatureScheme,
    client::{
        WebPkiServerVerifier,
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        verify_server_name,
    },
    crypto::{CryptoProvider, WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, ServerName, UnixTime, pem::PemObject},
    server::ParsedCertificate,
};
use std::{
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};

/// Go's Linux roots: the first of these files that reads, and every file in these directories.
#[cfg(target_os = "linux")]
const CERT_FILES: &[&str] = &[
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/ca-bundle.pem",
    "/etc/pki/tls/cacert.pem",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    "/etc/ssl/cert.pem",
];
#[cfg(target_os = "linux")]
const CERT_DIRECTORIES: &[&str] = &["/etc/ssl/certs", "/etc/pki/tls/certs"];

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The signature algorithms webpki's verifier checks handshakes with.
static ALGORITHMS: LazyLock<WebPkiSupportedAlgorithms> = LazyLock::new(|| provider().signature_verification_algorithms);

/// The verifier every verified connection shares. The first caller loads the trust store on a
/// blocking thread and later callers wait for it. A store that cannot be loaded does not stop
/// the process: each connection it would verify fails with the reason.
pub async fn verifier() -> Arc<dyn ServerCertVerifier> {
    static VERIFIER: tokio::sync::OnceCell<Arc<dyn ServerCertVerifier>> = tokio::sync::OnceCell::const_new();
    VERIFIER
        .get_or_init(|| async {
            tokio::task::spawn_blocking(system)
                .await
                .unwrap_or_else(|error| untrusted(Some(Arc::new(error))))
        })
        .await
        .clone()
}

/// A set, non-empty variable; Go ignores empty ones.
fn variable(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

/// On Linux, Go reads the on-disk roots: SSL_CERT_FILE replaces only the file list and
/// SSL_CERT_DIR only the directories.
#[cfg(target_os = "linux")]
fn system() -> Arc<dyn ServerCertVerifier> {
    let (roots, error) = on_disk_roots(
        variable("SSL_CERT_FILE").as_deref(),
        variable("SSL_CERT_DIR").as_deref(),
        CERT_FILES,
        CERT_DIRECTORIES,
    );
    verifying(roots, error)
}

/// On macOS and Windows, Go since 1.27 verifies with the platform unless SSL_CERT_FILE or
/// SSL_CERT_DIR is set, and then trusts only the roots they name.
#[cfg(any(target_vendor = "apple", windows))]
fn system() -> Arc<dyn ServerCertVerifier> {
    let (file, directories) = (variable("SSL_CERT_FILE"), variable("SSL_CERT_DIR"));
    if file.is_none() && directories.is_none() {
        return match rustls_platform_verifier::Verifier::new(provider()) {
            Ok(verifier) => Arc::new(verifier),
            Err(error) => untrusted(Some(Arc::new(error))),
        };
    }
    let (roots, error) = on_disk_roots(file.as_deref(), directories.as_deref(), &[], &[]);
    verifying(roots, error)
}

/// Platforms the client does not ship for keep the platform verifier's own rules.
#[cfg(not(any(target_os = "linux", target_vendor = "apple", windows)))]
fn system() -> Arc<dyn ServerCertVerifier> {
    match rustls_platform_verifier::Verifier::new(provider()) {
        Ok(verifier) => Arc::new(verifier),
        Err(error) => untrusted(Some(Arc::new(error))),
    }
}

pub(crate) fn verifying(roots: Vec<CertificateDer<'static>>, error: Option<io::Error>) -> Arc<dyn ServerCertVerifier> {
    let mut store = RootCertStore::empty();
    store.add_parsable_certificates(roots.iter().cloned());
    if store.is_empty() {
        return untrusted(error.map(|error| Arc::new(RootsUnavailable(error)) as _));
    }
    match WebPkiServerVerifier::builder_with_provider(Arc::new(store), provider()).build() {
        Ok(webpki) => Arc::new(Verifier {
            webpki: Ok(webpki),
            roots,
        }),
        Err(error) => untrusted(Some(Arc::new(error))),
    }
}

/// webpki's verifier over the roots, where Go's x509 agrees. Go takes a leaf that is itself a trusted root as a
/// chain of its own, as `openssl req -x509` makes one, where webpki refuses every CA as a leaf, and a self-issued
/// CA it does not trust reads as Go reads it: signed by an unknown authority. Without roots every certificate is
/// refused, as Go refuses one signed by an unknown authority, or with the reason the store could not be read.
#[derive(Debug)]
struct Verifier {
    webpki: Result<Arc<WebPkiServerVerifier>, Option<Arc<dyn std::error::Error + Send + Sync>>>,
    roots: Vec<CertificateDer<'static>>,
}
impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let webpki = match &self.webpki {
            Ok(webpki) => webpki,
            Err(Some(reason)) => return Err(CertificateError::Other(OtherError(reason.clone())).into()),
            Err(None) => return Err(CertificateError::UnknownIssuer.into()),
        };
        let result = webpki.verify_server_cert(end_entity, intermediates, server_name, ocsp, now);
        let Err(rustls::Error::InvalidCertificate(CertificateError::Other(OtherError(error)))) = &result else {
            return result;
        };
        if error.downcast_ref::<webpki::Error>() != Some(&webpki::Error::CaUsedAsEndEntity) {
            return result;
        }
        if self.roots.iter().any(|root| root.as_ref() == end_entity.as_ref()) {
            // webpki checked the validity period before it refused the CA as a leaf.
            verify_server_name(&ParsedCertificate::try_from(end_entity)?, server_name)?;
            return Ok(ServerCertVerified::assertion());
        }
        match webpki::EndEntityCert::try_from(end_entity) {
            Ok(cert) if cert.subject() == cert.issuer() => Err(CertificateError::UnknownIssuer.into()),
            _ => result,
        }
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, certificate, signature, &ALGORITHMS)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, certificate, signature, &ALGORITHMS)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        ALGORITHMS.supported_schemes()
    }
}

/// Go's loadOnDiskRoots: SSL_CERT_FILE, or else the first of `files` that reads, and every
/// file in SSL_CERT_DIR's list, or else in `directories`, except symlinks within their own
/// directory. What does not exist is skipped; the first other failure is returned beside the
/// roots, which Go reports only when no root loaded.
pub(crate) fn on_disk_roots(
    file: Option<&OsStr>,
    directories: Option<&OsStr>,
    files: &[&str],
    default_directories: &[&str],
) -> (Vec<CertificateDer<'static>>, Option<io::Error>) {
    let mut roots = Vec::new();
    let mut failure = None;
    let mut keep = |error: io::Error| {
        if error.kind() != io::ErrorKind::NotFound {
            failure.get_or_insert(error);
        }
    };
    let files: Vec<PathBuf> = match file {
        Some(file) => vec![file.into()],
        None => files.iter().map(PathBuf::from).collect(),
    };
    for file in files {
        match fs::read(&file) {
            Ok(data) => {
                roots.extend(certificates(&data));
                break;
            }
            Err(error) => keep(error),
        }
    }
    let directories: Vec<PathBuf> = match directories {
        Some(list) => std::env::split_paths(list).collect(),
        None => default_directories.iter().map(PathBuf::from).collect(),
    };
    for directory in directories {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                keep(error);
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !same_directory_link(&entry, &path)
                && let Ok(data) = fs::read(&path)
            {
                roots.extend(certificates(&data));
            }
        }
    }
    (roots, failure)
}

/// Go's isSameDirSymlink: a link whose target names no directory, as c_rehash's hash links do.
fn same_directory_link(entry: &fs::DirEntry, path: &Path) -> bool {
    entry.file_type().is_ok_and(|kind| kind.is_symlink())
        && fs::read_link(path).is_ok_and(|target| target.components().count() == 1)
}

/// Go's AppendCertsFromPEM: every CERTIFICATE block, skipping what does not parse. Each block is
/// parsed from its own BEGIN line, where Go's pem.Decode starts over after a broken block, so one
/// cut short loses no root after it.
fn certificates(data: &[u8]) -> impl Iterator<Item = CertificateDer<'static>> + '_ {
    const BEGIN: &[u8] = b"\n-----BEGIN ";
    let mut rest = data;
    let blocks = std::iter::from_fn(move || {
        let end = rest
            .windows(BEGIN.len())
            .position(|window| window == BEGIN)
            .map_or(rest.len(), |newline| newline + 1);
        let (block, tail) = rest.split_at(end);
        rest = tail;
        (!block.is_empty()).then_some(block)
    });
    blocks.flat_map(|block| CertificateDer::pem_slice_iter(block).filter_map(Result::ok))
}

#[derive(Debug)]
struct RootsUnavailable(io::Error);
impl std::fmt::Display for RootsUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "no trusted root certificates could be loaded: {}", self.0)
    }
}
impl std::error::Error for RootsUnavailable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

fn untrusted(reason: Option<Arc<dyn std::error::Error + Send + Sync>>) -> Arc<dyn ServerCertVerifier> {
    Arc::new(Verifier {
        webpki: Err(reason),
        roots: Vec::new(),
    })
}
