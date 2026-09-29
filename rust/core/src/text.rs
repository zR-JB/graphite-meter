pub fn display_character(c: char) -> bool {
    !c.is_control()
        && !matches!(c, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Server metadata as Go's catalogues and preflights bound it: at most 256 bytes that SafeText accepts.
pub fn label(text: &str) -> bool {
    text.len() <= 256 && text.chars().all(display_character)
}

pub fn terminal_character(c: char) -> bool {
    display_character(c)
        && !matches!(c, '\u{200b}'..='\u{200d}' | '\u{2028}'..='\u{2029}' | '\u{2060}'..='\u{2065}' | '\u{206a}'..='\u{206f}' | '\u{feff}')
}

/// Go's `wire.CleanText`: blanks controls and bidi overrides and keeps at most
/// `limit` characters, ending a longer text with an ellipsis.
pub fn clean(text: &str, limit: usize) -> String {
    clean_with(text, limit, display_character)
}

/// `clean`, keeping the characters `keep` accepts.
pub fn clean_with(text: &str, limit: usize, keep: fn(char) -> bool) -> String {
    let mut characters = text.chars().map(|c| if keep(c) { c } else { ' ' });
    let cleaned: String = characters.by_ref().take(limit).collect();
    if characters.next().is_none() {
        return cleaned;
    }
    cleaned.chars().take(limit.saturating_sub(1)).chain(['…']).collect()
}
