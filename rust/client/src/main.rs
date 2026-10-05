//! The Graphite Meter native terminal client.

use std::{io, process::ExitCode};

include!(concat!(env!("OUT_DIR"), "/legal.rs"));

fn main() -> ExitCode {
    NOTICES.keep();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [only] if only == "--legal" || only == "-legal" => match legal() {
            Ok(status) => ExitCode::from(status),
            Err(error) => {
                eprintln!("graphite-meter-client: {error}");
                ExitCode::FAILURE
            }
        },
        _ => ExitCode::SUCCESS,
    }
}

/// Prints the notices; the status is 141 into a closed pipe on Unix.
fn legal() -> Result<u8, String> {
    let report = NOTICES.report().ok_or(
        "this build embeds no notices; mise run rust-client-run -- --legal builds the TUI with dependency notices and \
         prints them",
    )?;
    graphite_meter_legal::print(io::stdout().lock(), report).map_err(|error| error.to_string())
}
