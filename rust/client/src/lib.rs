pub mod config;
pub mod controller;
pub mod events;
pub mod format;
pub mod model;
pub mod net;
pub mod report;
pub mod style;
pub mod text;

pub mod measure {
    pub mod aggregate;
    pub mod latency;
    pub mod live;
    pub mod wire;
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

pub const VERSION: &str = match option_env!("GM_ENGINE_VERSION") {
    Some(version) => version,
    None => concat!(env!("CARGO_PKG_VERSION"), "-dev"),
};

/// A run stopped by an interrupt or a termination ends with these statuses.
pub const INTERRUPTED: u8 = 130;
pub const TERMINATED: u8 = 143;

/// Runs `config` once without the interface, stage progress on stderr. Returns the view and a status `stop` can set.
pub async fn headless(
    config: Config,
    runtimes: Arc<Pool>,
    stop: impl Future<Output = u8>,
) -> (View, u8) {
    let (events, mut received) = Events::channel();
    let mut controller = Controller::new(false, runtimes, events);
    controller.command(Command::Run(config));
    let (mut view, mut signal, mut stop) = (View::default(), None, pin!(stop));
    loop {
        tokio::select! {
            Some(event) = received.recv() => {
                if let Event::Measuring(stage) = event {
                    let _ = writeln!(std::io::stderr(), "{}…", format::label(stage));
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
    let stopped = matches!(
        view.latest().and_then(|run| run.outcome),
        None | Some(Outcome::Stopped)
    );
    let status = status(&view, signal.filter(|_| stopped));
    (view, status)
}

/// The process ends with `signal` when set, with 0 when complete or before any run, else with 1.
pub fn status(view: &View, signal: Option<u8>) -> u8 {
    match (signal, view.latest().and_then(|run| run.outcome)) {
        (Some(code), _) => code,
        (None, None | Some(Outcome::Complete)) => 0,
        _ => 1,
    }
}
