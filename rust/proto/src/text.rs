//! Text that reaches terminals and logs: safe characters, cleaning and quoting; and decimal fields.

use std::str::FromStr;

/// A field of digits only, at most `digits` of them, that fits `T`.
pub(crate) fn decimal<T: FromStr>(field: &str, digits: usize) -> Option<T> {
    let digits_only = !field.is_empty() && field.len() <= digits && field.bytes().all(|byte| byte.is_ascii_digit());
    digits_only.then(|| field.parse().ok()).flatten()
}

/// Whether `c` reaches a terminal as itself: no control or bidirectional formatting character.
pub fn safe(c: char) -> bool {
    !c.is_control()
        && !matches!(c, '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// At most `limit` characters of `text`, unsafe ones blanked, longer ones ellipsized.
pub fn clean(text: &str, limit: usize) -> String {
    let blanked = text.chars().map(|c| if safe(c) { c } else { ' ' });
    match text.chars().count() <= limit {
        true => blanked.collect(),
        false => blanked.take(limit.saturating_sub(1)).chain(['…']).collect(),
    }
}

/// `text` in double quotes with Rust's debug escapes, and every unsafe character escaped.
pub fn quote(text: &str) -> String {
    let mut quoted = String::from('"');
    for c in text.chars() {
        match safe(c) || c.is_control() {
            true => quoted.extend(c.escape_debug()),
            false => quoted.extend(c.escape_unicode()),
        }
    }
    quoted.push('"');
    quoted
}
