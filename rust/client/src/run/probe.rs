//! Latency probing (`docs/MEASUREMENTS.md`): a pure `Schedule` of sends, deadlines and redials, and the loop that
//! runs it over a server's bus.
use crate::{
    measure::latency::{Deadline, ProbeOutcome},
    model::{Cadence, Stage},
    net::{Bus, Class, Client, Fault, LatencyPath, REDIAL_WINDOW, retrying},
    run::engine::Probe,
};
use graphite_meter_proto::bus::{Ping, Pong};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use tokio::{
    sync::{mpsc, watch},
    time::{sleep_until, timeout, timeout_at},
};
use tokio_util::sync::{CancellationToken, DropGuard};

/// How long a probe may take to leave.
const SEND_TIMEOUT: Duration = Duration::from_secs(1);
/// A channel lost this soon after it opened waits this long before it is dialed again.
const EARLY_LOSS: Duration = Duration::from_millis(500);
/// A timed-out probe's late reply still teaches the deadline this long after it went.
const LATE_REPLIES: Duration = Duration::from_secs(10);

/// The probes a prober keeps in flight: 16 idle at a fixed cadence, 4 reply-driven, 2 under load.
pub fn window(stage: Stage, cadence: Cadence) -> usize {
    match (stage, cadence) {
        (Stage::Latency, Cadence::Every(_)) => 16,
        (Stage::Latency, Cadence::ReplyDriven) => 4,
        _ => 2,
    }
}

/// When probes go and resolve: sends start to start or on each reply with the deadline as backup, a full window
/// skips a send, each probe's deadline is fixed as it goes, and after sending stops the rest drain to their deadlines.
#[derive(Debug)]
pub struct Schedule {
    cadence: Cadence,
    window: usize,
    deadline: Deadline,
    /// Each probe in flight: when it went and its deadline.
    pending: BTreeMap<u32, (Instant, Instant)>,
    /// Timed-out probes, when they went.
    late: BTreeMap<u32, Instant>,
    next: Ping,
    /// The next send; none once sending stopped.
    due: Option<Instant>,
    answered: bool,
}

impl Schedule {
    /// Sends at once, then as `cadence` says, with at most `window` probes in flight.
    pub fn new(cadence: Cadence, window: usize, now: Instant) -> Self {
        Self {
            cadence,
            window,
            deadline: Deadline::default(),
            pending: BTreeMap::new(),
            late: BTreeMap::new(),
            next: Ping { id: 0 },
            due: Some(now),
            answered: false,
        }
    }

    /// When a send or a deadline is next due; none once drained.
    pub fn wake(&self) -> Option<Instant> {
        let deadlines = self.pending.values().map(|&(_, deadline)| deadline);
        self.due.into_iter().chain(deadlines).min()
    }

    /// The probe to send at `now`, unless none is due or the window is full.
    pub fn send(&mut self, now: Instant) -> Option<Ping> {
        let due = self.due.filter(|due| now >= *due)?;
        let deadline = self.deadline.get();
        self.due = Some(match self.cadence {
            Cadence::ReplyDriven => now + deadline,
            Cadence::Every(spacing) => {
                let missed = now.duration_since(due).as_nanos() / spacing.as_nanos().max(1);
                due + spacing.saturating_mul(u32::try_from(missed + 1).unwrap_or(u32::MAX))
            }
        });
        if self.pending.len() >= self.window {
            return None;
        }
        let ping = self.next;
        self.next = ping.next();
        self.pending.insert(ping.id, (now, now + deadline));
        Some(ping)
    }

    /// What a reply at `now` resolves: a reply before its deadline, else a timeout; a late reply only teaches the
    /// deadline. Reply-driven probing sends again at once.
    pub fn reply(&mut self, pong: Pong, now: Instant) -> Option<Probe> {
        if self.cadence == Cadence::ReplyDriven && self.due.is_some() {
            self.due = Some(now);
        }
        if let Some(sent) = self.late.remove(&pong.id) {
            self.observe(now.saturating_duration_since(sent));
            return None;
        }
        let (sent, deadline) = self.pending.remove(&pong.id)?;
        let rtt = now.saturating_duration_since(sent);
        self.observe(rtt);
        let outcome = match now < deadline {
            true => ProbeOutcome::Reply { rtt, handling: Duration::from_nanos(pong.handling_nanos) },
            false => ProbeOutcome::Timeout,
        };
        Some(Probe::Outcome { sent, outcome })
    }

    /// Probes whose deadline passed by `now`, as timeouts.
    pub fn expire(&mut self, now: Instant) -> Vec<Probe> {
        self.late
            .retain(|_, sent| now.saturating_duration_since(*sent) <= LATE_REPLIES);
        let mut expired = Vec::new();
        self.pending.retain(|&id, &mut (sent, deadline)| {
            let waiting = now < deadline;
            if !waiting {
                expired.push(Probe::Outcome { sent, outcome: ProbeOutcome::Timeout });
                self.late.insert(id, sent);
            }
            waiting
        });
        expired
    }

    /// A probe that could not leave.
    pub fn failed(&mut self, ping: Ping) -> Option<Probe> {
        let (sent, _) = self.pending.remove(&ping.id)?;
        Some(Probe::Outcome { sent, outcome: ProbeOutcome::SendFailed })
    }

