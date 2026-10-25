//! The crate's vocabulary: stages, directions, cadences, failures, stage results and the run's outcome.

use crate::measure::{aggregate::Rate, latency::Population};
use graphite_meter_proto::{catalog::ServerId, reason::FailureReason};
use std::{
    ops::{Index, IndexMut},
    time::{Duration, Instant},
};

graphite_meter_proto::table! {
    /// A measured stage; runs take them in this order.
    #[derive(PartialOrd, Ord)]
    pub enum Stage {
        name.0: &'static str,
        directions.1: &'static [Direction],
    } {
        Latency => ("latency", &[]),
        Download => ("download", &[Direction::Down]),
        Upload => ("upload", &[Direction::Up]),
        Bidirectional => ("bidirectional", &[Direction::Down, Direction::Up]),
    }
}

impl Stage {
    pub fn moves(self, direction: Direction) -> bool {
        self.directions().contains(&direction)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Down,
    Up,
}

impl Direction {
    pub const BOTH: [Self; 2] = [Self::Down, Self::Up];
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dir<T> {
    pub down: T,
    pub up: T,
}

impl<T> Dir<T> {
    pub fn from_fn(mut value: impl FnMut(Direction) -> T) -> Self {
        Self {
            down: value(Direction::Down),
            up: value(Direction::Up),
        }
    }
}

impl Dir<Option<Throughput>> {
    pub fn mean(&self) -> f64 {
        [self.down, self.up]
            .into_iter()
            .flatten()
            .filter_map(|throughput| Some(throughput.rate?.mean))
            .sum()
    }

    pub fn bytes(&self) -> u64 {
        [self.down, self.up]
            .into_iter()
            .flatten()
            .map(|throughput| throughput.bytes)
            .sum()
    }
}

impl<T> Index<Direction> for Dir<T> {
    type Output = T;

    fn index(&self, direction: Direction) -> &T {
        match direction {
            Direction::Down => &self.down,
            Direction::Up => &self.up,
        }
    }
}

impl<T> IndexMut<Direction> for Dir<T> {
    fn index_mut(&mut self, direction: Direction) -> &mut T {
        match direction {
            Direction::Down => &mut self.down,
            Direction::Up => &mut self.up,
        }
    }
}

/// When a prober sends: on each reply, or start to start at a fixed spacing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cadence {
    ReplyDriven,
    Every(Duration),
}

/// Why a server's stage or path failed, decided where the failure arose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub reason: FailureReason,
    pub text: String,
}

impl Failure {
    pub fn new(reason: FailureReason, text: impl Into<String>) -> Self {
        Self {
            reason,
            text: text.into(),
        }
    }
}

/// What a failure ends: a server's path, its throughput or its latency population.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Server,
    Throughput,
    Latency,
}

/// A direction's lanes: moving, retrying a failure, or failed for good.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneHealth {
    Ok,
    Retrying(Failure),
    Failed(Failure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFailure {
    pub server: ServerId,
    pub scope: Scope,
    pub failure: Failure,
    pub at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Throughput {
    pub rate: Option<Rate>,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerResult {
    pub server: ServerId,
    pub left: bool,
    pub throughput: Dir<Option<Throughput>>,
    pub latency: Option<Population>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StageResult {
    pub stage: Stage,
    /// The measured window's length; zero when it never opened.
    pub measured: Duration,
    pub stopped: bool,
    /// All servers' headline per direction the stage moves, once its window opened.
    pub throughput: Dir<Option<Throughput>>,
    /// Every server that began the stage, in selection order.
    pub servers: Vec<ServerResult>,
    pub failures: Vec<ServerFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageStatus {
    Complete,
    Partial,
    Failed,
    Stopped,
}

impl StageResult {
    /// Whether a failure of `scope` is this stage's own; loaded latency lost leaves a transfer stage whole.
    pub fn owns(&self, scope: Scope) -> bool {
        match scope {
            Scope::Server => true,
            Scope::Latency => self.stage == Stage::Latency,
            Scope::Throughput => self.stage != Stage::Latency,
        }
    }

    pub fn absent<'a>(&self, selected: &'a [ServerId]) -> impl Iterator<Item = &'a ServerId> {
        selected
            .iter()
            .filter(|id| !self.servers.iter().any(|server| server.server == **id))
    }

    pub fn status(&self, focus: Option<&ServerId>, selected: &[ServerId]) -> StageStatus {
        let missing = |direction: &Direction| {
            self.throughput[*direction].is_none_or(|throughput| throughput.rate.is_none())
        };
        let focused = self
            .servers
            .iter()
            .find(|server| Some(&server.server) == focus);
        let median = focused.and_then(|server| server.latency?.median());
        match () {
            _ if self.stopped => StageStatus::Stopped,
            _ if self.stage.directions().iter().any(missing) => StageStatus::Failed,
            _ if self.stage == Stage::Latency && median.is_none() => StageStatus::Failed,
            _ if self.failures.iter().any(|failure| self.owns(failure.scope)) => {
                StageStatus::Partial
            }
            _ if self.absent(selected).next().is_some() => StageStatus::Partial,
            _ => StageStatus::Complete,
        }
    }
}

pub fn selection(results: &[StageResult]) -> Vec<ServerId> {
    let Some(first) = results.first() else {
        return Vec::new();
    };
    let mut selected: Vec<_> = first.servers.iter().map(|own| own.server.clone()).collect();
    let dropped = first
        .failures
        .iter()
        .filter(|failure| failure.scope == Scope::Server);
    for failure in dropped {
        if !selected.contains(&failure.server) {
            selected.push(failure.server.clone());
        }
    }
    selected
}

/// The run's latency server: the first selected one while it stays, else the first survivor with a latency median.
pub fn focus(results: &[StageResult]) -> Option<ServerId> {
    let first = &results.first()?.servers.first()?.server;
    let mut survivors = results.last()?.servers.iter().filter(|server| !server.left);
    if survivors.clone().any(|server| server.server == *first) {
        return Some(first.clone());
    }
    let idle = results
        .iter()
        .find(|result| result.stage == Stage::Latency)?;
    let measured = |survivor: &&ServerResult| {
        let mut servers = idle
            .servers
            .iter()
            .filter(|server| server.server == survivor.server);
        servers.any(|server| {
            server
                .latency
                .and_then(|latency| latency.median())
                .is_some()
        })
    };
    survivors.find(measured).map(|server| server.server.clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Every planned stage finished with every server.
    Complete,
    /// Every stage has its results, but a server or latency population failed.
    Partial,
    /// A planned result is missing after measurement began.
    Incomplete,
    Stopped,
    Failed,
}

impl Outcome {
    /// From the stage results; the first carries the failures of selected servers that never began a stage.
    pub fn of(results: &[StageResult], plan: &[Stage]) -> Self {
        let (focus, selected) = (focus(results), selection(results));
        let statuses: Vec<_> = results
            .iter()
            .map(|result| result.status(focus.as_ref(), &selected))
            .collect();
        let ran = |stage: &Stage| results.iter().any(|result| result.stage == *stage);
        let planned = plan.iter().all(ran);
        match () {
            _ if results.iter().any(|result| result.stopped) => Self::Stopped,
            _ if results.iter().all(|result| result.measured.is_zero()) => Self::Failed,
            _ if !planned || statuses.contains(&StageStatus::Failed) => Self::Incomplete,
            _ if statuses.contains(&StageStatus::Partial)
                || results.iter().any(|result| !result.failures.is_empty()) =>
            {
                Self::Partial
            }
            _ => Self::Complete,
        }
    }
}
