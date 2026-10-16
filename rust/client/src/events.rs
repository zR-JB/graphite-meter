//! What operations tell viewers as it happens, and the view report and interface reduce it to; sending never waits.
use crate::{
    controller::Kind,
    measure::{latency::Summary, live::LiveRate},
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
    Checking(Kind),
    /// The server catalog and the selected test servers with their paths, in catalog order.
    Prepared {
        servers: Arc<[ServerPath]>,
        catalog: Arc<[ServerEntry]>,
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
    /// A server's probes resolved since the last tick that it sent within the window, in resolution order, and its
    /// latency over the window so far.
    Probes {
        server: ServerId,
        probes: Box<[Probed]>,
        latency: Summary,
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
    /// The test server's name, or the server's origin.
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
    Canceled,
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

/// A probe sent `at` into the window: its round trip, or none for a timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probed {
    pub at: Duration,
    pub rtt: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub at: Duration,
    /// The mean of the merged values; none for a gap.
    pub value: Option<f64>,
    /// The merged values' lowest and highest.
    pub low: f64,
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
            (last.low, last.peak) = (last.low.min(value), last.peak.max(value));
            return;
        }
        if self.points.len() == POINTS {
            self.coarsen();
        }
        let peak = value.map_or(0.0, |value| value.max(0.0));
        self.points.push(Point { at, value, low: peak, peak, count: 1 });
    }

    /// Merges each pair of points in place; a gap absorbs its pair.
    fn coarsen(&mut self) {
        let kept = self.points.len().div_ceil(2);
        for index in 0..kept {
            let mut point = self.points[2 * index];
            if let Some(next) = self.points.get(2 * index + 1) {
                (point.low, point.peak) = (point.low.min(next.low), point.peak.max(next.peak));
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
    /// The latest preparation's test servers and server catalog.
    pub servers: Arc<[ServerPath]>,
    pub catalog: Arc<[ServerEntry]>,
    /// The run shown: the latest that started, kept while the next one checks its paths.
    pub run: Option<Run>,
    /// Whether a run checks its paths and has not started yet.
    pub starting: bool,
    /// A run that ended before it started, while an earlier one stays shown.
    pub refused: Option<Run>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Check {
    #[default]
    Idle,
    Checking,
    Ready,
    Failed(Failure),
    /// A sign-in the check asked for was canceled or expired.
    SignIn,
}

/// A stage in progress: warming up, or measuring from its window's offset into the plan.
#[derive(Debug, Clone)]
pub enum Running {
    Warmup(StagePlan),
    Measuring { plan: StagePlan, offset: Duration },
}

impl Running {
    pub fn plan(&self) -> &StagePlan {
        match self {
            Self::Warmup(plan) | Self::Measuring { plan, .. } => plan,
        }
    }

    fn into_plan(self) -> StagePlan {
        match self {
            Self::Warmup(plan) | Self::Measuring { plan, .. } => plan,
        }
    }
}

/// A run from its preparation on.
#[derive(Debug, Clone, Default)]
pub struct Run {
    pub plan: Vec<(Stage, Duration)>,
    /// The run's clock origin once it started.
    pub at: Option<Instant>,
    /// The latency server: the first selected one, or a survivor that measured latency once it left.
    pub focus: Option<ServerId>,
    /// The stage in progress.
    pub stage: Option<Running>,
    /// Each direction's presented rate, as the browser estimates it from the samples.
    pub live: Dir<LiveRate>,
    /// The latest sample's offset into the window.
    sampled: Duration,
    /// Presented rates in bytes per second.
    pub throughput: Dir<Series>,
    /// The highest combined rate presented or measured, in bytes per second; scales only grow during a run.
    pub peak: f64,
    /// The bytes moved so far: the finished stages' counts, and the running one's samples.
    pub moved: f64,
    /// Round trips in milliseconds per probed server.
    pub rtt: Vec<(ServerId, Series)>,
    /// Each server's latency in each stage while it runs.
    pub latency: Vec<(Stage, ServerId, Summary)>,
    pub results: Vec<StageResult>,
    /// Every failure with its stage, a finished stage's as its result records them.
    pub issues: Vec<(Stage, ServerFailure)>,
    pub outcome: Option<Outcome>,
    pub error: Option<Failure>,
    pub elapsed: Duration,
}

impl View {
    /// The run that ended last: one refused at its start, else the one shown.
    pub fn latest(&self) -> Option<&Run> {
        self.refused.as_ref().or(self.run.as_ref())
    }

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
            Event::Checking(Kind::Run) => (self.starting, self.refused) = (true, None),
            Event::Checking(Kind::Check) => self.check = Check::Checking,
            Event::Prepared { servers, catalog } => {
                (self.servers, self.catalog, self.check) = (servers.clone(), catalog.clone(), Check::Ready);
            }
            Event::CheckFailed(failure) if self.check == Check::Checking => self.check = Check::Failed(failure.clone()),
            Event::SignIn(prompt) => self.sign_in = Some(prompt.clone()),
            Event::SignInEnded(end) => {
                self.sign_in = None;
                if matches!(end, SignInEnd::Canceled | SignInEnd::Expired) && self.check == Check::Checking {
                    self.check = Check::SignIn;
                }
            }
            Event::RunStarted { plan, focus, at } => {
                (self.run, self.starting) = (Some(Run::new(plan, focus, *at)), false);
            }
            Event::RunFinished { outcome, error, elapsed } => {
                let earlier = std::mem::take(&mut self.starting) && self.run.is_some();
                let run = match earlier {
                    true => self.refused.insert(Run::default()),
                    false => self.run.get_or_insert_with(Run::default),
                };
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
                self.stage = Some(Running::Warmup(plan.clone()));
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
                    self.moved += rate * ms / 1e3;
                    if let Some(presented) = self.live[direction].presented() {
                        self.throughput[direction].push(offset + *at, Some(presented));
                    }
                }
                let presented = Direction::BOTH.map(|direction| self.live[direction].presented().unwrap_or_default());
                self.peak = self.peak.max(presented.iter().sum());
            }
            Event::Probes { server, probes, latency } => {
                let Some((stage, offset)) = self.window() else { return };
                let series = self.series(server);
                for probe in probes {
                    series.push(offset + probe.at, probe.rtt.map(|rtt| rtt.as_secs_f64() * 1e3));
                }
                let own = self
                    .latency
                    .iter_mut()
                    .find(|(own, id, _)| *own == stage && id == server);
                match own {
                    Some((.., summary)) => *summary = *latency,
                    None => self.latency.push((stage, server.clone(), *latency)),
                }
            }
            Event::ServerFailed(failure) => {
                let stage = self.stage.as_ref().map(|running| running.plan().stage);
                let Some(stage) = stage.or(self.plan.first().map(|(stage, _)| *stage)) else {
                    return;
                };
                self.issues.push((stage, failure.clone()));
            }
            Event::StageFinished(result) => self.finished(result),
            Event::Checking(_)
            | Event::Prepared { .. }
            | Event::CheckFailed(_)
            | Event::SignIn(_)
            | Event::SignInEnded(_)
            | Event::RunStarted { .. }
            | Event::RunFinished { .. } => {}
        }
    }

    /// Opens the stage's window at its planned offset and breaks every trace there.
    fn measuring(&mut self, stage: Stage) {
        let before = self.plan.iter().take_while(|(planned, _)| *planned != stage);
        let offset = before.map(|(_, duration)| *duration).sum();
        self.stage = self
            .stage
            .take()
            .map(|running| Running::Measuring { plan: running.into_plan(), offset });
        self.sampled = Duration::ZERO;
        for direction in Direction::BOTH {
            self.throughput[direction].push(offset, None);
        }
        for (_, series) in &mut self.rtt {
            series.push(offset, None);
        }
    }

    fn window(&self) -> Option<(Stage, Duration)> {
        match self.stage.as_ref()? {
            Running::Measuring { plan, offset } => Some((plan.stage, *offset)),
            Running::Warmup(_) => None,
        }
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
        let counted = self
            .results
            .iter()
            .flat_map(|result| [result.throughput.down, result.throughput.up]);
        self.moved = counted.flatten().map(|throughput| throughput.bytes as f64).sum();
        self.focus = focus(&self.results).or(self.focus.take());
        self.stage = None;
    }
}
