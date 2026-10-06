use graphite_meter_proto::{
    duration,
    flag::{self, Flag, Kind, Parsed, parse_bool},
};
use std::{ffi::OsString, time::Duration};

/// What the flags set, from Go-like defaults.
#[derive(Debug, Clone, PartialEq)]
struct Settings {
    url: String,
    warmup: Duration,
    auto_streams: i64,
    insecure: bool,
    servers: Vec<String>,
    name: String,
    verbose: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:7246".into(),
            warmup: Duration::from_millis(800),
            auto_streams: 6,
            insecure: false,
            servers: Vec::new(),
            name: String::new(),
            verbose: true,
        }
    }
}

fn boolean(value: &str) -> Result<bool, String> {
    parse_bool(value).ok_or_else(|| "parse error".into())
}

fn span(value: &str) -> Result<Duration, String> {
    let nanos = duration::parse(value).map_err(|_| "parse error")?;
    u64::try_from(nanos)
        .map(Duration::from_nanos)
        .map_err(|_| "negative".into())
}

fn count(value: &str) -> Result<i64, String> {
    value.parse().map_err(|_| "parse error".into())
}

fn own(value: &str) -> Result<String, String> {
    if value.starts_with("bad") {
        Err("must not be bad".into())
    } else {
        Ok(value.into())
    }
}

fn put<T>(slot: &mut T, value: T) -> Result<(), String> {
    *slot = value;
    Ok(())
}

/// A flag whose usage line these tests never print.
const fn flag(name: &'static str, kind: Kind, set: fn(&mut Settings, &str) -> Result<(), String>) -> Flag<Settings> {
    Flag {
        name,
        kind,
        usage: "",
        env: None,
        set,
        show: |_| String::new(),
    }
}

const FLAGS: [Flag<Settings>; 7] = [
    flag("url", Kind::String, |s, v| put(&mut s.url, v.into())),
    flag("warmup", Kind::Duration, |s, v| put(&mut s.warmup, span(v)?)),
    flag("auto-streams", Kind::Int, |s, v| put(&mut s.auto_streams, count(v)?)),
    flag("insecure", Kind::Bool, |s, v| put(&mut s.insecure, boolean(v)?)),
    flag("server", Kind::Value, |s, v| {
        s.servers.push(v.into());
        Ok(())
    }),
    flag("name", Kind::Value, |s, v| put(&mut s.name, own(v)?)),
    flag("verbose", Kind::Bool, |s, v| put(&mut s.verbose, own(v)? == "true")),
];

fn run(args: &[&str]) -> (Result<Parsed, String>, Settings) {
    let mut settings = Settings::default();
    let parsed = flag::parse(&FLAGS, &mut settings, args.iter().map(OsString::from));
    (parsed, settings)
}

fn rest(args: &[&str]) -> Parsed {
    Parsed::Arguments(args.iter().map(OsString::from).collect())
}

fn refusal(args: &[&str]) -> String {
    run(args).0.expect_err("a refused command line").to_string()
}

#[test]
fn flags_take_one_or_two_dashes_and_values_inline_or_next() {
    let (parsed, settings) =
        run(&["-url", "-insecure", "--auto-streams=3", "-warmup", "1s", "--server", "a", "-server=b"]);
    assert_eq!(parsed, Ok(rest(&[])));
    let expected = Settings {
        url: "-insecure".into(),
        auto_streams: 3,
        warmup: Duration::from_secs(1),
        servers: vec!["a".into(), "b".into()],
        ..Settings::default()
    };
    assert_eq!(settings, expected);
    let (_, settings) = run(&["-url=", "-name="]);
    assert_eq!(
        (settings.url.as_str(), settings.name.as_str()),
        ("", ""),
        "an empty inline value is a value"
    );
}

#[test]
fn booleans_take_a_value_only_inline() {
    let (parsed, settings) = run(&["-insecure", "false"]);
    assert_eq!((parsed, settings.insecure), (Ok(rest(&["false"])), true));
    let (parsed, settings) = run(&["-insecure", "--insecure=0"]);
    assert_eq!((parsed, settings.insecure), (Ok(rest(&[])), false));
    for (text, value) in [("T", Some(true)), ("FALSE", Some(false)), ("tRUE", None), ("yes", None)] {
        assert_eq!(parse_bool(text), value, "{text}");
    }
}

#[test]
fn parsing_stops_at_the_first_argument_that_is_no_flag_or_after_two_dashes() {
    assert_eq!(run(&["--", "-insecure"]), (Ok(rest(&["-insecure"])), Settings::default()));
    assert_eq!(run(&["-", "-insecure"]).0, Ok(rest(&["-", "-insecure"])));
    assert_eq!(run(&["x", "-insecure"]).0, Ok(rest(&["x", "-insecure"])));
    let (parsed, settings) = run(&["-insecure", "--url=a", "rest", "-x"]);
    assert_eq!((parsed, settings.insecure, settings.url.as_str()), (Ok(rest(&["rest", "-x"])), true, "a"));
    assert_eq!(run(&["-insecure", "-h", "-nope"]).0, Ok(Parsed::Help));
}

#[test]
fn refused_command_lines_carry_go_messages() {
    for (args, message) in [
        (&["-nope"][..], "flag provided but not defined: -nope"),
        (&["--nope=1"], "flag provided but not defined: -nope"),
        (&["---x"], "bad flag syntax: ---x"),
        (&["-url"], "flag needs an argument: -url"),
        (&["-insecure=maybe"], "invalid boolean value \"maybe\" for -insecure: parse error"),
        (&["-verbose=bad"], "invalid boolean value \"bad\" for -verbose: must not be bad"),
        (&["-name=bad"], "invalid value \"bad\" for flag -name: must not be bad"),
        (&["-auto-streams=x"], "invalid value \"x\" for flag -auto-streams: parse error"),
        (&["-warmup=1d"], "invalid value \"1d\" for flag -warmup: parse error"),
    ] {
        assert_eq!(refusal(args), message, "{args:?}");
    }
    assert_eq!(
        refusal(&["-name", "a\tb", "-name=bad\n"]),
        "invalid value \"bad\\n\" for flag -name: must not be bad"
    );
}
