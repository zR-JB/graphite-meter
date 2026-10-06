//! Native sign-in against a password-mode server, in a child process that trusts the server's CA alone: the
//! operator's browser approves over HTTP and the run completes.
use graphite_meter_client::{
    config::Config,
    controller::{Command, Controller},
    events::{Event, Events, SignInEnd, SignInPrompt},
    model::Outcome,
};
use graphite_meter_e2e::{self as e2e, Server, until};
use graphite_meter_net::Pool;
use graphite_meter_testkit::{Identity, Scratch};
use std::{net::SocketAddr, path::Path, process::Command as Process, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc::UnboundedReceiver,
};
use tokio_rustls::{TlsConnector, rustls::pki_types::ServerName};

/// Set in a child process to the directory holding the identity it trusts.
const CHILD: &str = "GRAPHITE_METER_TEST_IDENTITY";
const HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$OT2po7nOdP+21BKX5CuZQw$9kVgfSWvlFy31939zUCVY62fHIuSqC8RwL67EpQ8qy8";
const LIMIT: Duration = Duration::from_secs(20);

/// In a child process, the identity it trusts; otherwise runs `test` in a child that trusts a fresh one.
async fn trusted(test: &str) -> Option<Identity> {
    if let Some(directory) = std::env::var_os(CHILD) {
        let read = |name| std::fs::read_to_string(Path::new(&directory).join(name)).unwrap();
        return Some(Identity {
            ca: read("ca.pem"),
            certificate: read("cert.pem"),
            key: read("key.pem"),
        });
    }
    let (scratch, identity) = (Scratch::new().unwrap(), Identity::generate().unwrap());
    let ca = scratch.file("ca.pem", &identity.ca).unwrap();
    scratch.file("cert.pem", &identity.certificate).unwrap();
    scratch.file("key.pem", &identity.key).unwrap();
    let mut child = Process::new(std::env::current_exe().unwrap());
    child.args([test, "--exact", "--nocapture"]).env(CHILD, scratch.path());
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY"] {
        child.env_remove(name).env_remove(name.to_ascii_lowercase());
    }
    child
        .env("SSL_CERT_FILE", ca)
        .env("SSL_CERT_DIR", scratch.dir("none").unwrap());
    let status = tokio::task::spawn_blocking(move || child.status()).await.unwrap();
    assert!(status.unwrap().success(), "{test} failed in its child process");
    None
}

/// A password-mode server whose public origin is its HTTPS HTTP/1.1 listener.
async fn protected(identity: &Identity) -> (Server, SocketAddr) {
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = free.local_addr().unwrap();
    drop(free);
    let (listener, public) = (address.to_string(), format!("https://{address}"));
    let settings = [
        ("GM_H1_TLS_ADDR", listener.as_str()),
        ("GM_AUTH_MODE", "password"),
        ("GM_AUTH_PUBLIC_URL", &public),
        ("GM_AUTH_PASSWORD_HASH", HASH),
        ("GM_ADVERTISED_NATIVE_ENDPOINTS", "http1-tls"),
    ];
    (Server::serve(&settings, identity).await, address)
}

/// A check of a latency and download run at `address` over verified TLS.
fn config(address: SocketAddr) -> Config {
    let url = format!("https://{address}");
    e2e::config(&[
        "-url",
        &url,
        "-stages",
        "latency,download",
        "-latency-duration",
        "1s",
        "-download-duration",
        "1s",
    ])
}

/// The prompt an interactive check of `config` shows.
async fn prompted(config: &Config) -> (Controller, UnboundedReceiver<Event>, SignInPrompt) {
    let (events, mut received) = Events::channel();
    let mut controller = Controller::new(true, Arc::new(Pool::inline()), events);
    controller.command(Command::Check(config.clone()));
    let events = until(&mut received, LIMIT, |event| matches!(event, Event::SignIn(_))).await;
    let Some(Event::SignIn(prompt)) = events.last() else { unreachable!() };
    (controller, received, prompt.clone())
}

/// Sends `request` over TLS to `address` and reads the answer to its end.
async fn exchange(identity: &Identity, address: SocketAddr, request: &str) -> String {
    let connector = TlsConnector::from(Arc::new(identity.client(&[b"http/1.1"])));
    let stream = TcpStream::connect(address).await.unwrap();
    let mut tls = connector
        .connect(ServerName::IpAddress(address.ip().into()), stream)
        .await
        .unwrap();
    tls.write_all(request.as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    let _ = tls.read_to_end(&mut answer).await;
    String::from_utf8_lossy(&answer).into_owned()
}

/// Approves `prompt` as a signed-in operator's browser does: its page, showing the code, then the approval form.
async fn approve(server: &Server, identity: &Identity, address: SocketAddr, prompt: &SignInPrompt) {
    let login = server
        .app
        .auth()
        .store()
        .unwrap()
        .sign_in("operator", "Operator", "local")
        .unwrap();
    let page = prompt.url.strip_prefix(&format!("https://{address}")).unwrap();
    let cookie = format!("__Host-gm_session={}", login.token);
    let head = format!("host: {address}\r\ncookie: {cookie}\r\nconnection: close\r\n");
    let shown = exchange(identity, address, &format!("GET {page} HTTP/1.1\r\n{head}\r\n")).await;
    assert!(shown.starts_with("HTTP/1.1 200") && shown.contains(&prompt.code), "{shown}");
    let challenge = page.split_once("challenge=").unwrap().1;
    let form = format!("csrf={}&challenge={challenge}", login.csrf);
    let post = format!(
        "POST /auth/cli/approve HTTP/1.1\r\n{head}origin: https://{address}\r\ncontent-type: \
         application/x-www-form-urlencoded\r\ncontent-length: {}\r\n\r\n{form}",
        form.len()
    );
    let approved = exchange(identity, address, &post).await;
    assert!(approved.starts_with("HTTP/1.1 200"), "{approved}");
}

#[tokio::test]
async fn a_sign_in_approved_over_http_lets_the_run_complete() {
    let Some(identity) = trusted("a_sign_in_approved_over_http_lets_the_run_complete").await else {
        return;
    };
    let (server, address) = protected(&identity).await;
    let config = config(address);
    let (mut controller, mut received, prompt) = prompted(&config).await;
    assert!(
        prompt
            .url
            .starts_with(&format!("https://{address}/auth/cli?challenge=")),
        "{prompt:?}"
    );
    assert_eq!(prompt.code.len(), 8);
    approve(&server, &identity, address, &prompt).await;
    let checked = until(&mut received, LIMIT, |event| {
        matches!(event, Event::Prepared { .. } | Event::CheckFailed(_))
    })
    .await;
    assert_eq!(checked[0], Event::SignInEnded(SignInEnd::Approved));
    let Some(Event::Prepared { servers, .. }) = checked.last() else {
        panic!("{checked:#?}")
    };
    assert!(servers.iter().all(|server| server.path.is_ok()), "{servers:#?}");
    controller.command(Command::Run(config));
    let ran = until(&mut received, LIMIT, |event| matches!(event, Event::RunFinished { .. })).await;
    assert!(
        matches!(ran.last(), Some(Event::RunFinished { outcome: Outcome::Complete, .. })),
        "{ran:#?}"
    );
    assert!(!ran.iter().any(|event| matches!(event, Event::SignIn(_))));
    controller.settled().await;
}
