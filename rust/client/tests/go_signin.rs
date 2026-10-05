//! Native sign-in against an unchanged password-mode Go server: rust/interop/client.py starts it, names it in
//! `GM_GO_AUTH_URL` and approves the page this test prints as a signed-in operator's browser.
use graphite_meter_client::{
    config::{self, Config, Parsed},
    controller::{Command, Controller},
    events::{Event, Events, SignInEnd},
    model::Outcome,
};
use graphite_meter_net::Pool;
use std::{ffi::OsString, sync::Arc, time::Duration};
use tokio::{sync::mpsc::UnboundedReceiver, time::timeout};

/// The bound on each awaited event: an approval, or a whole run.
const LIMIT: Duration = Duration::from_secs(60);

/// A four-stage run at `url` forced onto one throughput and one latency path.
fn config(url: &str, protocol: &str, throughput: &str, latency: &str) -> Config {
    let args = [
        "--url",
        url,
        "--stages",
        "latency,download,upload,bidirectional",
        "--throughput-protocol",
        protocol,
        "--throughput-transport",
        throughput,
        "--latency-transport",
        latency,
        "--warmup=250ms",
        "--latency-duration=1s",
        "--download-duration=2s",
        "--upload-duration=2s",
        "--bidirectional-duration=2s",
    ];
    match config::parse(args.into_iter().map(OsString::from)) {
        Ok(Parsed::Run(config)) => *config,
        other => panic!("{other:?}"),
    }
}

/// The events up to and including the first that `last` accepts.
async fn until(received: &mut UnboundedReceiver<Event>, last: impl Fn(&Event) -> bool) -> Vec<Event> {
    let mut events = Vec::new();
    loop {
        match timeout(LIMIT, received.recv()).await {
            Ok(Some(event)) => {
                let done = last(&event);
                events.push(event);
                if done {
                    return events;
                }
            }
            Ok(None) => panic!("the controller left: {events:#?}"),
            Err(_) => panic!("no awaited event within {LIMIT:?}: {events:#?}"),
        }
    }
}

fn completed(events: &[Event]) {
    assert!(
        matches!(events.last(), Some(Event::RunFinished { outcome: Outcome::Complete, .. })),
        "{events:#?}"
    );
}

#[tokio::test]
#[ignore = "needs a password-mode Go server; run rust/interop/client.py"]
async fn go_server_approves_a_native_sign_in_for_later_runs() {
    let url = std::env::var("GM_GO_AUTH_URL").expect("GM_GO_AUTH_URL names the Go server");
    let (events, mut received) = Events::channel();
    let mut controller = Controller::new(true, Arc::new(Pool::new().unwrap()), events);
    controller.command(Command::Run(config(&url, "http3", "webtransport", "webtransport")));
    let asked = until(&mut received, |event| matches!(event, Event::SignIn(_) | Event::RunFinished { .. })).await;
    let Some(Event::SignIn(prompt)) = asked.last() else {
        panic!("the server did not ask for sign-in: {asked:#?}")
    };
    println!("approve {}", prompt.url);
    let ran = until(&mut received, |event| matches!(event, Event::RunFinished { .. })).await;
    assert_eq!(ran[0], Event::SignInEnded(SignInEnd::Approved), "{ran:#?}");
    completed(&ran);
    println!("approved run completed over WebTransport streams with datagram latency");
    controller.command(Command::Run(config(&url, "http1", "fetch-stream", "websocket")));
    let ran = until(&mut received, |event| matches!(event, Event::RunFinished { .. })).await;
    assert!(!ran.iter().any(|event| matches!(event, Event::SignIn(_))), "{ran:#?}");
    completed(&ran);
    println!("the kept grant completed a run over HTTPS HTTP/1.1 fetch streams");
    controller.settled().await;
}
