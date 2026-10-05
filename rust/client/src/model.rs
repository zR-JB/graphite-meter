//! The crate's vocabulary: stages, directions, cadences and failures.

use graphite_meter_proto::reason::FailureReason;
use std::{
    fmt,
    ops::{Index, IndexMut},
    time::Duration,
};

/// A measured stage; runs take them in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    Latency,
    Download,
    Upload,
    Bidirectional,
}

impl Stage {
    pub const ALL: [Self; 4] = [Self::Latency, Self::Download, Self::Upload, Self::Bidirectional];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Latency => "latency",
            Self::Download => "download",
            Self::Upload => "upload",
            Self::Bidirectional => "bidirectional",
        }
    }

    /// The directions the stage moves bytes in; none for latency.
    pub const fn directions(self) -> &'static [Direction] {
        match self {
            Self::Latency => &[],
            Self::Download => &[Direction::Down],
            Self::Upload => &[Direction::Up],
            Self::Bidirectional => &[Direction::Down, Direction::Up],
        }
    }

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

/// One value per direction.
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
        Self { reason, text: text.into() }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

/// What a failure ends: a server's path, its throughput or its latency population.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Server,
    Throughput,
    Latency,
}
