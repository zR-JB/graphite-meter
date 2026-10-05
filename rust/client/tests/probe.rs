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
fn a_fixed_cadence_sends_start_to_start_on_its_grid() {
    let t0 = Instant::now();
    let mut schedule = Schedule::new(Cadence::Every(ms(250)), 16, t0);
    let first = schedule.send(t0).unwrap();
    assert_eq!(schedule.send(t0 + ms(249)), None);
    outcome(schedule.reply(pong(first), t0 + ms(400)));
    assert_eq!(schedule.send(t0 + ms(400)).map(|ping| ping.id), Some(1), "a reply does not move the grid");
    assert_eq!(schedule.send(t0 + ms(600)).map(|ping| ping.id), Some(2), "a late wake sends once");
    assert_eq!(schedule.send(t0 + ms(700)), None);
    assert_eq!(schedule.send(t0 + ms(750)).map(|ping| ping.id), Some(3));
}

#[test]
fn reply_driven_probing_sends_on_each_reply_with_the_deadline_as_backup() {
    let t0 = Instant::now();
    let mut schedule = Schedule::new(Cadence::ReplyDriven, 4, t0);
    let first = schedule.send(t0).unwrap();
    assert_eq!(schedule.wake(), Some(t0 + ms(250)), "250 ms before the first reply");
    let reply = outcome(schedule.reply(pong(first), t0 + ms(30)));
    assert_eq!(reply, ProbeOutcome::Reply { rtt: ms(30), handling: Duration::from_nanos(100) });
    assert!(schedule.send(t0 + ms(30)).is_some(), "the reply sends the next");
    assert_eq!(schedule.send(t0 + ms(279)), None);
    assert!(schedule.send(t0 + ms(280)).is_some(), "the deadline backs up a missing reply");
}

#[test]
fn a_full_window_skips_a_send_without_counting_a_timeout() {
    let t0 = Instant::now();
    let mut schedule = Schedule::new(Cadence::Every(ms(80)), 2, t0);
    assert!(schedule.send(t0).is_some() && schedule.send(t0 + ms(80)).is_some());
    assert_eq!(schedule.send(t0 + ms(160)), None);
    assert!(schedule.expire(t0 + ms(249)).is_empty());
    let expired = schedule.expire(t0 + ms(250));
    assert_eq!(expired, [Probe::Outcome { sent: t0, outcome: ProbeOutcome::Timeout }]);
    assert!(schedule.send(t0 + ms(250)).is_some(), "the window has room again");
}

#[test]
fn after_sending_stops_probes_drain_to_their_deadlines_and_late_replies_change_nothing() {
    let t0 = Instant::now();
    let mut schedule = Schedule::new(Cadence::Every(ms(80)), 16, t0);
    let (first, second) = (schedule.send(t0).unwrap(), schedule.send(t0 + ms(80)).unwrap());
    schedule.stop();
    assert_eq!(schedule.send(t0 + ms(160)), None);
    assert!(matches!(outcome(schedule.reply(pong(second), t0 + ms(120))), ProbeOutcome::Reply { .. }));
    assert_eq!(schedule.wake(), Some(t0 + ms(250)), "the first probe's deadline");
    assert_eq!(outcome(schedule.expire(t0 + ms(250)).pop()), ProbeOutcome::Timeout);
    assert_eq!(schedule.wake(), None, "drained");
    assert_eq!(schedule.reply(pong(first), t0 + ms(300)), None, "a late reply cannot erase a timeout");
}

#[test]
fn a_lost_channel_leaves_its_probes_unresolved_and_a_send_failure_apart() {
    let t0 = Instant::now();
    let mut schedule = Schedule::new(Cadence::Every(ms(80)), 16, t0);
    let first = schedule.send(t0).unwrap();
    assert_eq!(outcome(schedule.failed(first)), ProbeOutcome::SendFailed);
    schedule.send(t0 + ms(80)).unwrap();
    assert_eq!(schedule.lost(), [Probe::Outcome { sent: t0 + ms(80), outcome: ProbeOutcome::Unresolved }]);
    assert!(schedule.expire(t0 + ms(10_000)).is_empty());
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
