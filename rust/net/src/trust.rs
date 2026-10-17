//! Loads the trust store for server certificates once on first use.
use crate::crypto::{CONFIGS, Verify, provider};
use rustls::{
    CertificateError, DigitallySignedStruct, OtherError, RootCertStore, SignatureScheme,
    client::{
        WebPkiServerVerifier,
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        verify_server_name,
    },
    crypto::{verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, ServerName, UnixTime, pem::PemObject},
    server::ParsedCertificate,
};
use std::{
    error::Error,
    ffi::OsString,
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};
use tokio::sync::OnceCell;

/// Linux distributions keep their root bundles here. The first that reads counts.
#[cfg(target_os = "linux")]
const FILES: &[&str] = &[
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/ca-bundle.pem",
    "/etc/pki/tls/cacert.pem",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    "/etc/ssl/cert.pem",
];
/// Linux keeps root directories here. Every file in them counts.
#[cfg(target_os = "linux")]
const DIRECTORIES: &[&str] = &["/etc/ssl/certs", "/etc/pki/tls/certs"];

type Reason = Arc<dyn Error + Send + Sync>;

/// Holds the roots to verify against, loaded on first use from `SSL_CERT_FILE` and `SSL_CERT_DIR` or else the system.
pub struct Trust {
    file: Option<OsString>,
    directories: Option<OsString>,
    verifier: OnceCell<Arc<dyn ServerCertVerifier>>,
    pub(crate) configs: [OnceCell<Arc<rustls::ClientConfig>>; CONFIGS],
}

impl Trust {
    /// Takes the roots `lookup`'s `SSL_CERT_FILE` and `SSL_CERT_DIR` name. Empty values are unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<OsString>) -> Self {
        let set = |name| lookup(name).filter(|value| !value.is_empty());
        Self {
            file: set("SSL_CERT_FILE"),
            directories: set("SSL_CERT_DIR"),
            verifier: OnceCell::new(),
            configs: [const { OnceCell::const_new() }; CONFIGS],
        }
    }

    /// Shares the process's roots among every client that verifies against them.
    pub fn system() -> Arc<Self> {
        static SYSTEM: LazyLock<Arc<Trust>> =
            LazyLock::new(|| Arc::new(Trust::from_lookup(|name| std::env::var_os(name))));
        SYSTEM.clone()
    }
}

impl fmt::Debug for Trust {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Trust")
            .field("file", &self.file)
            .field("directories", &self.directories)
            .finish()
    }
}

/// Builds the verifier for `verify`. Trusted roots load once off-thread. An unreadable store fails connections only.
pub(crate) async fn verifier(verify: &Verify) -> Result<Arc<dyn ServerCertVerifier>, tokio::task::JoinError> {
    let Verify::Trusted(trust) = verify else {
        return Ok(Arc::new(Verifier::Insecure));
    };
    let locations = Locations::new(trust.file.clone(), trust.directories.clone());
    trust
        .verifier
        .get_or_try_init(|| tokio::task::spawn_blocking(move || system(locations)))
        .await
        .cloned()
}

/// Builds a verifier that refuses every certificate for `reason`.
pub(crate) fn refusing(reason: impl Error + Send + Sync + 'static) -> Arc<dyn ServerCertVerifier> {
    Arc::new(Verifier::Empty(Some(Arc::new(reason))))
}

#[cfg(target_os = "linux")]
fn system(locations: Locations) -> Arc<dyn ServerCertVerifier> {
    Arc::new(Verifier::load(&locations.or(FILES, DIRECTORIES)))
}

/// Uses the platform verifier, or only the roots `SSL_CERT_FILE` and `SSL_CERT_DIR` name when either is set.
#[cfg(not(target_os = "linux"))]
fn system(locations: Locations) -> Arc<dyn ServerCertVerifier> {
    if !locations.files.is_empty() || !locations.directories.is_empty() {
        return Arc::new(Verifier::load(&locations));
    }
    match rustls_platform_verifier::Verifier::new(provider()) {
        Ok(verifier) => Arc::new(verifier),
        Err(error) => refusing(error),
    }
}

/// Roots load from the first file of `files` that reads and from every file in `directories`.
struct Locations {
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
}

impl Locations {
    /// Takes `SSL_CERT_FILE`'s file and `SSL_CERT_DIR`'s path list.
    fn new(file: Option<OsString>, directories: Option<OsString>) -> Self {
        Self {
            files: file.into_iter().map(PathBuf::from).collect(),
            directories: directories.map_or_else(Vec::new, |list| std::env::split_paths(&list).collect()),
        }
    }

