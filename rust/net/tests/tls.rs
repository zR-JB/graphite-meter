//! Client TLS: Go's suite order, and trust from the process's `SSL_CERT_FILE` and `SSL_CERT_DIR`.
use graphite_meter_net::{Verify, client_config, provider};
use graphite_meter_testkit::{Identity, Scratch};
use rustls::{
    CertificateError::UnknownIssuer,
    CipherSuite::{TLS13_AES_128_GCM_SHA256, TLS13_AES_256_GCM_SHA384, TLS13_CHACHA20_POLY1305_SHA256},
    ClientConfig, ClientConnection, Connection, Error, ServerConnection,
    pki_types::ServerName,
};
use std::{path::Path, process::Command, sync::Arc};

/// Set in a child process that runs one test with the trust variables its parent chose.
const CHILD: &str = "GRAPHITE_METER_TEST_CHILD";
/// The private key of the self-signed server a child trusts.
const KEY: &str = "GRAPHITE_METER_TEST_KEY";

/// Completes an in-memory handshake and returns the suite it agreed on.
fn handshake(client: ClientConfig, server: &Identity, name: &str) -> Result<rustls::CipherSuite, Error> {
    let name = ServerName::try_from(name.to_owned()).unwrap();
    let mut client: Connection = ClientConnection::new(Arc::new(client), name)?.into();
    let mut server: Connection = ServerConnection::new(Arc::new(server.server(&[])))?.into();
    while client.is_handshaking() || server.is_handshaking() {
        transfer(&mut client, &mut server)?;
        transfer(&mut server, &mut client)?;
    }
    Ok(client.negotiated_cipher_suite().expect("a finished handshake").suite())
}

fn transfer(from: &mut Connection, to: &mut Connection) -> Result<(), Error> {
    let mut bytes = Vec::new();
    from.write_tls(&mut bytes).unwrap();
    to.read_tls(&mut &bytes[..]).unwrap();
    to.process_new_packets().map(drop)
}

fn untrusted(result: Result<rustls::CipherSuite, Error>) -> bool {
    matches!(result, Err(Error::InvalidCertificate(UnknownIssuer)))
}

/// Runs `test` again in a child process with `variables` set, and expects it to pass.
fn rerun(test: &str, variables: &[(&str, &Path)]) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--nocapture"])
        .env(CHILD, "1")
        .envs(variables.iter().copied())
        .status()
        .unwrap();
    assert!(status.success(), "{test} failed in its child process");
}

fn child() -> bool {
    std::env::var_os(CHILD).is_some()
}

#[tokio::test]
async fn the_client_offers_go_s_tls13_order_and_the_server_follows_it() {
    let suites: Vec<_> = provider().cipher_suites[..3]
        .iter()
        .map(|suite| suite.suite())
        .collect();
    let aes_first = [TLS13_AES_128_GCM_SHA256, TLS13_AES_256_GCM_SHA384, TLS13_CHACHA20_POLY1305_SHA256];
    let chacha_first = [TLS13_CHACHA20_POLY1305_SHA256, TLS13_AES_128_GCM_SHA256, TLS13_AES_256_GCM_SHA384];
    assert!(suites == aes_first || suites == chacha_first, "{suites:?}");
    let client = client_config(Verify::Insecure, rustls::DEFAULT_VERSIONS, &[]).await;
    assert_eq!(handshake(client, &Identity::generate().unwrap(), "localhost").unwrap(), suites[0]);
}

#[tokio::test]
async fn ssl_cert_file_and_dir_name_the_only_roots_and_a_trusted_self_signed_ca_is_its_own_chain() {
    let test = "ssl_cert_file_and_dir_name_the_only_roots_and_a_trusted_self_signed_ca_is_its_own_chain";
    if !child() {
        let (scratch, identity) = (Scratch::new().unwrap(), Identity::self_signed("localhost").unwrap());
        let file = scratch.file("root.pem", &identity.certificate).unwrap();
        let key = scratch.file("root.key", &identity.key).unwrap();
        let directory = scratch.dir("none").unwrap();
        return rerun(test, &[("SSL_CERT_FILE", &file), ("SSL_CERT_DIR", &directory), (KEY, &key)]);
    }
    let read = |name| std::fs::read_to_string(std::env::var_os(name).unwrap()).unwrap();
    let certificate = read("SSL_CERT_FILE");
    let server = Identity { ca: certificate.clone(), certificate, key: read(KEY) };
    let trusted = || client_config(Verify::Trusted, rustls::DEFAULT_VERSIONS, &[]);
    handshake(trusted().await, &server, "localhost").unwrap();
    assert!(handshake(trusted().await, &server, "other.test").is_err(), "its name still counts");
    assert!(untrusted(handshake(trusted().await, &Identity::generate().unwrap(), "localhost")));
}

#[tokio::test]
async fn a_trust_store_without_roots_fails_each_verified_handshake_and_nothing_else() {
    let test = "a_trust_store_without_roots_fails_each_verified_handshake_and_nothing_else";
    if !child() {
        let scratch = Scratch::new().unwrap();
        let (file, directory) = (scratch.file("empty.pem", "").unwrap(), scratch.dir("none").unwrap());
        return rerun(test, &[("SSL_CERT_FILE", &file), ("SSL_CERT_DIR", &directory)]);
    }
    let server = Identity::generate().unwrap();
    for _ in 0..2 {
        let trusted = client_config(Verify::Trusted, rustls::DEFAULT_VERSIONS, &[]).await;
        assert!(untrusted(handshake(trusted, &server, "localhost")));
    }
    let insecure = client_config(Verify::Insecure, rustls::DEFAULT_VERSIONS, &[]).await;
    handshake(insecure, &server, "localhost").unwrap();
}
