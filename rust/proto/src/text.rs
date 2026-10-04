//! Text that reaches terminals and logs: safe characters, cleaning and Go's quoting.

use std::fmt::Write as _;

/// Whether `c` reaches a terminal as itself: no control or bidirectional formatting character, as Go's `SafeText`.
pub fn safe(c: char) -> bool {
    !c.is_control()
        && !matches!(c, '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Whether `c` is safe and shows: no invisible formatting character or line and paragraph separator either.
pub fn visible(c: char) -> bool {
    safe(c)
        && !matches!(c,
            '\u{ad}' | '\u{600}'..='\u{605}' | '\u{6dd}' | '\u{70f}' | '\u{890}'..='\u{891}' | '\u{8e2}' | '\u{180e}'
            | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}' | '\u{110bd}' | '\u{110cd}' | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}' | '\u{1d173}'..='\u{1d17a}' | '\u{e0001}' | '\u{e0020}'..='\u{e007f}')
}

/// At most `limit` characters of `text` with each that `keep` refuses blanked; a longer text ends in an ellipsis,
/// as Go's `CleanText`.
pub fn clean(text: &str, limit: usize, keep: fn(char) -> bool) -> String {
    let blanked = text.chars().map(|c| if keep(c) { c } else { ' ' });
    match text.chars().count() <= limit {
        true => blanked.collect(),
        false => blanked.take(limit.saturating_sub(1)).chain(['…']).collect(),
    }
}

/// `text` in double quotes with Go's `%q` escapes; unassigned code points print as themselves.
pub fn quote(text: &str) -> String {
    let mut quoted = String::from('"');
    for c in text.chars() {
        let escape = match c {
            '"' | '\\' => Some(c),
            '\u{7}' => Some('a'),
            '\u{8}' => Some('b'),
            '\u{c}' => Some('f'),
            '\n' => Some('n'),
            '\r' => Some('r'),
            '\t' => Some('t'),
            '\u{b}' => Some('v'),
            _ => None,
        };
        let code = u32::from(c);
        let _ = match escape {
            Some(escape) => write!(quoted, "\\{escape}"),
            None if printable(c) => write!(quoted, "{c}"),
            None if code < 0x80 => write!(quoted, "\\x{code:02x}"),
            None if code <= 0xffff => write!(quoted, "\\u{code:04x}"),
            None => write!(quoted, "\\U{code:08x}"),
        };
    }
    quoted.push('"');
    quoted
}

/// Go's `unicode.IsPrint`, but for unassigned code points.
fn printable(c: char) -> bool {
    let private = matches!(c, '\u{e000}'..='\u{f8ff}' | '\u{f0000}'..);
    c == ' ' || visible(c) && !c.is_whitespace() && !private
}
