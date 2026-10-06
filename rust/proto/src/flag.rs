//! Go's `flag` syntax and `PrintDefaults` usage lines, driven by one table per binary.

use crate::text::quote;
use std::{ffi::OsString, iter};

table! {
    /// How a flag takes its value, and the type its usage line names.
    pub enum Kind {
        /// The type name a usage line shows when its text quotes none.
        type_name.0: &'static str,
        /// The text of the type's zero value.
        zero.1: &'static str,
    } {
        /// A switch: its name alone sets it, a value only as `-name=value`.
        Bool => ("", "false"),
        Int => ("int", "0"),
        Duration => ("duration", "0s"),
        String => ("string", ""),
        /// The binary's own value type, as Go's `fs.Var`: usage names `value` and prints the default unquoted.
        Value => ("value", ""),
    }
}

/// One row of a binary's flag table.
pub struct Flag<T> {
    /// Empty for a row only the environment sets.
    pub name: &'static str,
    pub kind: Kind,
    /// What the flag sets; a word in backquotes names its value in the usage line.
    pub usage: &'static str,
    /// The environment variable that sets the same value.
    pub env: Option<&'static str>,
    /// Applies a value, or tells why it is refused.
    pub set: fn(&mut T, &str) -> Result<(), String>,
    /// The value as text, as Go's `flag.Value.String`.
    pub show: fn(&T) -> String,
}

/// What follows the flags on a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// `-h` or `-help` asked for the usage.
    Help,
    Arguments(Vec<OsString>),
}

/// Applies leading `args` flags to `target` as Go's `flag.Parse`: `-name` or `--name`, until a non-flag or `--`.
pub fn parse<T>(flags: &[Flag<T>], target: &mut T, args: impl IntoIterator<Item = OsString>) -> Result<Parsed, String> {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_encoded_bytes() {
            b"--" => return Ok(Parsed::Arguments(args.collect())),
            [b'-', _, ..] => {}
            _ => return Ok(Parsed::Arguments(iter::once(arg).chain(args).collect())),
        }
        let arg = utf8(arg)?;
        let name = arg[1..].strip_prefix('-').unwrap_or(&arg[1..]);
        if name.starts_with(['-', '=']) {
            return Err(format!("bad flag syntax: {arg}"));
        }
        let (name, inline) = match name.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (name, None),
        };
        let Some(flag) = flags.iter().find(|flag| flag.name == name) else {
            return match name {
                "h" | "help" => Ok(Parsed::Help),
                _ => Err(format!("flag provided but not defined: -{name}")),
            };
        };
        if flag.kind == Kind::Bool {
            let value = inline.unwrap_or("true");
            let refused = |reason| format!("invalid boolean value {} for -{name}: {reason}", quote(value));
            (flag.set)(target, value).map_err(refused)?;
            continue;
        }
        let value = match inline {
            Some(value) => value.to_owned(),
            None => {
                let next = args.next().ok_or_else(|| format!("flag needs an argument: -{name}"))?;
                utf8(next)?
            }
        };
        let refused = |reason| format!("invalid value {} for flag -{name}: {reason}", quote(&value));
        (flag.set)(target, &value).map_err(refused)?;
    }
    Ok(Parsed::Arguments(Vec::new()))
}

fn utf8(arg: OsString) -> Result<String, String> {
    arg.into_string()
        .map_err(|arg| format!("argument {} is not valid UTF-8", quote(&arg.to_string_lossy())))
}

/// Go's `PrintDefaults` lines for named `flags` by name, each with its environment variable and non-zero default.
pub fn defaults<T>(flags: &[Flag<T>], defaults: &T) -> String {
    let mut sorted: Vec<&Flag<T>> = flags.iter().filter(|flag| !flag.name.is_empty()).collect();
    sorted.sort_by_key(|flag| flag.name);
    let mut lines = String::new();
    for flag in sorted {
        let (type_name, zero) = flag.kind.row();
        let (value_name, usage) = unquote(flag.usage).unwrap_or((type_name, flag.usage.into()));
        let value_name = match value_name {
            "" => String::new(),
            name => format!(" {name}"),
        };
        let env = flag.env.map_or(String::new(), |env| format!(" (env {env})"));
        let default = match (flag.show)(defaults) {
            default if default == zero => String::new(),
            default if flag.kind == Kind::String => format!(" (default {})", quote(&default)),
            default => format!(" (default {default})"),
        };
        lines.push_str(&format!("  -{}{value_name}\n    \t{usage}{env}{default}\n", flag.name));
    }
    lines
}

/// The first word of `usage` in backquotes, and `usage` without those backquotes.
fn unquote(usage: &str) -> Option<(&str, String)> {
    let (before, rest) = usage.split_once('`')?;
    let (name, after) = rest.split_once('`')?;
    Some((name, format!("{before}{name}{after}")))
}

/// Go's `strconv.ParseBool`.
pub fn parse_bool(text: &str) -> Option<bool> {
    match text {
        "1" | "t" | "T" | "true" | "TRUE" | "True" => Some(true),
        "0" | "f" | "F" | "false" | "FALSE" | "False" => Some(false),
        _ => None,
    }
}
