//! Operations against the server through the controller: the sign-ins a protected server asks for where none can
//! happen.
use graphite_meter_client::{
    config::Config,
    controller::{Command, Controller, SIGN_IN},
    events::{Event, Events},
    model::{Failure, Outcome},
};
use graphite_meter_e2e::{self as e2e, Server, until};
use graphite_meter_net::Pool;
use graphite_meter_proto::{origin::Origin, reason::FailureReason};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc::UnboundedReceiver;

const HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$OT2po7nOdP+21BKX5CuZQw$9kVgfSWvlFy31939zUCVY62fHIuSqC8RwL67EpQ8qy8";
const LIMIT: Duration = Duration::from_secs(20);

/// A one-second latency run at `url` without warmup.
fn config(url: &Origin, args: &[&str]) -> Config {
    let url = url.to_string();
    e2e::config(&[&["-url", &url, "-stages", "latency", "-latency-duration", "1s", "-warmup", "0s"], args].concat())
}

fn controller(interactive: bool) -> (Controller, UnboundedReceiver<Event>) {
    let (events, received) = Events::channel();
    (Controller::new(interactive, Arc::new(Pool::inline()), events), received)
}

fn run_finished(event: &Event) -> bool {
    matches!(event, Event::RunFinished { .. })
}

#[tokio::test]
async fn a_protected_server_refuses_sign_in_over_http_with_insecure_tls_and_without_the_interface() {
    let server = Server::with(&[
        ("GM_AUTH_MODE", "password"),
        ("GM_AUTH_PUBLIC_URL", "https://127.0.0.7"),
        ("GM_AUTH_PASSWORD_HASH", HASH),
        ("GM_ADVERTISED_NATIVE_ENDPOINTS", "http2,http3"),
    ])
    .await;
    let refused = |text| Failure::new(FailureReason::PreparationFailed, text);
    let checks = [
        (&server.http1, &[][..], refused("authenticated operation requires an HTTPS -url")),
        (
            &server.http2,
            &["-insecure"],
            refused("sign-in refuses skipped TLS verification (Skip TLS verify, -insecure)"),
        ),
    ];
    let (mut interactive, mut received) = controller(true);
    for (url, args, failure) in checks {
        interactive.command(Command::Check(config(url, args)));
        let events = until(&mut received, LIMIT, |event| matches!(event, Event::CheckFailed(_))).await;
        assert_eq!(events.last(), Some(&Event::CheckFailed(failure)));
    }
    let (mut headless, mut received) = controller(false);
    headless.command(Command::Run(config(&server.http1, &[])));
    let events = until(&mut received, LIMIT, run_finished).await;
    let error = Some(Failure::new(FailureReason::SignInRequired, SIGN_IN));
    assert!(
        matches!(events.last(), Some(Event::RunFinished { outcome: Outcome::Failed, error: refusal, .. }) if *refusal == error)
    );
    assert!(!events.iter().any(|event| matches!(event, Event::SignIn(_))));
}
