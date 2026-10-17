//! Defines the crate's vocabulary, from stages, directions and cadences to failures, stage results and a run's outcome.

use crate::measure::{aggregate::Rate, latency::Population};
use graphite_meter_proto::{catalog::ServerId, reason::FailureReason};
use std::{
    ops::{Index, IndexMut},
    time::{Duration, Instant},
};

graphite_meter_proto::table! {
    /// A run measures these stages in this order.
    #[derive(PartialOrd, Ord)]
    pub enum Stage {
        name.0: &'static str,
        /// The stage moves bytes in these directions, none for latency.
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

/// Holds one value per direction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dir<T> {
    pub down: T,
    pub up: T,
}

impl<T> Dir<T> {
    pub fn from_fn(mut value: impl FnMut(Direction) -> T) -> Self {
        Self { down: value(Direction::Down), up: value(Direction::Up) }
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

/// A prober sends on each reply, or start to start at a fixed spacing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cadence {
    ReplyDriven,
    Every(Duration),
}

/// Says why a server's stage or path failed, decided where the failure arose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub reason: FailureReason,
    pub text: String,
}

impl Failure {
    pub fn new(reason: FailureReason, text: impl Into<String>) -> Self {
        Self { reason, text: text.into() }
    }
}

/// A failure ends a server's path, its throughput or its latency population.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Server,
    Throughput,
    Latency,
}

/// A direction's lanes are moving, retrying a failure, or failed for good.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneHealth {
    Ok,
    Retrying(Failure),
    Failed(Failure),
}

/// Holds a failure as a stage recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFailure {
    pub server: ServerId,
    pub scope: Scope,
    pub failure: Failure,
    pub at: Instant,
}

/// Holds a direction's headline, if any, and its unique measured bytes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Throughput {
    pub rate: Option<Rate>,
    pub bytes: u64,
}

/// Holds one server's share of a stage.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerResult {
    pub server: ServerId,
    /// The server left the run in this stage.
    pub left: bool,
    /// Set for each direction the stage moves.
    pub throughput: Dir<Option<Throughput>>,
    /// Set when the stage probed this server.
    pub latency: Option<Population>,
}

/// Holds what a stage measured.
#[derive(Debug, Clone, PartialEq)]
pub struct StageResult {
    pub stage: Stage,
    /// The measured window lasted this long, zero when it never opened.
    pub measured: Duration,
    pub stopped: bool,
    /// Holds all servers' headline per direction the stage moves, once its window opened.
    pub throughput: Dir<Option<Throughput>>,
    /// Lists every server that began the stage, in selection order.
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
    /// Adds the directions' measured mean rates, in bytes per second.
    pub fn mean(&self) -> f64 {
        let directions = [self.throughput.down, self.throughput.up].into_iter().flatten();
        directions.filter_map(|throughput| Some(throughput.rate?.mean)).sum()
    }

    pub fn status(&self, focus: Option<&ServerId>) -> StageStatus {
        let missing =
            |direction: &Direction| self.throughput[*direction].is_none_or(|throughput| throughput.rate.is_none());
        let focused = self.servers.iter().find(|server| Some(&server.server) == focus);
        let median = focused.and_then(|server| server.latency?.median());
        match () {
            _ if self.stopped => StageStatus::Stopped,
            _ if self.stage.directions().iter().any(missing) => StageStatus::Failed,
            _ if self.stage == Stage::Latency && median.is_none() => StageStatus::Failed,
            _ if !self.failures.is_empty() => StageStatus::Partial,
            _ => StageStatus::Complete,
        }
    }
}

/// The run's latency server is the first selected one while it stays, else the first survivor with a latency median.
pub fn focus(results: &[StageResult]) -> Option<ServerId> {
    let first = &results.first()?.servers.first()?.server;
    let mut survivors = results.last()?.servers.iter().filter(|server| !server.left);
    if survivors.clone().any(|server| server.server == *first) {
        return Some(first.clone());
    }
    let idle = results.iter().find(|result| result.stage == Stage::Latency)?;
    let measured = |survivor: &&ServerResult| {
        let mut servers = idle.servers.iter().filter(|server| server.server == survivor.server);
        servers.any(|server| server.latency.and_then(|latency| latency.median()).is_some())
    };
    survivors.find(measured).map(|server| server.server.clone())
}

/// A run ends in one of these ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Every planned stage finished with every server.
    Complete,
    /// Every stage has its results, but a server or latency population failed.
    Partial,
    /// A planned result is missing after measurement began.
    Incomplete,
    Stopped,
    /// Nothing was measured.
    Failed,
}

impl Outcome {
    /// Judges the stage results. The first carries the failures of selected servers that never began a stage.
    pub fn of(results: &[StageResult], plan: &[Stage]) -> Self {
        let focus = focus(results);
        let statuses: Vec<_> = results.iter().map(|result| result.status(focus.as_ref())).collect();
        let ran = |stage: &Stage| results.iter().any(|result| result.stage == *stage);
        let planned = plan.iter().all(ran);
        match () {
            _ if results.iter().any(|result| result.stopped) => Self::Stopped,
            _ if results.iter().all(|result| result.measured.is_zero()) => Self::Failed,
            _ if !planned || statuses.contains(&StageStatus::Failed) => Self::Incomplete,
            _ if statuses.contains(&StageStatus::Partial) => Self::Partial,
            _ => Self::Complete,
        }
    }
}
