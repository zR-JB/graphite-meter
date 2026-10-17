//! Tests the HTTPS listener and its certificate, TLS 1.3 only, startup checks, validity and renewal.

use super::*;
use graphite_meter_server::{config::TlsFiles, transport::tls::Certificates};
use graphite_meter_testkit::{Identity, Scratch};
use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use std::{path::PathBuf, sync::Arc, time::SystemTime};
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// Writes a certificate and key in `scratch` from `identity`.
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
    // The client offers AES-256-GCM first. A server without AES hardware takes ChaCha20-Poly1305 instead.
    let suite = stream.get_ref().1.negotiated_cipher_suite().map(|suite| suite.suite());
    let expected = if graphite_meter_net::aes_hardware() {
        rustls::CipherSuite::TLS13_AES_256_GCM_SHA384
    } else {
        rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256
    };
    assert_eq!(suite, Some(expected));
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
    let refused = Server::bind(config(&env), pool()).await.err().unwrap();
    assert_eq!(
        refused,
        "TLS certificate and key unusable: the private key does not match the certificate"
    );
    let key = scratch.file("key.pem", &first.key).unwrap();
    let mut env = tls_env(cert.to_str().unwrap(), key.to_str().unwrap());
    env.push(("GM_H1_TLS_PUBLIC_ORIGIN", "https://speed.example"));
    let refused = Server::bind(config(&env), pool()).await.err().unwrap();
    assert!(refused.starts_with("TLS certificate incompatible with speed.example: "), "{refused}");
}

/// Tells whether a client trusting only `trusted`'s CA completes a handshake with `acceptor`.
async fn handshake(acceptor: &TlsAcceptor, trusted: &Identity) -> bool {
    let (client, server) = tokio::io::duplex(64 << 10);
    let connector = TlsConnector::from(Arc::new(trusted.client(&[b"http/1.1"])));
    let name = ServerName::try_from("localhost").unwrap();
    let (connected, _) = tokio::join!(connector.connect(name, client), acceptor.accept(server));
    connected.is_ok()
}

#[tokio::test]
async fn a_pair_outside_its_validity_is_refused_and_a_complete_renewal_is_served_in_place() {
    let scratch = Scratch::new().unwrap();
    let (first, renewed) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let (cert, key) = files(&scratch, &first);
    let files = TlsFiles { cert, key };
    let fits = |_: usize| Ok::<(), String>(());
    let (now, years) = (SystemTime::now(), Duration::from_secs(3 * 366 * 86_400));
    let expired = Certificates::load(files.clone(), Vec::new(), now + years, fits).unwrap_err();
    assert!(expired.starts_with("TLS certificate expired at "), "{expired}");
    let early = Certificates::load(files.clone(), Vec::new(), now - years, fits).unwrap_err();
    assert!(early.starts_with("TLS certificate is not valid before "), "{early}");

    let certificates = Arc::new(Certificates::load(files, Vec::new(), now, fits).unwrap());
    let acceptor = certificates.acceptor(b"http/1.1");
    assert!(handshake(&acceptor, &first).await && !handshake(&acceptor, &renewed).await);
    scratch.file("cert.pem", &renewed.certificate).unwrap();
    let half = certificates.reload(now).unwrap_err();
    assert!(half.ends_with("the private key does not match the certificate"), "{half}");
    assert!(handshake(&acceptor, &first).await, "a half-written renewal keeps the current pair");
    scratch.file("key.pem", &renewed.key).unwrap();
    let refused = certificates.reload(now + years).unwrap_err();
    assert!(refused.starts_with("TLS certificate expired at "), "{refused}");
    assert_eq!(certificates.reload(now), Ok(true));
    assert!(handshake(&acceptor, &renewed).await, "the running acceptor serves the renewal");
    assert!(!handshake(&acceptor, &first).await);
    assert_eq!(certificates.reload(now), Ok(false), "an unchanged pair");
}
