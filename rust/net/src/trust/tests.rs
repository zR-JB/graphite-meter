use super::*;
use graphite_meter_testkit::Identity;
use rustls::CertificateError::{Expired, ExpiredContext, UnknownIssuer};
use std::time::Duration;

/// A scratch directory whose paths tests pass around as text.
struct Scratch(graphite_meter_testkit::Scratch);

impl Scratch {
    fn new() -> Self {
        Self(graphite_meter_testkit::Scratch::new().unwrap())
    }

    fn file(&self, name: &str, contents: &str) -> String {
        self.0.file(name, contents).unwrap().to_str().unwrap().to_owned()
    }

    fn dir(&self, name: &str) -> String {
        self.0.dir(name).unwrap().to_str().unwrap().to_owned()
    }
}

fn certificate() -> (String, CertificateDer<'static>) {
    let pem = Identity::self_signed("localhost").unwrap().certificate;
    let der = CertificateDer::from_pem_slice(pem.as_bytes()).unwrap();
    (pem, der)
}

/// The locations a process with these variables reads, given default lists.
fn locations(variables: &[(&str, &str)], files: &[&str], directories: &[&str]) -> Locations {
    let lookup = |name: &str| {
        variables
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.into())
    };
    Locations::from_lookup(lookup, files, directories)
}

fn roots(locations: &Locations) -> Vec<CertificateDer<'static>> {
    let (roots, failure) = locations.roots();
    assert!(failure.is_none(), "{failure:?}");
    roots
}

fn verify(store: &Store, certificate: &CertificateDer<'_>, name: &str, now: UnixTime) -> Result<(), rustls::Error> {
    let name = ServerName::try_from(name.to_owned()).unwrap();
    store.verify_server_cert(certificate, &[], &name, &[], now).map(drop)
}

#[test]
fn ssl_cert_file_replaces_only_the_file_list_and_ssl_cert_dir_only_the_directories() {
    let scratch = Scratch::new();
    let [(file_pem, file), (bundle_pem, bundle), (dir_pem, dir), (listed_pem, listed)] =
        [certificate(), certificate(), certificate(), certificate()];
    let named = scratch.file("named.pem", &file_pem);
    let bundle_path = scratch.file("bundle.pem", &bundle_pem);
    let defaults = scratch.dir("certs");
    scratch.file("certs/root.pem", &dir_pem);
    let listed_dir = scratch.dir("listed");
    scratch.file("listed/root.pem", &listed_pem);
    let missing = scratch.0.path().join("missing").to_str().unwrap().to_owned();
    let files = [missing.as_str(), bundle_path.as_str(), named.as_str()];
    let read = |variables: &[(&str, &str)]| roots(&locations(variables, &files, &[&defaults]));
    assert_eq!(read(&[]), [bundle.clone(), dir.clone()], "the first file that reads and every directory");
    assert_eq!(read(&[("SSL_CERT_FILE", &named)]), [file.clone(), dir.clone()]);
    let both = std::env::join_paths([&listed_dir, &missing]).unwrap();
    assert_eq!(read(&[("SSL_CERT_DIR", both.to_str().unwrap())]), [bundle.clone(), listed.clone()]);
    assert_eq!(read(&[("SSL_CERT_FILE", &missing)]), [dir], "a missing file is skipped");
    assert_eq!(read(&[("SSL_CERT_FILE", &named), ("SSL_CERT_DIR", &listed_dir)]), [file, listed]);
    assert_eq!(read(&[("SSL_CERT_FILE", ""), ("SSL_CERT_DIR", "")]), read(&[]), "empty values are unset");
}

#[test]
fn a_block_cut_short_loses_no_root_after_it() {
    let scratch = Scratch::new();
    let [(cut, _), (pem, root)] = [certificate(), certificate()];
    let cut: String = cut.lines().take(3).map(|line| format!("{line}\n")).collect();
    let bundle = scratch.file("bundle.pem", &format!("{cut}{pem}"));
    assert_eq!(roots(&locations(&[("SSL_CERT_FILE", &bundle)], &[], &[])), [root]);
}

#[cfg(unix)]
#[test]
fn hash_links_load_once_and_an_unreadable_directory_is_reported_beside_the_roots() {
    let scratch = Scratch::new();
    let (pem, root) = certificate();
    let directory = scratch.dir("certs");
    scratch.file("certs/root.pem", &pem);
    std::os::unix::fs::symlink("root.pem", format!("{directory}/0123abcd.0")).unwrap();
    assert_eq!(roots(&locations(&[], &[], &[&directory])), std::slice::from_ref(&root));
    let not_a_directory = scratch.file("plain", &pem);
    let (loaded, failure) = locations(&[], &[], &[&not_a_directory, &directory]).roots();
    assert_eq!((loaded, failure.is_some()), (vec![root], true));
}

#[test]
fn a_trusted_self_signed_ca_is_its_own_chain_and_an_untrusted_one_has_an_unknown_issuer() {
    let scratch = Scratch::new();
    let (pem, ca) = certificate();
    let store = Store::load(&locations(&[("SSL_CERT_FILE", &scratch.file("ca.pem", &pem))], &[], &[]));
    let now = UnixTime::now();
    verify(&store, &ca, "localhost", now).unwrap();
    assert!(verify(&store, &ca, "other.test", now).is_err(), "its name still counts");
    let later = UnixTime::since_unix_epoch(Duration::from_secs(now.as_secs() + 10 * 86_400));
    let expired = verify(&store, &ca, "localhost", later);
    assert!(
        matches!(expired, Err(rustls::Error::InvalidCertificate(Expired | ExpiredContext { .. }))),
        "{expired:?}"
    );
    let stranger = verify(&store, &certificate().1, "localhost", now);
    assert!(matches!(stranger, Err(rustls::Error::InvalidCertificate(UnknownIssuer))), "{stranger:?}");
}

#[test]
fn a_store_without_roots_refuses_every_certificate_as_untrusted() {
    let scratch = Scratch::new();
    let (empty, directory) = (scratch.file("empty.pem", ""), scratch.dir("none"));
    let store = Store::load(&locations(&[("SSL_CERT_FILE", &empty), ("SSL_CERT_DIR", &directory)], &[], &[]));
    let refused = verify(&store, &certificate().1, "localhost", UnixTime::now());
    assert!(matches!(refused, Err(rustls::Error::InvalidCertificate(UnknownIssuer))), "{refused:?}");
    let unreadable = Store::load(&locations(&[], &[&directory], &[]));
    let refused = verify(&unreadable, &certificate().1, "localhost", UnixTime::now());
    let Err(rustls::Error::InvalidCertificate(CertificateError::Other(OtherError(reason)))) = refused else {
        panic!("{refused:?}");
    };
    assert!(
        reason
            .to_string()
            .starts_with("no trusted root certificates could be loaded: "),
        "{reason}"
    );
}
