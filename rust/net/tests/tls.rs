//! Client TLS trust from the process's `SSL_CERT_FILE` and `SSL_CERT_DIR`.
use graphite_meter_net::{Verify, client_config};
use graphite_meter_proto::discovery::Protocol;
use graphite_meter_testkit::{Identity, Scratch};
use rustls::{
    CertificateError::{Other, UnknownIssuer},
    ClientConfig, ClientConnection, Connection, Error, ServerConnection,
    pki_types::ServerName,
};
use std::{
    path::Path,
    pin::pin,
    process::Command,
    sync::Arc,
    task::{Context, Poll, Waker},
};

/// Set in a child process that runs one test with the trust variables its parent chose.
const CHILD: &str = "GRAPHITE_METER_TEST_CHILD";
/// The private key of the self-signed server a child trusts.
const KEY: &str = "GRAPHITE_METER_TEST_KEY";

/// Completes an in-memory handshake and returns the suite it agreed on.
fn handshake(client: Arc<ClientConfig>, server: &Identity, name: &str) -> Result<rustls::CipherSuite, Error> {
    let name = ServerName::try_from(name.to_owned()).unwrap();
    let mut client: Connection = ClientConnection::new(client, name)?.into();
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

/// Runs `test` again in a child process trusting only a self-signed server for `localhost`.
fn rerun_trusting_self_signed(test: &str) {
    let (scratch, identity) = (Scratch::new().unwrap(), Identity::self_signed("localhost").unwrap());
    let file = scratch.file("root.pem", &identity.certificate).unwrap();
    let key = scratch.file("root.key", &identity.key).unwrap();
    let directory = scratch.dir("none").unwrap();
    rerun(test, &[("SSL_CERT_FILE", &file), ("SSL_CERT_DIR", &directory), (KEY, &key)]);
}

/// The self-signed server a child trusts.
fn trusted_server() -> Identity {
    let read = |name| std::fs::read_to_string(std::env::var_os(name).unwrap()).unwrap();
    let certificate = read("SSL_CERT_FILE");
    Identity { ca: certificate.clone(), certificate, key: read(KEY) }
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
        let trusted = client_config(Verify::Trusted, Some(Protocol::Negotiated)).await;
        assert!(untrusted(handshake(trusted, &server, "localhost")));
    }
    let insecure = client_config(Verify::Insecure, Some(Protocol::Negotiated)).await;
    handshake(insecure, &server, "localhost").unwrap();
}

#[test]
fn a_trust_load_cut_short_by_its_runtime_s_shutdown_loads_again_on_next_use() {
    let test = "a_trust_load_cut_short_by_its_runtime_s_shutdown_loads_again_on_next_use";
    if !child() {
        return rerun_trusting_self_signed(test);
    }
    let server = trusted_server();
    let stopped = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let handle = stopped.handle().clone();
    stopped.shutdown_background();
    let cut_short = {
        let _context = handle.enter();
        let mut load = pin!(client_config(Verify::Trusted, Some(Protocol::Http1)));
        let Poll::Ready(config) = load.as_mut().poll(&mut Context::from_waker(Waker::noop())) else {
            panic!("a stopped runtime cancels the load at once");
        };
        config
    };
    let refused = handshake(cut_short, &server, "localhost");
    assert!(matches!(refused, Err(Error::InvalidCertificate(Other(_)))), "{refused:?}");
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    // Only the self-signed root in SSL_CERT_FILE trusts this server, as its own chain.
    handshake(
        runtime.block_on(client_config(Verify::Trusted, Some(Protocol::Http1))),
        &server,
        "localhost",
    )
    .unwrap();
}
