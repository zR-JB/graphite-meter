//! Operations against the server through the controller: a run replacing a check, a stop before the run starts, and
//! the sign-ins a protected server asks for where none can happen.
use graphite_meter_client::{
    config::{self, Config, Parsed},
    controller::{Command, Controller, SIGN_IN},
    events::{Event, Events},
    model::{Failure, Outcome},
};
use graphite_meter_e2e::{Server, until};
use graphite_meter_net::Pool;
use graphite_meter_proto::{origin::Origin, reason::FailureReason};
use std::{
    ffi::OsString,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, sync::mpsc::UnboundedReceiver};

const HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$OT2po7nOdP+21BKX5CuZQw$9kVgfSWvlFy31939zUCVY62fHIuSqC8RwL67EpQ8qy8";
const LIMIT: Duration = Duration::from_secs(20);

/// A one-second latency run at `url` without warmup.
fn config(url: &Origin, args: &[&str]) -> Config {
    let url = url.to_string();
    let args = [&["-url", &url, "-stages", "latency", "-latency-duration", "1s", "-warmup", "0s"], args].concat();
    match config::parse(args.into_iter().map(OsString::from)) {
        Ok(Parsed::Run(config)) => *config,
        other => panic!("{other:?}"),
    }
}

fn controller(interactive: bool) -> (Controller, UnboundedReceiver<Event>) {
    let (events, received) = Events::channel();
    (Controller::new(interactive, Arc::new(Pool::inline()), events), received)
}

fn run_finished(event: &Event) -> bool {
    matches!(event, Event::RunFinished { .. })
}

#[tokio::test]
async fn a_run_replacing_a_check_starts_at_once() {
    let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent = Origin::parse(&format!("http://{}", silent.local_addr().unwrap())).unwrap();
    let server = Server::start().await;
    let (mut controller, mut received) = controller(true);
    let started = Instant::now();
    controller.command(Command::Check(config(&silent, &[])));
    controller.command(Command::Run(config(&server.http1, &[])));
    let events = until(&mut received, LIMIT, run_finished).await;
    assert!(started.elapsed() < Duration::from_secs(8), "{:?}", started.elapsed());
    let checked = events.iter().position(|event| *event == Event::Checking { run: true });
    assert!(checked.is_some_and(|checked| {
        events[..checked]
            .iter()
            .all(|event| *event == Event::Checking { run: false })
    }));
    assert!(events.iter().any(|event| matches!(event, Event::RunStarted { .. })));
    let outcome = events.last().unwrap();
    assert!(matches!(outcome, Event::RunFinished { outcome: Outcome::Complete, .. }), "{events:#?}");
    controller.settled().await;
}

#[tokio::test]
async fn a_run_stopped_before_it_starts_still_finishes_the_check() {
    let server = Server::start().await;
    let config = config(&server.http1, &[]);
    let (mut controller, mut received) = controller(true);
    controller.command(Command::Check(config.clone()));
    controller.command(Command::Run(config));
    controller.command(Command::Stop);
    let stopped = until(&mut received, LIMIT, run_finished).await;
    assert!(!stopped.iter().any(|event| matches!(event, Event::RunStarted { .. })), "{stopped:#?}");
    let finished = stopped.last().unwrap();
    assert!(matches!(finished, Event::RunFinished { outcome: Outcome::Stopped, error: None, .. }));
    let checked = until(&mut received, LIMIT, |event| matches!(event, Event::Prepared { .. })).await;
    assert_eq!(checked[0], Event::Checking { run: false });
    let Some(Event::Prepared { servers, .. }) = checked.last() else {
        unreachable!()
    };
    assert!(servers.iter().all(|server| server.path.is_ok()), "{servers:#?}");
    controller.settled().await;
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
