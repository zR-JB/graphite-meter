use std::process::{Command, Output};

const CLIENT: &str = env!("CARGO_BIN_EXE_graphite-meter-client");

fn client(args: &[&str]) -> Output {
    Command::new(CLIENT).args(args).output().unwrap()
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap()
}

fn usage() -> String {
    let golden = include_str!("usage.txt");
    golden.replacen("graphite-meter-client", CLIENT, 1)
}

#[test]
fn help_prints_the_usage_to_stderr() {
    for arg in ["-h", "-help", "--help"] {
        let output = client(&[arg]);
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stdout.is_empty());
        assert_eq!(text(output.stderr), usage());
    }
}

#[test]
fn a_flag_error_prints_its_message_and_the_usage() {
    let cases: [(&[&str], &str); 5] = [
        (&["-bogus"], "unknown option --bogus"),
        (
            &["--streams", "x"],
            "invalid value \"x\" for --streams: use a whole number",
        ),
        (
            &["--streams", "-1"],
            "invalid value \"-1\" for --streams: use a whole number",
        ),
        (
            &["--throughput-protocol", "h9"],
            "invalid value \"h9\" for --throughput-protocol: use auto, http1, http2 or http3",
        ),
        (
            &["-insecure=maybe"],
            "invalid value \"maybe\" for --insecure: use true or false",
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
fn legal_without_embedded_notices_names_the_task_that_embeds_them() {
    for args in [&["-legal"][..], &["--legal"], &["-streams", "99", "-legal"]] {
        let output = client(args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let expected = "graphite-meter-client: this build embeds no notices; mise run tui -- --legal \
                        builds the client with dependency notices and prints them\n";
        assert_eq!(text(output.stderr), expected);
    }
}

#[cfg(unix)]
#[test]
fn version_into_a_closed_pipe_exits_141_quietly() {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let output = Command::new(CLIENT)
        .arg("--version")
        .stdout(writer)
        .output()
        .unwrap();
    assert_eq!(
        (output.status.code(), text(output.stderr)),
        (Some(141), String::new())
    );
}

#[test]
fn a_report_without_a_server_asks_for_one() {
    let scratch = graphite_meter_testkit::Scratch::new().unwrap();
    let mut command = Command::new(CLIENT);
    for home in ["XDG_CONFIG_HOME", "HOME", "APPDATA"] {
        command.env(home, scratch.path());
    }
    let output = command.arg("-report").output().unwrap();
    let expected = "graphite-meter-client: no server address: pass --url with your Graphite Meter server, for example \
                    https://meter.example\n";
    assert_eq!(
        (output.status.code(), text(output.stderr)),
        (Some(2), expected.into())
    );
}
