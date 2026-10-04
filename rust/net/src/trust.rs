//! The trust store for server certificates, loaded once on first use from the places Go 1.27 reads.
use crate::crypto::{Verify, provider};
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
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Go's Linux root files, of which the first that reads counts.
#[cfg(target_os = "linux")]
const FILES: &[&str] = &[
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/ca-bundle.pem",
    "/etc/pki/tls/cacert.pem",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    "/etc/ssl/cert.pem",
];
/// Go's Linux root directories, every file of which counts.
#[cfg(target_os = "linux")]
const DIRECTORIES: &[&str] = &["/etc/ssl/certs", "/etc/pki/tls/certs"];

type Reason = Arc<dyn Error + Send + Sync>;

/// The verifier for `verify`; the trusted one loads on a blocking thread and is kept once a load finishes, and a
/// store that cannot be read fails each connection it would verify, not the process.
pub(crate) async fn verifier(verify: Verify) -> Result<Arc<dyn ServerCertVerifier>, tokio::task::JoinError> {
    static TRUSTED: tokio::sync::OnceCell<Arc<dyn ServerCertVerifier>> = tokio::sync::OnceCell::const_new();
    if verify == Verify::Insecure {
        return Ok(Arc::new(Verifier::Insecure));
    }
    let load = || tokio::task::spawn_blocking(|| system(|name| std::env::var_os(name)));
    TRUSTED.get_or_try_init(load).await.cloned()
}

/// A verifier that refuses every certificate for `reason`.
pub(crate) fn refusing(reason: impl Error + Send + Sync + 'static) -> Arc<dyn ServerCertVerifier> {
    Arc::new(Verifier::Empty(Some(Arc::new(reason))))
}

/// The roots `SSL_CERT_FILE` and `SSL_CERT_DIR` name, each replacing its own part of the default lists.
#[cfg(target_os = "linux")]
fn system(lookup: impl Fn(&str) -> Option<OsString>) -> Arc<dyn ServerCertVerifier> {
    Arc::new(Verifier::load(&Locations::from_lookup(lookup, FILES, DIRECTORIES)))
}

/// The platform verifier, or only the roots `SSL_CERT_FILE` and `SSL_CERT_DIR` name when either is set.
#[cfg(not(target_os = "linux"))]
fn system(lookup: impl Fn(&str) -> Option<OsString>) -> Arc<dyn ServerCertVerifier> {
    let set = |name| lookup(name).is_some_and(|value: OsString| !value.is_empty());
    if set("SSL_CERT_FILE") || set("SSL_CERT_DIR") {
        return Arc::new(Verifier::load(&Locations::from_lookup(lookup, &[], &[])));
    }
    match rustls_platform_verifier::Verifier::new(provider()) {
        Ok(verifier) => Arc::new(verifier),
        Err(error) => refusing(error),
    }
}

/// Where roots load from: the first file of `files` that reads, and every file in `directories`.
struct Locations {
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
}

impl Locations {
    /// Each set, non-empty variable replaces its default list; `SSL_CERT_DIR` is a path list.
    fn from_lookup(lookup: impl Fn(&str) -> Option<OsString>, files: &[&str], directories: &[&str]) -> Self {
        let set = |name| lookup(name).filter(|value| !value.is_empty());
        let (file, directory_list) = (set("SSL_CERT_FILE"), set("SSL_CERT_DIR"));
        Self {
            files: file.map_or_else(|| files.iter().map(PathBuf::from).collect(), |file| vec![file.into()]),
            directories: directory_list.map_or_else(
                || directories.iter().map(PathBuf::from).collect(),
                |list| std::env::split_paths(&list).collect(),
            ),
        }
    }

    /// The roots, each once, and the first failure other than a missing path, which matters only when none loaded.
    fn roots(&self) -> (Vec<CertificateDer<'static>>, Option<io::Error>) {
        let mut failure = None;
        let mut keep = |error: io::Error| {
            if error.kind() != io::ErrorKind::NotFound {
                failure.get_or_insert(error);
            }
        };
        let mut roots = Vec::new();
        if let Some(data) = self
            .files
            .iter()
            .find_map(|file| fs::read(file).map_err(&mut keep).ok())
        {
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

/// Adds the roots in `data` that `roots` lacks; Debian's bundle also lies in its directory, once more behind links.
fn add(roots: &mut Vec<CertificateDer<'static>>, data: &[u8]) {
    for root in certificates(data) {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
}

/// A link to a bare file name, as c_rehash's hash links are; Go skips them so each root loads once.
fn same_directory_link(entry: &fs::DirEntry, path: &Path) -> bool {
    entry.file_type().is_ok_and(|kind| kind.is_symlink())
        && fs::read_link(path).is_ok_and(|target| target.components().count() == 1)
}

/// Every CERTIFICATE block that parses, each read from its own BEGIN line so a cut block loses no other.
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

/// How server certificates are checked; handshake signatures are checked in every case.
#[derive(Debug)]
enum Verifier {
    /// webpki over the loaded roots, taking a leaf that is itself a trusted root as its own chain.
    Roots {
        webpki: Arc<WebPkiServerVerifier>,
        roots: Vec<CertificateDer<'static>>,
    },
    /// No root loaded: every certificate fails, with the reason the store could not be read if one is known.
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
