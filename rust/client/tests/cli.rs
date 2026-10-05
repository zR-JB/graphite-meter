use std::process::{Command, Output};

const CLIENT: &str = env!("CARGO_BIN_EXE_graphite-meter-client");

fn client(args: &[&str]) -> Output {
    Command::new(CLIENT).args(args).output().unwrap()
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap()
}

/// Go's usage, captured from its client, as this binary names itself.
fn usage() -> String {
    let golden = include_str!("usage.txt");
    golden.replacen("graphite-meter-client", CLIENT, 1)
}

#[test]
fn help_prints_go_usage_to_stderr() {
    for arg in ["-h", "-help", "--help"] {
        let output = client(&[arg]);
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stdout.is_empty());
        assert_eq!(text(output.stderr), usage());
    }
}

#[test]
fn a_flag_error_prints_go_message_and_usage() {
    let cases: [(&[&str], &str); 4] = [
        (&["-bogus"], "flag provided but not defined: -bogus"),
        (&["-streams", "x"], "invalid value \"x\" for flag -streams: parse error"),
        (&["-insecure=maybe"], "invalid boolean value \"maybe\" for -insecure: parse error"),
        (
            &["-server", "a", "-server", "a"],
            "invalid value \"a\" for flag -server: select one to 4 different server IDs",
        ),
    ];
    for (args, message) in cases {
        let output = client(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(text(output.stderr), format!("{message}\n{}", usage()));
    }
}

#[test]
fn another_argument_error_names_the_client() {
    let cases: [(&[&str], &str); 3] = [
        (&["-report", "extra"], "unexpected argument \"extra\""),
        (
            &["-streams", "15"],
            "forced streams must be from 1 to 14 per server and direction, or 0 for automatic",
        ),
        (
            &["-throughput-protocol", "h9"],
            "invalid throughput protocol \"h9\": use auto, http1, http2, or http3",
        ),
    ];
    for (args, message) in cases {
        let output = client(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(text(output.stderr), format!("graphite-meter-client: {message}\n"));
    }
}

#[test]
fn version_acts_once_every_flag_parsed() {
    for args in [&["--version"][..], &["-version", "-streams", "99", "extra"]] {
        let output = client(args);
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        assert_eq!(text(output.stdout), format!("graphite-meter-client {}\n", graphite_meter_client::VERSION));
    }
}

#[test]
fn legal_without_embedded_notices_names_the_task_that_embeds_them() {
    for args in [&["-legal"][..], &["--legal"], &["-streams", "99", "-legal"]] {
        let output = client(args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let expected = "graphite-meter-client: this build embeds no notices; mise run rust-client-run -- --legal \
                        builds the TUI with dependency notices and prints them\n";
        assert_eq!(text(output.stderr), expected);
    }
}

#[cfg(unix)]
#[test]
fn version_into_a_closed_pipe_exits_141_quietly() {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let output = Command::new(CLIENT).arg("--version").stdout(writer).output().unwrap();
    assert_eq!((output.status.code(), text(output.stderr)), (Some(141), String::new()));
}