    /// The channel is gone: probes in flight stay unresolved.
    pub fn lost(&mut self) -> Vec<Probe> {
        self.late.clear();
        let pending = std::mem::take(&mut self.pending).into_values();
        pending
            .map(|(sent, _)| Probe::Outcome { sent, outcome: ProbeOutcome::Unresolved })
            .collect()
    }

    /// Sending stops; probes in flight drain to their deadlines.
    pub fn stop(&mut self) {
        self.due = None;
    }

    /// Whether any probe was answered, in time or late.
    pub fn answered(&self) -> bool {
        self.answered
    }

    /// When a channel lost at `now`, opened at `opened`, is dialed again and by when it must open: within 2 s, capped
    /// at the window's `end`; after 500 ms when it was lost within 500 ms of opening, unless that wait reaches the cap.
    /// None before the first reply or past the cap.
    pub fn redial(&self, opened: Instant, now: Instant, end: Option<Instant>) -> Option<(Instant, Instant)> {
        let bound = end.map_or(now + REDIAL_WINDOW, |end| end.min(now + REDIAL_WINDOW));
        if !self.answered || now >= bound {
            return None;
        }
        let paced = now + EARLY_LOSS;
        let early = now.saturating_duration_since(opened) < EARLY_LOSS && paced < bound;
        Some((if early { paced } else { now }, bound))
    }

    fn observe(&mut self, rtt: Duration) {
        self.deadline.observe(rtt);
        self.answered = true;
    }
}

/// A server's running prober; dropping it ends it.
#[derive(Debug)]
pub struct Prober {
    window: watch::Sender<Window>,
    probes: mpsc::UnboundedReceiver<Probe>,
    _ends: DropGuard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Window {
    Waiting,
    Open { end: Instant },
    Closed,
}

impl Prober {
    /// Probes `path` at `cadence` for `stage` until `token` is cancelled.
    pub fn spawn(client: Client, path: LatencyPath, stage: Stage, cadence: Cadence, token: CancellationToken) -> Self {
        let (window, windows) = watch::channel(Window::Waiting);
        let (sender, probes) = mpsc::unbounded_channel();
        let schedule = Schedule::new(cadence, self::window(stage, cadence), Instant::now());
        let probing = probe(client, path, schedule, windows, sender);
        tokio::spawn(token.clone().run_until_cancelled_owned(probing));
        Self { window, probes, _ends: token.drop_guard() }
    }

    /// The window opened; it ends at `end`, which caps a redial.
    pub fn open(&self, end: Instant) {
        self.window.send_replace(Window::Open { end });
    }

    /// The window closed: sending stops and probes in flight drain.
    pub fn close(&self) {
        self.window.send_replace(Window::Closed);
    }

    pub fn drain(&mut self, into: &mut Vec<Probe>) {
        while let Ok(probe) = self.probes.try_recv() {
            into.push(probe);
        }
    }
}

/// Dials the bus, then serves it and dials it again after a loss until the window drained or the population failed.
async fn probe(
    client: Client,
    path: LatencyPath,
    mut schedule: Schedule,
    mut window: watch::Receiver<Window>,
    out: mpsc::UnboundedSender<Probe>,
) {
    let emit = |probe| drop(out.send(probe));
    let (mut at, mut bound) = (Instant::now(), Instant::now() + REDIAL_WINDOW);
    let fault = loop {
        sleep_until(at.into()).await;
        let dialed = timeout_at(bound.into(), retrying(|| client.bus(&path))).await;
        let mut bus = match dialed.unwrap_or(Err(Fault::TimedOut("latency bus"))) {
            Ok(bus) => bus,
            Err(fault) => break fault,
        };
        let opened = Instant::now();
        emit(Probe::Up);
        let served = serve(&mut bus, &mut schedule, &mut window, &emit).await;
        schedule.lost().into_iter().for_each(emit);
        let (end, fault) = match (*window.borrow(), served) {
            (Window::Closed, _) | (_, Ok(())) => return emit(Probe::Drained),
            (Window::Open { end }, Err(fault)) => (Some(end), fault),
            (Window::Waiting, Err(fault)) => (None, fault),
        };
        match schedule.redial(opened, Instant::now(), end) {
            Some(redial) if fault.class() != Class::Final => (at, bound) = redial,
            _ => break fault,
        }
    };
    emit(Probe::Down { at: Instant::now(), failure: fault.failure() });
}

/// Sends, resolves and expires probes on `bus` until sending stopped and every probe resolved, or the bus failed.
async fn serve(
    bus: &mut Bus,
    schedule: &mut Schedule,
    window: &mut watch::Receiver<Window>,
    emit: &impl Fn(Probe),
) -> Result<(), Fault> {
    loop {
        if *window.borrow_and_update() == Window::Closed {
            schedule.stop();
        }
        let Some(wake) = schedule.wake() else { return Ok(()) };
        tokio::select! {
            biased;
            Ok(()) = window.changed() => {}
            () = sleep_until(wake.into()) => {
                let now = Instant::now();
                schedule.expire(now).into_iter().for_each(emit);
                let Some(ping) = schedule.send(now) else { continue };
                let sent = timeout(SEND_TIMEOUT, bus.send(ping)).await;
                if let Err(fault) = sent.unwrap_or(Err(Fault::TimedOut("probe send"))) {
                    schedule.failed(ping).into_iter().for_each(emit);
                    if !schedule.answered() {
                        return Err(fault);
                    }
                }
            }
            pong = bus.next() => schedule.reply(pong?, Instant::now()).into_iter().for_each(emit),
        }
    }
}
