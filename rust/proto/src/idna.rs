//! International host names in punycode, for the letters IDNA keeps as they are.

const BASE: u32 = 36;
const T_MIN: u32 = 1;
const T_MAX: u32 = 26;

/// `host` with each international label in punycode, as Go's IDNA lookup converts it; `None` for a label holding
/// a character IDNA would map to another, a combining mark, mixed writing directions or misplaced hyphens.
pub fn to_ascii(host: &str) -> Option<String> {
    let host = host.replace(['\u{3002}', '\u{ff0e}', '\u{ff61}'], ".");
    let labels: Vec<String> = host.split('.').map(label).collect::<Option<_>>()?;
    Some(labels.join("."))
}

fn label(text: &str) -> Option<String> {
    if text.is_ascii() {
        return ascii_label(text);
    }
    let letters: Vec<char> = text.chars().map(lowercase_kept).collect::<Option<_>>()?;
    if !hyphens_allowed(&letters) || mixes_directions(&letters) {
        return None;
    }
    let label = format!("xn--{}", punycode(&letters)?);
    (label.len() <= 63).then_some(label)
}

/// A label beside international ones: lowercase letters, digits and hyphens, placed as in any other label
/// unless it is punycode already.
fn ascii_label(text: &str) -> Option<String> {
    let label = text.to_ascii_lowercase();
    let ldh_byte = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-';
    let ldh = label.bytes().all(ldh_byte);
    let edges = label.starts_with('-') || label.ends_with('-');
    let reserved = label.get(2..4) == Some("--") && !label.starts_with("xn--");
    (ldh && !edges && !reserved).then_some(label)
}

/// No hyphen first, last, or in both the third and fourth places.
fn hyphens_allowed(letters: &[char]) -> bool {
    letters.first() != Some(&'-') && letters.last() != Some(&'-') && letters.get(2..4) != Some(&['-', '-'][..])
}

/// The lowercase form of a kept letter, ASCII letter, digit or hyphen.
fn lowercase_kept(original: char) -> Option<char> {
    let mut lower = original.to_lowercase();
    let (Some(letter), None) = (lower.next(), lower.next()) else {
        return None;
    };
    let ascii = letter.is_ascii_lowercase() || letter.is_ascii_digit() || letter == '-';
    (ascii || kept(original) && kept(letter)).then_some(letter)
}

/// Whether a label with right-to-left letters also holds left-to-right ones, or begins or ends otherwise (RFC 5893).
fn mixes_directions(letters: &[char]) -> bool {
    let rtl = |letter: &char| matches!(letter, '\u{5d0}'..='\u{5ff}' | '\u{620}'..='\u{64a}');
    let ltr = |letter: &char| letter.is_alphabetic() && !rtl(letter);
    letters.iter().any(rtl)
        && (letters.iter().any(ltr) || !letters.first().is_some_and(rtl) || !letters.last().is_some_and(rtl))
}

/// Letters UTS 46 maps to themselves and NFC leaves alone: Latin, IPA, Greek, Cyrillic, Armenian, Hebrew, Arabic,
/// Georgian, kana, CJK ideographs and Hangul syllables, without ligatures, digraphs and compatibility forms.
fn kept(letter: char) -> bool {
    letter.is_alphabetic()
        && matches!(letter,
            '\u{c0}'..='\u{d6}' | '\u{d8}'..='\u{f6}' | '\u{f8}'..='\u{12f}' | '\u{131}'
            | '\u{134}'..='\u{13e}' | '\u{141}'..='\u{148}' | '\u{14a}'..='\u{17e}'
            | '\u{180}'..='\u{1c3}' | '\u{1cd}'..='\u{1f0}' | '\u{1f4}'..='\u{2af}'
            | '\u{386}' | '\u{388}'..='\u{38a}' | '\u{38c}' | '\u{38e}'..='\u{3a1}' | '\u{3a3}'..='\u{3ce}'
            | '\u{400}'..='\u{481}' | '\u{48a}'..='\u{52f}'
            | '\u{531}'..='\u{556}' | '\u{560}'..='\u{586}' | '\u{588}'
            | '\u{5d0}'..='\u{5ea}' | '\u{5ef}'..='\u{5f2}' | '\u{620}'..='\u{63f}' | '\u{641}'..='\u{64a}'
            | '\u{10a0}'..='\u{10c5}' | '\u{10c7}' | '\u{10cd}' | '\u{10d0}'..='\u{10fa}' | '\u{10fd}'..='\u{10ff}'
            | '\u{2d00}'..='\u{2d25}' | '\u{2d27}' | '\u{2d2d}'
            | '\u{3005}'..='\u{3007}' | '\u{3041}'..='\u{3096}' | '\u{309d}'..='\u{309e}'
            | '\u{30a1}'..='\u{30fa}' | '\u{30fc}'..='\u{30fe}'
            | '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{ac00}'..='\u{d7a3}'
            | '\u{20000}'..='\u{2a6df}' | '\u{2a700}'..='\u{2ebef}' | '\u{30000}'..='\u{3134f}')
}

/// RFC 3492's encoder; `None` on overflow, which no 63-byte label reaches.
fn punycode(letters: &[char]) -> Option<String> {
    let mut output: String = letters.iter().filter(|letter| letter.is_ascii()).collect();
    let basic = output.len() as u32;
    if basic > 0 {
        output.push('-');
    }
    let (mut least, mut delta, mut bias, mut handled) = (0x80, 0_u32, 72, basic);
    while (handled as usize) < letters.len() {
        let code = letters
            .iter()
            .map(|&letter| u32::from(letter))
            .filter(|&code| code >= least)
            .min()?;
        delta = delta.checked_add((code - least).checked_mul(handled + 1)?)?;
        for letter in letters.iter().map(|&letter| u32::from(letter)) {
            if letter < code {
                delta = delta.checked_add(1)?;
            }
            if letter == code {
                let mut rest = delta;
                for k in (BASE..).step_by(BASE as usize) {
                    let threshold = k.saturating_sub(bias).clamp(T_MIN, T_MAX);
                    if rest < threshold {
                        break;
                    }
                    output.push(digit(threshold + (rest - threshold) % (BASE - threshold)));
                    rest = (rest - threshold) / (BASE - threshold);
                }
                output.push(digit(rest));
                bias = adapt(delta, handled + 1, handled == basic);
                delta = 0;
                handled += 1;
            }
        }
        delta = delta.checked_add(1)?;
        least = code + 1;
    }
    Some(output)
}

fn adapt(delta: u32, points: u32, first: bool) -> u32 {
    let mut delta = delta / if first { 700 } else { 2 };
    delta += delta / points;
    let mut k = 0;
    while delta > (BASE - T_MIN) * T_MAX / 2 {
        delta /= BASE - T_MIN;
        k += BASE;
    }
    k + (BASE - T_MIN + 1) * delta / (delta + 38)
}

fn digit(value: u32) -> char {
    char::from(if value < 26 { b'a' + value as u8 } else { b'0' + (value - 26) as u8 })
}
