//! What a run tells its viewers as it happens; sending never waits for them.
use crate::{
    model::{Dir, Failure, Outcome, Scope, Stage, StageResult},
    run::engine::StagePlan,
};
use graphite_meter_proto::catalog::ServerId;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    RunStarted {
        plan: Vec<(Stage, Duration)>,
        focus: ServerId,
    },
    StageStarted(StagePlan),
    /// The stage's measured window opened.
    Measuring,
    /// A boundary's all-servers rates in bytes per second, `at` into the window.
    Sample {
        at: Duration,
        rates: Dir<Option<f64>>,
        recovering: bool,
    },
    /// A probe sent `at` into the window: its round trip, or none for a timeout.
    Probe {
        server: ServerId,
        at: Duration,
        rtt: Option<Duration>,
    },
    /// A failure `at` into the run.
    ServerFailed {
        server: ServerId,
        scope: Scope,
        failure: Failure,
        at: Duration,
    },
    StageFinished(StageResult),
    RunFinished {
        outcome: Outcome,
        error: Option<Failure>,
    },
}

/// The sending side of a run's events.
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
