//! The probe schedule against scripted times, and a prober against a loopback reflector.
use futures_util::{SinkExt, StreamExt};
use graphite_meter_client::{
    measure::latency::ProbeOutcome,
    model::{Cadence, Stage},
    net::{Client, LatencyPath},
    run::{
        engine::Probe,
        probe::{Prober, Schedule},
    },
};
use graphite_meter_net::Pool;
use graphite_meter_proto::{
    bus::{Ping, Pong},
    discovery::LatencyTransport,
    origin::Origin,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

const fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

fn pong(ping: Ping) -> Pong {
    Pong { id: ping.id, handling_nanos: 100 }
}

fn outcome(probe: Option<Probe>) -> ProbeOutcome {
    match probe {
        Some(Probe::Outcome { outcome, .. }) => outcome,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_lost_channel_redials_within_2_s_capped_at_the_window_end_after_500_ms_when_lost_early() {
    let t0 = Instant::now();
    let mut schedule = Schedule::new(Cadence::ReplyDriven, 4, t0);
    assert_eq!(schedule.redial(t0, t0 + ms(100), None), None, "nothing answered yet");
    let first = schedule.send(t0).unwrap();
    outcome(schedule.reply(pong(first), t0 + ms(20)));
    assert_eq!(schedule.redial(t0, t0 + ms(100), None), Some((t0 + ms(600), t0 + ms(2100))));
    assert_eq!(schedule.redial(t0, t0 + ms(1000), None), Some((t0 + ms(1000), t0 + ms(3000))));
    let end = Some(t0 + ms(1500));
    assert_eq!(schedule.redial(t0, t0 + ms(100), end), Some((t0 + ms(600), t0 + ms(1500))));
    let near = Some(t0 + ms(500));
    assert_eq!(
        schedule.redial(t0, t0 + ms(100), near),
        Some((t0 + ms(100), t0 + ms(500))),
        "no wait near the end"
    );
    assert_eq!(schedule.redial(t0, t0 + ms(500), near), None, "the window ended");
}

#[test]
fn a_reply_driven_probe_goes_on_the_reply_in_flight_and_never_on_a_late_one() {
    let t0 = Instant::now();
    let mut schedule = Schedule::new(Cadence::ReplyDriven, 4, t0);
    let first = schedule.send(t0).unwrap();
    let backup = t0 + ms(250);
    assert_eq!(schedule.wake(), Some(backup), "the first deadline backs the reply");
    assert_eq!(outcome(schedule.expire(backup).pop()), ProbeOutcome::Timeout);
    let second = schedule.send(backup).unwrap();
    assert_eq!(schedule.reply(pong(first), backup + ms(50)), None);
    assert_eq!(schedule.send(backup + ms(50)), None, "a late reply sent a probe");
    outcome(schedule.reply(pong(second), backup + ms(60)));
    assert_eq!(schedule.wake(), Some(backup + ms(60)), "the reply in flight sends at once");
    assert!(schedule.send(backup + ms(60)).is_some());
}

async fn reflector() -> Origin {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = Origin::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            tcp.set_nodelay(true).unwrap();
            tokio::spawn(async move {
                let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
                while let Some(Ok(Message::Text(text))) = socket.next().await {
                    let ping = Ping::decode(text.as_bytes()).unwrap();
                    if socket.send(Message::text(ping.reply(0).encode())).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    origin
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_driven_prober_keeps_a_probe_in_flight_on_loopback() {
    let path = LatencyPath {
        origin: reflector().await,
        transport: LatencyTransport::WebSocket,
    };
    let client = Client::new(false, Arc::new(Pool::inline()));
    let start = Instant::now();
    let mut prober = Prober::spawn(client, path, Stage::Latency, Cadence::ReplyDriven, CancellationToken::new());
    tokio::time::sleep(ms(500)).await;
    let rtts: Vec<_> = prober
        .drain()
        .filter_map(|probe| match probe {
            Probe::Outcome { outcome: ProbeOutcome::Reply { rtt, .. }, .. } => Some(rtt),
            _ => None,
        })
        .collect();
    let (busy, elapsed) = (rtts.iter().sum::<Duration>(), start.elapsed());
    assert!(busy * 2 > elapsed, "{} replies were in flight {busy:?} of {elapsed:?}", rtts.len());
}
