//! What operations tell viewers as it happens, and the view report and interface reduce it to; sending never waits.
use crate::{
    measure::live::LiveRate,
    model::{Dir, Direction, Failure, Outcome, ServerFailure, Stage, StageResult, focus},
    run::{engine::StagePlan, prepare::ServerPath},
};
use graphite_meter_proto::catalog::{ServerEntry, ServerId};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A path check began, or a run's preparation.
    Checking {
        run: bool,
    },
    /// The catalogue and the selected servers with their paths, in catalogue order.
    Prepared {
        servers: Arc<[ServerPath]>,
        catalogue: Arc<[ServerEntry]>,
    },
    /// The path check could not finish.
    CheckFailed(Failure),
    /// A server asks for sign-in until `SignInEnded`.
    SignIn(SignInPrompt),
    SignInEnded(SignInEnd),
    /// The run's plan, its first latency server and its clock's origin.
    RunStarted {
        plan: Vec<(Stage, Duration)>,
        focus: ServerId,
        at: Instant,
    },
    StageStarted(StagePlan),
    /// The stage's measured window opened.
    Measuring(Stage),
    /// A boundary's all-servers rates in bytes per second, `at` into the window; none restarts a direction.
    Sample {
        at: Duration,
        rates: Dir<Option<f64>>,
    },
    /// A probe sent `at` into the window: its round trip, or none for a timeout.
    Probe {
        server: ServerId,
        at: Duration,
        rtt: Option<Duration>,
    },
    /// A failure as it happens.
    ServerFailed(ServerFailure),
    StageFinished(StageResult),
    /// The run ended `elapsed` after it began; `error` says why it never started.
    RunFinished {
        outcome: Outcome,
        error: Option<Failure>,
        elapsed: Duration,
    },
}

/// What the sign-in screen shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInPrompt {
    /// The server's name, or the catalogue's origin.
    pub issuer: String,
    /// The approval page.
    pub url: String,
    pub code: String,
    pub deadline: tokio::time::Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInEnd {
    Approved,
    Expired,
    Failed,
    Cancelled,
}

/// The sending side of the events.
#[derive(Debug, Clone)]
pub struct Events(mpsc::UnboundedSender<Event>);

impl Events {
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<Event>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self(sender), receiver)
    }

    /// Sends `event`; a viewer that left misses it.
    pub fn send(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

/// The most points a series keeps; at the limit every pair merges and the step doubles.
const POINTS: usize = 480;
/// The shortest spacing of points.
const STEP: Duration = Duration::from_millis(50);

/// A trace over the plan's measured time, keeping its planned span; nearby values merge into peak-keeping means.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Series {
    pub points: Vec<Point>,
    step: Duration,
    pub span: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub at: Duration,
    /// The mean of the merged values; none for a gap.
    pub value: Option<f64>,
    pub peak: f64,
    /// How many values merged into it.
    pub count: u32,
}

impl Series {
    pub fn new(span: Duration) -> Self {
        Self { points: Vec::new(), step: STEP, span }
    }

    /// Adds `value` at `at`; none breaks the trace.
    pub fn push(&mut self, at: Duration, value: Option<f64>) {
        if let (Some(last), Some(value)) = (self.points.last_mut(), value)
            && let Some(mean) = last.value.filter(|_| at.saturating_sub(last.at) < self.step)
        {
            last.count += 1;
            last.value = Some(mean + (value - mean) / f64::from(last.count));
            last.peak = last.peak.max(value);
            return;
        }
        if self.points.len() == POINTS {
            self.coarsen();
        }
        let peak = value.map_or(0.0, |value| value.max(0.0));
        self.points.push(Point { at, value, peak, count: 1 });
    }

