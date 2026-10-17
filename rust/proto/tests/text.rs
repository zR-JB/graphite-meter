use graphite_meter_proto::text::{clean, quote, safe};

#[test]
fn safe_text_has_no_controls_or_bidi_formatting() {
    for c in ['a', 'é', ' ', '\u{a0}', '\u{200b}', '😀', '\u{fffd}'] {
        assert!(safe(c), "{c:?}");
    }
    for c in [
        '\0', '\t', '\n', '\u{7f}', '\u{85}', '\u{61c}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202e}', '\u{2066}',
        '\u{2069}',
    ] {
        assert!(!safe(c), "{c:?}");
    }
}

#[test]
fn cleaning_blanks_refused_characters_and_ends_long_text_in_an_ellipsis() {
    assert_eq!(clean("a\u{202e}b\nc", 10), "a b c");
    assert_eq!(clean("a\u{200b}b", 10), "a\u{200b}b");
    assert_eq!(clean("abcdef", 6), "abcdef");
    assert_eq!(clean("abcdefg", 6), "abcde…");
    assert_eq!(clean("ééééééé", 3), "éé…", "the limit counts characters");
    assert_eq!(clean("ab", 0), "…");
}

#[test]
fn quoting_escapes_controls_bidi_formatting_quotes_and_backslashes() {
    for (text, quoted) in [
        ("plain 日本 é", r#""plain 日本 é""#),
        ("a\"b\\c", r#""a\"b\\c""#),
        ("\t\n\r\0\u{1}\u{7f}\u{85}", r#""\t\n\r\0\u{1}\u{7f}\u{85}""#),
        ("\u{61c}\u{200e}\u{202e}\u{2066}", r#""\u{61c}\u{200e}\u{202e}\u{2066}""#),
    ] {
        assert_eq!(quote(text), quoted, "{text:?}");
    }
    assert!(
        quote("\u{200b}\u{feff}").chars().all(|c| c.is_ascii_graphic()),
        "invisible characters escape too"
    );
}
