//! Client TLS trust from `SSL_CERT_FILE` and `SSL_CERT_DIR`.
use graphite_meter_net::{Trust, Verify, client_config};
use graphite_meter_proto::discovery::Protocol;
use graphite_meter_testkit::{Identity, Scratch};
use rustls::{
    CertificateError::{Other, UnknownIssuer},
    ClientConfig, ClientConnection, Connection, Error, ServerConnection,
    pki_types::ServerName,
};
use std::{
    ffi::OsString,
    path::Path,
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};

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

/// Trust in only the roots of `file` and `directory`, as their variables would name them.
fn trust(file: &Path, directory: &Path) -> Verify {
    let (file, directory) = (file.as_os_str().to_owned(), directory.as_os_str().to_owned());
    Verify::Trusted(Arc::new(Trust::from_lookup(|name| match name {
        "SSL_CERT_FILE" => Some(file.clone()),
        "SSL_CERT_DIR" => Some(directory.clone()),
        _ => None::<OsString>,
    })))
}

#[tokio::test]
async fn a_trust_store_without_roots_fails_each_verified_handshake_and_nothing_else() {
    let scratch = Scratch::new().unwrap();
    let verify = trust(&scratch.file("empty.pem", "").unwrap(), &scratch.dir("none").unwrap());
    let server = Identity::generate().unwrap();
    for _ in 0..2 {
        let trusted = client_config(&verify, Some(Protocol::Negotiated)).await;
        assert!(untrusted(handshake(trusted, &server, "localhost")));
    }
    let insecure = client_config(&Verify::Insecure, Some(Protocol::Negotiated)).await;
    handshake(insecure, &server, "localhost").unwrap();
}

#[test]
fn a_trust_load_cut_short_by_its_runtime_s_shutdown_loads_again_on_next_use() {
    let (scratch, server) = (Scratch::new().unwrap(), Identity::self_signed("localhost").unwrap());
    let verify = trust(&scratch.file("root.pem", &server.certificate).unwrap(), &scratch.dir("none").unwrap());
    let stopped = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let handle = stopped.handle().clone();
    stopped.shutdown_background();
    let cut_short = {
        let _context = handle.enter();
        let mut load = pin!(client_config(&verify, Some(Protocol::Http1)));
        let Poll::Ready(config) = load.as_mut().poll(&mut Context::from_waker(Waker::noop())) else {
            panic!("a stopped runtime cancels the load at once");
        };
        config
    };
    let refused = handshake(cut_short, &server, "localhost");
    assert!(matches!(refused, Err(Error::InvalidCertificate(Other(_)))), "{refused:?}");
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    // Only the self-signed root in SSL_CERT_FILE trusts this server, as its own chain.
    handshake(runtime.block_on(client_config(&verify, Some(Protocol::Http1))), &server, "localhost").unwrap();
}
