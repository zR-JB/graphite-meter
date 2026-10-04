use graphite_meter_proto::{
    duration,
    flag::{self, Flag, FlagError, Kind, Parsed, parse_bool},
};
use std::{ffi::OsString, time::Duration};

/// What the flags set: a client's built-in Go values and a server's own values paired with variables.
#[derive(Debug, Clone, PartialEq)]
struct Settings {
    url: String,
    empty: String,
    warmup: Duration,
    zero: Duration,
    auto_streams: i64,
    streams: i64,
    insecure: bool,
    loaded: bool,
    servers: Vec<String>,
    address: String,
    name: String,
    verbose: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:7246".into(),
            empty: String::new(),
            warmup: Duration::from_millis(800),
            zero: Duration::ZERO,
            auto_streams: 6,
            streams: 0,
            insecure: false,
            loaded: true,
            servers: Vec::new(),
            address: ":7246".into(),
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

/// The flags of the Go program the expected usage and messages come from.
const FLAGS: [Flag<Settings>; 12] = [
    Flag {
        name: "url",
        kind: Kind::String,
        usage: "origin of the catalogue",
        env: None,
        set: |s, v| put(&mut s.url, v.into()),
        show: |s| s.url.clone(),
    },
    Flag {
        name: "empty",
        kind: Kind::String,
        usage: "an empty string",
        env: None,
        set: |s, v| put(&mut s.empty, v.into()),
        show: |s| s.empty.clone(),
    },
    Flag {
        name: "warmup",
        kind: Kind::Duration,
        usage: "per-stage warmup duration",
        env: None,
        set: |s, v| put(&mut s.warmup, span(v)?),
        show: |s| duration::format(s.warmup),
    },
    Flag {
        name: "zero-duration",
        kind: Kind::Duration,
        usage: "a zero duration",
        env: None,
        set: |s, v| put(&mut s.zero, span(v)?),
        show: |s| duration::format(s.zero),
    },
    Flag {
        name: "auto-streams",
        kind: Kind::Int,
        usage: "maximum streams",
        env: None,
        set: |s, v| put(&mut s.auto_streams, count(v)?),
        show: |s| s.auto_streams.to_string(),
    },
    Flag {
        name: "streams",
        kind: Kind::Int,
        usage: "forced streams",
        env: None,
        set: |s, v| put(&mut s.streams, count(v)?),
        show: |s| s.streams.to_string(),
    },
    Flag {
        name: "insecure",
        kind: Kind::Bool,
        usage: "skip verification",
        env: None,
        set: |s, v| put(&mut s.insecure, boolean(v)?),
        show: |s| s.insecure.to_string(),
    },
    Flag {
        name: "loaded-latency",
        kind: Kind::Bool,
        usage: "measure under load",
        env: None,
        set: |s, v| put(&mut s.loaded, boolean(v)?),
        show: |s| s.loaded.to_string(),
    },
    Flag {
        name: "server",
        kind: Kind::Value,
        usage: "selected ID (repeat)",
        env: None,
        set: |s, v| {
            s.servers.push(v.into());
            Ok(())
        },
        show: |_| String::new(),
    },
    Flag {
        name: "h1-addr",
        kind: Kind::Value,
        usage: "clear HTTP/1.1 listen `address`",
        env: Some("GM_H1_ADDR"),
        set: |s, v| put(&mut s.address, own(v)?),
        show: |s| s.address.clone(),
    },
    Flag {
        name: "name",
        kind: Kind::Value,
        usage: "server `name` advertised",
        env: Some("GM_SERVER_NAME"),
        set: |s, v| put(&mut s.name, own(v)?),
        show: |s| s.name.clone(),
    },
    Flag {
        name: "verbose",
        kind: Kind::Bool,
        usage: "log throughput",
        env: Some("GM_VERBOSE"),
        set: |s, v| put(&mut s.verbose, own(v)? == "true"),
        show: |s| s.verbose.to_string(),
    },
];

fn run(args: &[&str]) -> (Result<Parsed, FlagError>, Settings) {
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
fn usage_lines_read_as_go_prints_its_defaults() {
    let expected = [
        "  -auto-streams int\n    \tmaximum streams (default 6)\n",
        "  -empty string\n    \tan empty string\n",
        "  -h1-addr address\n    \tclear HTTP/1.1 listen address (env GM_H1_ADDR) (default :7246)\n",
        "  -insecure\n    \tskip verification\n",
        "  -loaded-latency\n    \tmeasure under load (default true)\n",
        "  -name name\n    \tserver name advertised (env GM_SERVER_NAME)\n",
        "  -server value\n    \tselected ID (repeat)\n",
        "  -streams int\n    \tforced streams\n",
        "  -url string\n    \torigin of the catalogue (default \"http://127.0.0.1:7246\")\n",
        "  -verbose\n    \tlog throughput (env GM_VERBOSE) (default true)\n",
        "  -warmup duration\n    \tper-stage warmup duration (default 800ms)\n",
        "  -zero-duration duration\n    \ta zero duration\n",
    ];
    assert_eq!(flag::defaults(&FLAGS, &Settings::default()), expected.concat());
}

#[test]
fn only_the_first_backquoted_word_names_the_value() {
    let odd = Flag::<Settings> {
        name: "odd",
        usage: "one `back quote only",
        env: Some("GM_ODD"),
        ..FLAGS[9]
    };
    let two = Flag::<Settings> {
        name: "two",
        usage: "a `first` and `second` name",
        env: None,
        ..FLAGS[9]
    };
    let lines = flag::defaults(&[odd, two], &Settings::default());
    let expected = [
        "  -odd value\n    \tone `back quote only (env GM_ODD) (default :7246)\n",
        "  -two first\n    \ta first and `second` name (default :7246)\n",
    ];
    assert_eq!(lines, expected.concat());
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
    let (parsed, settings) = run(&["-insecure=false", "--loaded-latency=0"]);
    assert_eq!((parsed, settings.insecure, settings.loaded), (Ok(rest(&[])), false, false));
}

#[test]
fn parsing_stops_at_the_first_argument_that_is_no_flag_or_after_two_dashes() {
    assert_eq!(run(&["--", "-insecure"]), (Ok(rest(&["-insecure"])), Settings::default()));
    assert_eq!(run(&["-", "-insecure"]).0, Ok(rest(&["-", "-insecure"])));
    assert_eq!(run(&["x", "-insecure"]).0, Ok(rest(&["x", "-insecure"])));
    let (parsed, settings) = run(&["-insecure", "--url=a", "rest", "-x"]);
    assert_eq!((parsed, settings.insecure, settings.url.as_str()), (Ok(rest(&["rest", "-x"])), true, "a"));
}

#[test]
fn help_asks_for_the_usage() {
    for args in [&["-h"][..], &["--help"], &["-help=1"], &["-insecure", "-h", "-nope"]] {
        assert_eq!(run(args).0, Ok(Parsed::Help), "{args:?}");
    }
}

#[test]
fn refused_command_lines_carry_go_messages() {
    for (args, message) in [
        (&["-nope"][..], "flag provided but not defined: -nope"),
        (&["--nope=1"], "flag provided but not defined: -nope"),
        (&["---x"], "bad flag syntax: ---x"),
        (&["-=x"], "bad flag syntax: -=x"),
        (&["--=x"], "bad flag syntax: --=x"),
        (&["-url"], "flag needs an argument: -url"),
        (&["-insecure=maybe"], "invalid boolean value \"maybe\" for -insecure: parse error"),
        (&["-verbose=bad"], "invalid boolean value \"bad\" for -verbose: must not be bad"),
        (&["-name=bad"], "invalid value \"bad\" for flag -name: must not be bad"),
        (&["-name", "bad"], "invalid value \"bad\" for flag -name: must not be bad"),
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

#[cfg(unix)]
#[test]
fn arguments_after_the_flags_need_not_be_utf8() {
    use std::os::unix::ffi::OsStringExt;
    let raw = OsString::from_vec(vec![b'x', 0xff]);
    let mut settings = Settings::default();
    let parsed = flag::parse(&FLAGS, &mut settings, [OsString::from("-insecure"), raw.clone()]);
    assert_eq!(parsed, Ok(Parsed::Arguments(vec![raw])));
    let flag = OsString::from_vec(vec![b'-', 0xff]);
    assert!(flag::parse(&FLAGS, &mut settings, [flag]).is_err());
}

#[test]
fn booleans_read_as_go_reads_them() {
    for text in ["1", "t", "T", "true", "TRUE", "True"] {
        assert_eq!(parse_bool(text), Some(true), "{text}");
    }
    for text in ["0", "f", "F", "false", "FALSE", "False"] {
        assert_eq!(parse_bool(text), Some(false), "{text}");
    }
    for text in ["", "yes", "tRUE", " true", "2"] {
        assert_eq!(parse_bool(text), None, "{text}");
    }
}
