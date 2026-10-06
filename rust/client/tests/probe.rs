//! The probe schedule against scripted times.
use graphite_meter_client::{
    measure::latency::ProbeOutcome,
    model::Cadence,
    run::{engine::Probe, probe::Schedule},
};
use graphite_meter_proto::bus::{Ping, Pong};
use std::time::{Duration, Instant};

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
