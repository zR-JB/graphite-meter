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
}

pub mod run {
    pub mod coordinator;
    pub mod engine;
    pub mod participant;
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

/// Runs `config` once without the interface, writing stage progress to stderr; `stop` stops it with the status it
/// resolves to. The run's view and the process status.
pub async fn headless(config: Config, runtimes: Arc<Pool>, stop: impl Future<Output = u8>) -> (View, u8) {
    let (events, mut received) = Events::channel();
    let mut controller = Controller::new(false, runtimes, events);
    controller.command(Command::Run(config));
    let (mut view, mut signal, mut stop) = (View::default(), None, pin!(stop));
    loop {
        tokio::select! {
            Some(event) = received.recv() => {
                if let Some(line) = report::progress(&event) {
                    let _ = writeln!(std::io::stderr(), "{line}");
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
    let status = status(&view, signal);
    (view, status)
}

/// The status the process ends with: a signal's once it stopped the run, 0 when complete or before any run, else 1.
pub fn status(view: &View, signal: Option<u8>) -> u8 {
    match (view.run.as_ref().and_then(|run| run.outcome), signal) {
        (None | Some(Outcome::Stopped), Some(code)) => code,
        (None | Some(Outcome::Complete), _) => 0,
        _ => 1,
    }
}
