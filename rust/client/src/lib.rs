//! The Graphite Meter native terminal client.

pub mod config;
pub mod controller;
pub mod events;
pub mod model;
pub mod net;
pub mod report;
pub mod text;

pub mod measure {
    pub mod aggregate;
    pub mod format;
    pub mod latency;
    pub mod live;
}

pub mod run {
    pub mod coordinator;
    pub mod engine;
    pub mod prepare;
    pub mod probe;
    pub mod select;
    pub mod upload;
}

pub mod tui;

use config::Config;
use controller::{Command, Controller};
use events::{Event, Events, View};
use graphite_meter_net::Pool;
use model::Outcome;
use std::{io::Write, pin::pin, sync::Arc};

/// The version `--version` prints.
pub const VERSION: &str = match option_env!("GM_ENGINE_VERSION") {
    Some(version) => version,
    None => concat!(env!("CARGO_PKG_VERSION"), "-rust-dev"),
};

/// The statuses of an interrupt and of a termination that stopped a run.
pub const INTERRUPTED: u8 = 130;
pub const TERMINATED: u8 = 143;

/// What a caught interrupt or termination does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reaction {
    /// Stop the run, which ends with this status.
    Stop(u8),
    /// Exit at once with this status.
    Exit(u8),
}

/// The signals caught so far: the first stops the run, a later one exits at once.
#[derive(Debug, Default)]
pub struct Interrupts {
    stopping: bool,
}

impl Interrupts {
    /// The reaction to a signal whose status is `status`.
    pub fn on(&mut self, status: u8) -> Reaction {
        match std::mem::replace(&mut self.stopping, true) {
            false => Reaction::Stop(status),
            true => Reaction::Exit(status),
        }
    }
}

/// Runs `config` once without the interface, stage progress to stderr; the view and the status, which `stop` can set.
pub async fn headless(config: Config, runtimes: Arc<Pool>, stop: impl Future<Output = u8>) -> (View, u8) {
    let (events, mut received) = Events::channel();
    let mut controller = Controller::new(false, runtimes, events);
    controller.command(Command::Run(config));
    let (mut view, mut signal, mut stop) = (View::default(), None, pin!(stop));
    loop {
        tokio::select! {
            Some(event) = received.recv() => {
                if let Event::Measuring(stage) = event {
                    let _ = writeln!(std::io::stderr(), "{}…", report::vocabulary::label(stage));
                }
                view.apply(&event);
                if matches!(event, Event::RunFinished { .. }) {
                    break;
                }
            }
            code = &mut stop, if signal.is_none() => {
                signal = Some(code);
                controller.command(Command::Stop);
            }
            else => break,
        }
    }
    controller.settled().await;
    // The signal's status holds only for a run it stopped.
    let stopped = matches!(view.run.as_ref().and_then(|run| run.outcome), None | Some(Outcome::Stopped));
    let status = status(&view, signal.filter(|_| stopped));
    (view, status)
}

/// The status the process ends with: `signal` when set, 0 when complete or before any run, else 1.
pub fn status(view: &View, signal: Option<u8>) -> u8 {
    match (signal, view.run.as_ref().and_then(|run| run.outcome)) {
        (Some(code), _) => code,
        (None, None | Some(Outcome::Complete)) => 0,
        _ => 1,
    }
}