    /// Merges each pair of points in place; a gap absorbs its pair.
    fn coarsen(&mut self) {
        let kept = self.points.len().div_ceil(2);
        for index in 0..kept {
            let mut point = self.points[2 * index];
            if let Some(next) = self.points.get(2 * index + 1) {
                point.peak = point.peak.max(next.peak);
                point.value = match (point.value, next.value) {
                    (Some(mean), Some(other)) => {
                        let (count, others) = (f64::from(point.count), f64::from(next.count));
                        Some((mean * count + other * others) / (count + others))
                    }
                    _ => None,
                };
                point.count += next.count;
            }
            self.points[index] = point;
        }
        self.points.truncate(kept);
        self.step *= 2;
    }
}

/// What the operations so far showed.
#[derive(Debug, Clone, Default)]
pub struct View {
    pub check: Check,
    pub sign_in: Option<SignInPrompt>,
    /// The latest preparation's servers and catalogue.
    pub servers: Arc<[ServerPath]>,
    pub catalogue: Arc<[ServerEntry]>,
    pub run: Option<Run>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Check {
    #[default]
    Idle,
    Checking,
    Ready,
    Failed(Failure),
    /// A sign-in the check asked for was cancelled or expired.
    SignIn,
}

/// A run from its preparation on.
#[derive(Debug, Clone, Default)]
pub struct Run {
    pub plan: Vec<(Stage, Duration)>,
    /// The run's clock origin once it started.
    pub at: Option<Instant>,
    /// The latency server: the first selected one, or a survivor that measured latency once it left.
    pub focus: Option<ServerId>,
    /// The stage in progress, and its window's offset into the plan once open.
    pub stage: Option<(StagePlan, Option<Duration>)>,
    /// Each direction's presented rate, as the browser estimates it from the samples.
    pub live: Dir<LiveRate>,
    /// The latest sample's offset into the window.
    sampled: Duration,
    /// Presented rates in bytes per second.
    pub throughput: Dir<Series>,
    /// The highest combined rate presented or measured, in bytes per second; scales only grow during a run.
    pub peak: f64,
    /// Round trips in milliseconds per probed server.
    pub rtt: Vec<(ServerId, Series)>,
    pub results: Vec<StageResult>,
    /// Every failure with its stage, a finished stage's as its result records them.
    pub issues: Vec<(Stage, ServerFailure)>,
    pub outcome: Option<Outcome>,
    pub error: Option<Failure>,
    pub elapsed: Duration,
}

impl View {
    /// Whether `server` is still in the run: in its latest stage without leaving, or prepared before any.
    pub fn remains(&self, server: &ServerId) -> bool {
        let results = self.run.iter().flat_map(|run| run.results.iter().rev());
        let mut own = results.filter_map(|result| result.servers.iter().find(|own| own.server == *server));
        let prepared = |prepared: &ServerPath| prepared.id == *server && prepared.path.is_ok();
        match own.next() {
            Some(own) => !own.left,
            None => self.servers.iter().any(prepared),
        }
    }

    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::Checking { run: true } => self.run = Some(Run::default()),
            Event::Checking { run: false } => self.check = Check::Checking,
            Event::Prepared { servers, catalogue } => {
                (self.servers, self.catalogue, self.check) = (servers.clone(), catalogue.clone(), Check::Ready);
            }
            Event::CheckFailed(failure) if self.check == Check::Checking => self.check = Check::Failed(failure.clone()),
            Event::SignIn(prompt) => self.sign_in = Some(prompt.clone()),
            Event::SignInEnded(end) => {
                self.sign_in = None;
                if matches!(end, SignInEnd::Cancelled | SignInEnd::Expired) && self.check == Check::Checking {
                    self.check = Check::SignIn;
                }
            }
            Event::RunStarted { plan, focus, at } => self.run = Some(Run::new(plan, focus, *at)),
            Event::RunFinished { outcome, error, elapsed } => {
                let run = self.run.get_or_insert_with(Run::default);
                (run.outcome, run.error, run.elapsed, run.stage) = (Some(*outcome), error.clone(), *elapsed, None);
            }
            event => {
                if let Some(run) = &mut self.run {
                    run.apply(event);
                }
            }
        }
    }
}

