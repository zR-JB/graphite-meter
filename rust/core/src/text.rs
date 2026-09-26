pub fn display_character(c: char) -> bool {
    !c.is_control()
        && !matches!(c, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

pub fn terminal_character(c: char) -> bool {
    display_character(c)
        && !matches!(c, '\u{200b}'..='\u{200d}' | '\u{2028}'..='\u{2029}' | '\u{2060}'..='\u{2065}' | '\u{206a}'..='\u{206f}' | '\u{feff}')
}
