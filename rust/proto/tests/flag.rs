use graphite_meter_proto::{
    duration,
    flag::{self, Flag, Parsed, parse_bool},
};
use std::{ffi::OsString, time::Duration};

#[derive(Debug, Clone, PartialEq)]
struct Settings {
    url: String,
    warmup: Duration,
    insecure: bool,
    servers: Vec<String>,
    name: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:7246".into(),
            warmup: Duration::from_millis(800),
            insecure: false,
            servers: Vec::new(),
            name: String::new(),
        }
    }
}

fn put<T>(slot: &mut T, value: T) -> Result<(), String> {
    *slot = value;
    Ok(())
}

const fn flag(
    name: &'static str,
    value: &'static str,
    usage: &'static str,
    set: fn(&mut Settings, &str) -> Result<(), String>,
    show: fn(&Settings) -> String,
) -> Flag<Settings> {
    Flag {
        name,
        value,
        usage,
        env: None,
        set,
        show,
    }
}

const FLAGS: [Flag<Settings>; 6] = [
    flag(
        "url",
        "url",
        "Server address",
        |s, v| put(&mut s.url, v.into()),
        |s| s.url.clone(),
    ),
    flag(
        "warmup",
        "duration",
        "Warmup per stage",
        |s, v| put(&mut s.warmup, duration::parse(v)?),
        |s| duration::format(s.warmup),
    ),
    flag(
        "insecure",
        "",
        "Skip verification",
        |s, v| put(&mut s.insecure, parse_bool(v)?),
        |s| s.insecure.to_string(),
    ),
    flag(
        "server",
        "id",
        "Selected server",
        |s, v| {
            s.servers.push(v.into());
            Ok(())
        },
        |_| String::new(),
    ),
    flag(
        "name",
        "name",
        "Name",
        |s, v| match v.starts_with("bad") {
            true => Err("must not be bad".into()),
            false => put(&mut s.name, v.into()),
        },
        |_| String::new(),
    ),
    Flag {
        name: "",
        value: "",
        usage: "",
        env: Some("SECRET"),
        set: |_, _| Ok(()),
        show: |_| String::new(),
    },
];

fn run(args: &[&str]) -> (Result<Parsed, String>, Settings) {
    let mut settings = Settings::default();
    let parsed = flag::parse(&FLAGS, &mut settings, args.iter().map(OsString::from));
    (parsed, settings)
}

fn rest(args: &[&str]) -> Parsed {
    Parsed::Arguments(args.iter().map(OsString::from).collect())
}

#[test]
fn options_take_one_or_two_dashes_and_values_inline_or_next() {
    let (parsed, settings) = run(&[
        "-url",
        "-insecure",
        "-warmup",
        "1s",
        "--server",
        "a",
        "-server=b",
        "--insecure",
    ]);
    assert_eq!(parsed, Ok(rest(&[])));
    let expected = Settings {
        url: "-insecure".into(),
        warmup: Duration::from_secs(1),
        servers: vec!["a".into(), "b".into()],
        insecure: true,
        ..Settings::default()
    };
    assert_eq!(settings, expected);
    let (_, settings) = run(&["--url=", "-name="]);
    assert_eq!(
        (settings.url.as_str(), settings.name.as_str()),
        ("", ""),
        "an empty inline value is a value"
    );
}

#[test]
fn switches_take_a_value_only_inline() {
    let (parsed, settings) = run(&["--insecure", "false"]);
    assert_eq!((parsed, settings.insecure), (Ok(rest(&["false"])), true));
    let (parsed, settings) = run(&["--insecure", "--insecure=FALSE"]);
    assert_eq!((parsed, settings.insecure), (Ok(rest(&[])), false));
    for (text, value) in [
        ("True", Ok(true)),
        ("1", Ok(true)),
        ("0", Ok(false)),
        ("t", Err("use true or false")),
    ] {
        assert_eq!(parse_bool(text), value, "{text}");
    }
}

#[test]
fn parsing_stops_at_the_first_argument_that_is_no_option_or_after_two_dashes() {
    assert_eq!(
        run(&["--", "-insecure"]),
        (Ok(rest(&["-insecure"])), Settings::default())
    );
    assert_eq!(run(&["-", "-insecure"]).0, Ok(rest(&["-", "-insecure"])));
    let (parsed, settings) = run(&["-insecure", "--url=a", "rest", "-x"]);
    assert_eq!(
        (parsed, settings.insecure, settings.url.as_str()),
        (Ok(rest(&["rest", "-x"])), true, "a")
    );
    assert_eq!(run(&["-insecure", "--help", "-nope"]).0, Ok(Parsed::Help));
}

#[test]
fn refused_command_lines_name_the_option() {
    for (args, message) in [
        (&["-nope"][..], "unknown option --nope"),
        (&["--nope=1"], "unknown option --nope"),
        (&["---x"], "unknown option ---x"),
        (&["--=x"], "unknown option --"),
        (&["--url"], "option --url needs a value"),
        (
            &["--insecure=maybe"],
            "invalid value \"maybe\" for --insecure: use true or false",
        ),
        (
            &["--name", "bad\n"],
            "invalid value \"bad\\n\" for --name: must not be bad",
        ),
        (
            &["-warmup=-1s"],
            "invalid value \"-1s\" for --warmup: a duration cannot be negative",
        ),
        (&["--\u{202e}"], "unknown option -- "),
    ] {
        assert_eq!(run(args).0, Err(message.into()), "{args:?}");
    }
}

#[test]
fn usage_lists_named_options_in_table_order_with_set_defaults() {
    let expected = "  --url <url>          Server address (default: http://127.0.0.1:7246)
  --warmup <duration>  Warmup per stage (default: 800ms)
  --insecure           Skip verification
  --server <id>        Selected server
  --name <name>        Name
";
    assert_eq!(flag::options(&FLAGS, &Settings::default()), expected);
    let on = Settings {
        insecure: true,
        ..Settings::default()
    };
    assert!(flag::options(&FLAGS, &on).contains("Skip verification (default: true)"));
}
