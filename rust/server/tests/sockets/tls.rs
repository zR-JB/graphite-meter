//! The HTTPS listener and its certificate: TLS 1.3 only, startup checks and renewals.

use super::*;
use graphite_meter_server::{config::TlsFiles, transport::tls::Certificates};
use graphite_meter_testkit::{Identity, Scratch};
use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use std::{path::PathBuf, sync::Arc, time::SystemTime};
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// A certificate and key in `scratch` from `identity`.
fn files(scratch: &Scratch, identity: &Identity) -> (PathBuf, PathBuf) {
    let cert = scratch.file("cert.pem", &identity.certificate).unwrap();
    (cert, scratch.file("key.pem", &identity.key).unwrap())
}

fn tls_env<'a>(cert: &'a str, key: &'a str) -> Vec<(&'static str, &'a str)> {
    vec![("GM_TLS_CERT", cert), ("GM_TLS_KEY", key), ("GM_H1_TLS_ADDR", "localhost:0")]
}

#[tokio::test]
async fn the_https_listener_serves_http_1_1_over_tls_1_3_only() {
    let (scratch, identity) = (Scratch::new().unwrap(), Identity::generate().unwrap());
    let (cert, key) = files(&scratch, &identity);
    let server = start(&tls_env(cert.to_str().unwrap(), key.to_str().unwrap())).await;
    let address = server.tls.unwrap();
    let connector = TlsConnector::from(Arc::new(identity.client(&[b"http/1.1"])));
    let socket = TcpStream::connect(address).await.unwrap();
    let stream = connector
        .connect(ServerName::try_from("localhost").unwrap(), socket)
        .await
        .unwrap();
    assert_eq!(stream.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
    let mut client = Client::new(stream);
    let answer = client.request("GET /probe", "").await;
    assert_eq!(answer.status, 200);
    assert_eq!(answer.header("strict-transport-security"), None, "HSTS belongs to authentication");

    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_slice(identity.ca.as_bytes()).unwrap())
        .unwrap();
    let tls12 = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS12])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let socket = TcpStream::connect(address).await.unwrap();
    let refused = TlsConnector::from(Arc::new(tls12)).connect(ServerName::try_from("localhost").unwrap(), socket);
    assert!(refused.await.is_err(), "TLS 1.2 is refused");
}

#[tokio::test]
async fn startup_refuses_a_mismatched_pair_and_an_uncovered_public_host() {
    let scratch = Scratch::new().unwrap();
    let (first, second) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let cert = scratch.file("cert.pem", &first.certificate).unwrap();
    let key = scratch.file("key.pem", &second.key).unwrap();
    let env = tls_env(cert.to_str().unwrap(), key.to_str().unwrap());
    let refused = Server::bind(config(&env)).await.err().unwrap();
    assert_eq!(refused, "load matching TLS certificate/key: tls: private key does not match public key");
    let key = scratch.file("key.pem", &first.key).unwrap();
    let mut env = tls_env(cert.to_str().unwrap(), key.to_str().unwrap());
    env.push(("GM_H1_TLS_PUBLIC_ORIGIN", "https://speed.example"));
    let refused = Server::bind(config(&env)).await.err().unwrap();
    assert!(refused.starts_with("TLS certificate incompatible with speed.example: "), "{refused}");
}

/// Whether a client trusting `identity`'s CA completes a handshake with `acceptor`.
async fn trusts(acceptor: &TlsAcceptor, identity: &Identity) -> bool {
    let (client, server) = tokio::io::duplex(64 << 10);
    let connector = TlsConnector::from(Arc::new(identity.client(&[b"http/1.1"])));
    let name = ServerName::try_from("localhost").unwrap();
    let (connected, _) = tokio::join!(connector.connect(name, client), acceptor.accept(server));
    connected.is_ok()
}

#[tokio::test]
async fn a_renewal_replaces_the_pair_only_once_complete() {
    let scratch = Scratch::new().unwrap();
    let (old, new) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let (cert, key) = files(&scratch, &old);
    let certificates = Certificates::load(TlsFiles { cert, key }, vec![], SystemTime::now()).unwrap();
    let certificates = Arc::new(certificates);
    let acceptor = certificates.acceptor(b"http/1.1");
    assert!(trusts(&acceptor, &old).await);
    scratch.file("cert.pem", &new.certificate).unwrap();
    let refused = certificates.reload(SystemTime::now()).unwrap_err();
    assert!(refused.contains("private key does not match public key"), "{refused}");
    assert!(trusts(&acceptor, &old).await, "an incomplete renewal keeps the previous pair");
    assert!(!trusts(&acceptor, &new).await);
    scratch.file("key.pem", &new.key).unwrap();
    assert_eq!(certificates.reload(SystemTime::now()), Ok(true));
    assert!(trusts(&acceptor, &new).await, "the complete renewal serves new handshakes");
    assert!(!trusts(&acceptor, &old).await);
    assert_eq!(certificates.reload(SystemTime::now()), Ok(false), "an unchanged pair is no renewal");
}