impl Run {
    fn new(plan: &[(Stage, Duration)], focus: &ServerId, at: Instant) -> Self {
        let span = plan.iter().map(|(_, duration)| *duration).sum();
        Self {
            plan: plan.to_vec(),
            at: Some(at),
            focus: Some(focus.clone()),
            throughput: Dir::from_fn(|_| Series::new(span)),
            ..Self::default()
        }
    }

    /// The run's events from its start to its end.
    fn apply(&mut self, event: &Event) {
        match event {
            Event::StageStarted(plan) => {
                self.stage = Some((plan.clone(), None));
                self.live = Dir::default();
            }
            Event::Measuring(stage) => self.measuring(*stage),
            Event::Sample { at, rates } => {
                let Some((stage, offset)) = self.window() else { return };
                let ms = at
                    .saturating_sub(std::mem::replace(&mut self.sampled, *at))
                    .as_secs_f64()
                    * 1e3;
                for &direction in stage.directions() {
                    let Some(rate) = rates[direction] else {
                        // The window restarts, and so does what it presents.
                        self.live[direction] = LiveRate::default();
                        self.throughput[direction].push(offset + *at, None);
                        continue;
                    };
                    self.live[direction].observe(rate * ms / 1e3, ms);
                    let presented = self.live[direction].presented();
                    if presented > 0.0 {
                        self.throughput[direction].push(offset + *at, Some(presented));
                    }
                }
                self.peak = self.peak.max(self.live.down.presented() + self.live.up.presented());
            }
            Event::Probe { server, at, rtt } => {
                let Some((_, offset)) = self.window() else { return };
                let rtt = rtt.map(|rtt| rtt.as_secs_f64() * 1e3);
                self.series(server).push(offset + *at, rtt);
            }
            Event::ServerFailed(failure) => {
                let stage = self.stage.as_ref().map(|(plan, _)| plan.stage);
                let Some(stage) = stage.or(self.plan.first().map(|(stage, _)| *stage)) else {
                    return;
                };
                self.issues.push((stage, failure.clone()));
            }
            Event::StageFinished(result) => self.finished(result),
            _ => {}
        }
    }

    /// Opens the stage's window at its planned offset and breaks every trace there.
    fn measuring(&mut self, stage: Stage) {
        let before = self.plan.iter().take_while(|(planned, _)| *planned != stage);
        let offset = before.map(|(_, duration)| *duration).sum();
        if let Some((_, window)) = &mut self.stage {
            *window = Some(offset);
        }
        self.sampled = Duration::ZERO;
        for direction in Direction::BOTH {
            self.throughput[direction].push(offset, None);
        }
        for (_, series) in &mut self.rtt {
            series.push(offset, None);
        }
    }

    fn window(&self) -> Option<(Stage, Duration)> {
        let (plan, window) = self.stage.as_ref()?;
        Some((plan.stage, (*window)?))
    }

    fn series(&mut self, server: &ServerId) -> &mut Series {
        let at = self.rtt.iter().position(|(probed, _)| probed == server);
        let at = at.unwrap_or_else(|| {
            self.rtt.push((server.clone(), Series::new(self.throughput.down.span)));
            self.rtt.len() - 1
        });
        &mut self.rtt[at].1
    }

    /// Keeps `result`, whose failures replace the stage's announced ones, and moves the focus off a server that left.
    fn finished(&mut self, result: &StageResult) {
        self.issues.retain(|(stage, _)| *stage != result.stage);
        let failures = result.failures.iter().map(|failure| (result.stage, failure.clone()));
        self.issues.extend(failures);
        self.peak = self.peak.max(result.mean());
        self.results.push(result.clone());
        self.focus = focus(&self.results).or(self.focus.take());
        self.stage = None;
    }
}
