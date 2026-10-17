//! Command-line options from one table per binary: `--name value`, `--name=value` or `-name`, and their usage.

use crate::text::{clean, quote};
use std::{ffi::OsString, iter};

/// One row of a binary's option table.
pub struct Flag<T> {
    /// Empty for a row only the environment sets.
    pub name: &'static str,
    /// The value's placeholder in usage; empty for a switch, which its name alone turns on.
    pub value: &'static str,
    pub usage: &'static str,
    /// The environment variable that sets the same value.
    pub env: Option<&'static str>,
    /// Applies a value, or tells why it is refused.
    pub set: fn(&mut T, &str) -> Result<(), String>,
    /// The value as usage shows a default; empty shows none.
    pub show: fn(&T) -> String,
}

/// What follows the options on a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// `-h` or `--help` asked for the usage.
    Help,
    Arguments(Vec<OsString>),
}

/// Applies leading options in `args` to `target`, until an argument that is no option or `--`.
pub fn parse<T>(flags: &[Flag<T>], target: &mut T, args: impl IntoIterator<Item = OsString>) -> Result<Parsed, String> {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_encoded_bytes() {
            b"--" => return Ok(Parsed::Arguments(args.collect())),
            [b'-', _, ..] => {}
            _ => return Ok(Parsed::Arguments(iter::once(arg).chain(args).collect())),
        }
        let arg = utf8(arg)?;
        let option = arg.strip_prefix("--").unwrap_or(&arg[1..]);
        let (name, inline) = option
            .split_once('=')
            .map_or((option, None), |(name, value)| (name, Some(value)));
        let Some(flag) = flags.iter().find(|flag| !flag.name.is_empty() && flag.name == name) else {
            return match name {
                "h" | "help" => Ok(Parsed::Help),
                _ => Err(format!("unknown option --{}", clean(name, 64))),
            };
        };
        let value = match (inline, flag.value) {
            (Some(value), _) => value.to_owned(),
            (None, "") => "true".into(),
            (None, _) => utf8(args.next().ok_or_else(|| format!("option --{name} needs a value"))?)?,
        };
        (flag.set)(target, &value)
            .map_err(|reason| format!("invalid value {} for --{name}: {reason}", quote(&value)))?;
    }
    Ok(Parsed::Arguments(Vec::new()))
}

fn utf8(arg: OsString) -> Result<String, String> {
    arg.into_string()
        .map_err(|arg| format!("argument {} is not valid UTF-8", quote(&arg.to_string_lossy())))
}

/// One usage line per named row in table order: the option, its description, variable and default.
pub fn options<T>(flags: &[Flag<T>], defaults: &T) -> String {
    let named = || flags.iter().filter(|flag| !flag.name.is_empty());
    let spelled = |flag: &Flag<T>| match flag.value {
        "" => format!("--{}", flag.name),
        value => format!("--{} <{value}>", flag.name),
    };
    let width = named().map(|flag| spelled(flag).len()).max().unwrap_or_default();
    let mut lines = String::new();
    for flag in named() {
        lines += &format!("  {:width$}  {}", spelled(flag), flag.usage);
        if let Some(env) = flag.env {
            lines += &format!(" (env: {env})");
        }
        match (flag.show)(defaults) {
            default if default.is_empty() || flag.value.is_empty() && default == "false" => {}
            default => lines += &format!(" (default: {default})"),
        }
        lines.push('\n');
    }
    lines
}

/// `true`, `false`, `1` or `0`, in any case.
pub fn parse_bool(text: &str) -> Result<bool, &'static str> {
    match text {
        "1" => Ok(true),
        "0" => Ok(false),
        _ if text.eq_ignore_ascii_case("true") => Ok(true),
        _ if text.eq_ignore_ascii_case("false") => Ok(false),
        _ => Err("use true or false"),
    }
}
