use graphite_meter_proto::text::{clean, quote, safe, visible};

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
fn visible_text_also_has_no_invisible_formatting() {
    for c in ['a', '\u{a0}', '😀', '\u{e000}'] {
        assert!(visible(c), "{c:?}");
    }
    for c in ['\n', '\u{202e}', '\u{ad}', '\u{200b}', '\u{200d}', '\u{2060}', '\u{feff}'] {
        assert!(!visible(c), "{c:?}");
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
fn quoting_escapes_as_go_does() {
    // "@" stands for the backslash that begins each of Go's escapes.
    for (text, quoted) in [
        ("plain", "\"plain\""),
        (
            "x\u{1}y\u{202e}\"é\u{a0}\t\u{7f}\u{85}😀 \u{200b}",
            "\"x@x01y@u202e@\"é@u00a0@t@x7f@u0085😀 @u200b\"",
        ),
        ("a\\b", "\"a@@b\""),
        ("\u{ad}\u{feff}\u{e000}\u{f0000}\u{2028}", "\"@u00ad@ufeff@ue000@U000f0000@u2028\""),
        ("\u{7}\u{8}\u{c}\n\r\u{b}", "\"@a@b@f@n@r@v\""),
        ("日本", "\"日本\""),
        ("א\u{10ffff}\u{fffd}", "\"א@U0010ffff\u{fffd}\""),
    ] {
        assert_eq!(quote(text), quoted.replace('@', "\\"), "{text:?}");
    }
}