    /// Each list left empty takes its defaults.
    #[cfg(any(target_os = "linux", test))]
    fn or(mut self, files: &[&str], directories: &[&str]) -> Self {
        if self.files.is_empty() {
            self.files = files.iter().map(PathBuf::from).collect();
        }
        if self.directories.is_empty() {
            self.directories = directories.iter().map(PathBuf::from).collect();
        }
        self
    }

    /// Reads each root once and keeps the first failure other than a missing path, which matters only when none loaded.
    fn roots(&self) -> (Vec<CertificateDer<'static>>, Option<io::Error>) {
        let mut failure = None;
        let mut keep = |error: io::Error| {
            if error.kind() != io::ErrorKind::NotFound {
                failure.get_or_insert(error);
            }
        };
        let mut roots = Vec::new();
        let read = |file: &PathBuf| fs::read(file).map_err(&mut keep).ok();
        if let Some(data) = self.files.iter().find_map(read) {
            add(&mut roots, &data);
        }
        for directory in &self.directories {
            let Ok(entries) = fs::read_dir(directory).map_err(&mut keep) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !same_directory_link(&entry, &path)
                    && let Ok(data) = fs::read(&path)
                {
                    add(&mut roots, &data);
                }
            }
        }
        (roots, failure)
    }
}

/// Adds the roots in `data` that `roots` lacks. Debian's bundle also lies in its directory, once more behind links.
fn add(roots: &mut Vec<CertificateDer<'static>>, data: &[u8]) {
    for root in certificates(data) {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
}

/// Spots a link to a bare file name, as c_rehash's hash links are. The caller skips it so each root loads once.
fn same_directory_link(entry: &fs::DirEntry, path: &Path) -> bool {
    entry.file_type().is_ok_and(|kind| kind.is_symlink())
        && fs::read_link(path).is_ok_and(|target| target.components().count() == 1)
}

/// Yields every CERTIFICATE block that parses, each read from its own BEGIN line so a cut block loses no other.
fn certificates(data: &[u8]) -> impl Iterator<Item = CertificateDer<'static>> + '_ {
    const BEGIN: &[u8] = b"\n-----BEGIN ";
    let mut rest = data;
    let blocks = std::iter::from_fn(move || {
        let newline = rest.windows(BEGIN.len()).position(|window| window == BEGIN);
        let end = newline.map_or(rest.len(), |newline| newline + 1);
        let (block, tail) = rest.split_at(end);
        rest = tail;
        (!block.is_empty()).then_some(block)
    });
    blocks.flat_map(|block| CertificateDer::pem_slice_iter(block).filter_map(Result::ok))
}

/// Says how server certificates are checked. Handshake signatures are checked in every case.
#[derive(Debug)]
enum Verifier {
    /// webpki checks against the loaded roots and takes a leaf that is itself a trusted root as its own chain.
    Roots {
        webpki: Arc<WebPkiServerVerifier>,
        roots: Vec<CertificateDer<'static>>,
    },
    /// Every certificate fails because no root loaded, with the reason the store could not be read if one is known.
    Empty(Option<Reason>),
    /// Every certificate passes.
    Insecure,
}

impl Verifier {
    fn load(locations: &Locations) -> Self {
        let (roots, failure) = locations.roots();
        let mut store = RootCertStore::empty();
        store.add_parsable_certificates(roots.iter().cloned());
        if store.is_empty() {
            let unavailable = |error: io::Error| {
                let reason = format!("no trusted root certificates could be loaded: {error}");
                Arc::new(io::Error::new(error.kind(), reason)) as Reason
            };
            return Self::Empty(failure.map(unavailable));
        }
        match WebPkiServerVerifier::builder_with_provider(Arc::new(store), provider()).build() {
            Ok(webpki) => Self::Roots { webpki, roots },
            Err(error) => Self::Empty(Some(Arc::new(error))),
        }
    }
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
        let (webpki, roots) = match self {
            Self::Roots { webpki, roots } => (webpki, roots),
            Self::Empty(Some(reason)) => return Err(CertificateError::Other(OtherError(reason.clone())).into()),
            Self::Empty(None) => return Err(CertificateError::UnknownIssuer.into()),
            Self::Insecure => return Ok(ServerCertVerified::assertion()),
        };
        let result = webpki.verify_server_cert(end_entity, intermediates, server_name, ocsp, now);
        let Err(rustls::Error::InvalidCertificate(CertificateError::Other(OtherError(error)))) = &result else {
            return result;
        };
        if error.downcast_ref::<webpki::Error>() != Some(&webpki::Error::CaUsedAsEndEntity) {
            return result;
        }
        if roots.iter().any(|root| root.as_ref() == end_entity.as_ref()) {
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
        verify_tls12_signature(message, certificate, signature, &provider().signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, certificate, signature, &provider().signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider().signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests;
