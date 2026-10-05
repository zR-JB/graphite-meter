//! The Graphite Meter native terminal client.

use graphite_meter_client::{
    VERSION,
    config::{self, Parsed, Refusal},
};
use graphite_meter_proto::text;
use std::{io, process::ExitCode};

include!(concat!(env!("OUT_DIR"), "/legal.rs"));

/// The status of a refused command line.
const USAGE_ERROR: u8 = 2;

fn main() -> ExitCode {
    NOTICES.keep();
    let mut args = std::env::args_os();
    let program = args.next().unwrap_or_default();
    let usage = || config::usage(&text::clean(&program.to_string_lossy(), usize::MAX, text::safe));
    match config::parse(args) {
        Ok(Parsed::Help) => eprint!("{}", usage()),
        Ok(Parsed::Version) => println!("graphite-meter-client {VERSION}"),
        Ok(Parsed::Legal) => return legal(),
        Ok(Parsed::Run(_)) => {}
        Err(Refusal::Flag(error)) => {
            eprint!("{error}\n{}", usage());
            return ExitCode::from(USAGE_ERROR);
        }
        Err(Refusal::Argument(error)) => {
            eprintln!("graphite-meter-client: {error}");
            return ExitCode::from(USAGE_ERROR);
        }
    }
    ExitCode::SUCCESS
}

/// Prints the notices; the status is 141 into a closed pipe on Unix.
fn legal() -> ExitCode {
    let printed = NOTICES
        .report()
        .ok_or_else(|| {
            "this build embeds no notices; mise run rust-client-run -- --legal builds the TUI with dependency notices \
             and prints them"
                .to_owned()
        })
        .and_then(|report| graphite_meter_legal::print(io::stdout().lock(), report).map_err(|error| error.to_string()));
    match printed {
        Ok(status) => ExitCode::from(status),
        Err(error) => {
            eprintln!("graphite-meter-client: {error}");
            ExitCode::FAILURE
        }
    }
}
