//! Origin identities preserve explicit port and IPv6 spelling used by Go catalogs.
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub scheme: String,
    pub host: String,
    pub port: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OriginError;
impl fmt::Display for OriginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected an absolute HTTP(S) origin")
    }
}
impl std::error::Error for OriginError {}

impl Origin {
    pub fn port_number(&self) -> u16 {
        self.port
            .as_deref()
            .map_or(if self.scheme == "https" { 443 } else { 80 }, |port| {
                port.parse().expect("validated origin port")
            })
    }

    pub fn authority(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        match &self.port {
            Some(port) => format!("{host}:{port}"),
            None => host,
        }
    }

    pub fn key(&self) -> String {
        let mut normalized = self.clone();
        normalized.scheme.make_ascii_lowercase();
        normalized.host.make_ascii_lowercase();
        if matches!(
            (normalized.scheme.as_str(), normalized.port.as_deref()),
            ("http", Some("80")) | ("https", Some("443"))
        ) {
            normalized.port = None;
        }
        format!("{}://{}", normalized.scheme, normalized.authority())
    }
}

/// A relative self target is represented explicitly, never as an empty hostname.
pub fn target_origin(raw: &str) -> Result<Option<Origin>, OriginError> {
    if raw == "." {
        return Ok(None);
    }
    if raw.len() > 2048
        || raw
            .bytes()
            .any(|c| c <= b' ' || c == 127 || matches!(c, b'\\' | b'#' | b'?'))
    {
        return Err(OriginError);
    }
    let (scheme, authority) = raw.split_once("://").ok_or(OriginError)?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") || authority.contains(['/', '@']) {
        return Err(OriginError);
    }
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, suffix) = bracketed.split_once(']').ok_or(OriginError)?;
        host.parse::<std::net::Ipv6Addr>().map_err(|_| OriginError)?;
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').ok_or(OriginError)?)
        };
        (host, port)
    } else {
        let (host, port) = authority
            .split_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port)));
        if !ascii_name(host) {
            return Err(OriginError);
        }
        (host, port)
    };
    let port = port.filter(|port| !port.is_empty());
    if let Some(port) = port
        && (!port.bytes().all(|c| c.is_ascii_digit()) || port.parse::<u16>().is_err())
    {
        return Err(OriginError);
    }
    Ok(Some(Origin {
        scheme,
        host: host.to_owned(),
        port: port.map(str::to_owned),
    }))
}

pub fn canonical_origin(raw: &str) -> Result<String, OriginError> {
    let origin = target_origin(raw)?.ok_or(OriginError)?;
    if origin.port_number() == 0 {
        return Err(OriginError);
    }
    Ok(origin.key())
}

/// A received catalogue origin as Go's CatalogOrigin reads it: one trailing slash, which
/// servers.schema.json allows, and an international host, in punycode as Go's client dials it.
/// Configured origins keep `canonical_origin`'s rules.
pub fn catalog_origin(raw: &str) -> Result<String, OriginError> {
    canonical_origin(&ascii_origin(raw.strip_suffix('/').unwrap_or(raw))?)
}

/// `raw` with an international host in punycode, as Go's idna.Lookup.ToASCII converts it for the
/// scripts `kept` names; an ASCII origin is returned as it is, and anything else is refused.
pub fn ascii_origin(raw: &str) -> Result<String, OriginError> {
    if raw.is_ascii() {
        return Ok(raw.to_owned());
    }
    let (scheme, authority) = raw.split_once("://").ok_or(OriginError)?;
    // The port, if any, keeps its colon.
    let (host, port) = authority.split_at(authority.rfind(':').unwrap_or(authority.len()));
    Ok(format!("{scheme}://{}{port}", ascii_host(host).ok_or(OriginError)?))
}

/// `host` with each international label in punycode, as [`ascii_origin`] converts them.
pub fn ascii_host(host: &str) -> Option<String> {
    Some(host.split('.').map(ascii_label).collect::<Option<Vec<_>>>()?.join("."))
}

/// One host label: an ASCII label is left to the origin's own checks; any other is lowercased,
/// must hold only letters `kept` names, ASCII lowercase letters, digits and inner hyphens, with
/// right-to-left letters not mixed with left-to-right ones, and becomes an `xn--` label.
fn ascii_label(label: &str) -> Option<String> {
    if label.is_ascii() {
        return Some(label.to_owned());
    }
    let mut lowered = Vec::with_capacity(label.len());
    for original in label.chars() {
        let mut lower = original.to_lowercase();
        let (Some(letter), None) = (lower.next(), lower.next()) else {
            return None;
        };
        let ascii = letter.is_ascii_lowercase() || letter.is_ascii_digit() || letter == '-';
        if !ascii && !(kept(original) && kept(letter)) {
            return None;
        }
        lowered.push(letter);
    }
    let hyphens =
        lowered.first() == Some(&'-') || lowered.last() == Some(&'-') || lowered.get(2..4) == Some(&['-', '-'][..]);
    // RFC 5893 within the label: a right-to-left label holds no left-to-right letter and begins
    // and ends with a right-to-left one.
    let rtl = |letter: &char| matches!(letter, '\u{5d0}'..='\u{5ff}' | '\u{620}'..='\u{64a}');
    let mixed = lowered.iter().any(rtl)
        && (lowered.iter().any(char::is_ascii_lowercase)
            || lowered.iter().any(|letter| !letter.is_ascii() && !rtl(letter))
            || !lowered.first().is_some_and(rtl)
            || !lowered.last().is_some_and(rtl));
    if hyphens || mixed {
        return None;
    }
    let label = format!("xn--{}", punycode(&lowered)?);
    (label.len() <= 63).then_some(label)
}

/// Letters that UTS 46 keeps as their lowercase form, which NFC leaves alone: Latin, IPA, Greek,
/// Cyrillic, Armenian, Hebrew, Arabic, Georgian, kana, CJK ideographs and Hangul syllables,
/// without the ligatures, digraphs, compatibility and joining forms IDNA maps to other letters.
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

/// RFC 3492's encoder; `None` only on overflow, which no 63-octet label reaches.
fn punycode(input: &[char]) -> Option<String> {
    const BASE: u32 = 36;
    const T_MIN: u32 = 1;
    const T_MAX: u32 = 26;
    fn digit(value: u32) -> char {
        char::from(if value < 26 {
            b'a' + value as u8
        } else {
            b'0' + (value - 26) as u8
        })
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
    let mut output: String = input.iter().filter(|letter| letter.is_ascii()).collect();
    let basic = output.len() as u32;
    if basic > 0 {
        output.push('-');
    }
    let (mut code, mut delta, mut bias, mut handled) = (128_u32, 0_u32, 72_u32, basic);
    while (handled as usize) < input.len() {
        let next = input
            .iter()
            .map(|&letter| u32::from(letter))
            .filter(|&letter| letter >= code)
            .min()?;
        delta = delta.checked_add((next - code).checked_mul(handled + 1)?)?;
        code = next;
        for &letter in input {
            let letter = u32::from(letter);
            if letter < code {
                delta = delta.checked_add(1)?;
            }
            if letter == code {
                let mut rest = delta;
                let mut k = BASE;
                loop {
                    let threshold = k.saturating_sub(bias).clamp(T_MIN, T_MAX);
                    if rest < threshold {
                        break;
                    }
                    output.push(digit(threshold + (rest - threshold) % (BASE - threshold)));
                    rest = (rest - threshold) / (BASE - threshold);
                    k += BASE;
                }
                output.push(digit(rest));
                bias = adapt(delta, handled + 1, handled == basic);
                delta = 0;
                handled += 1;
            }
        }
        delta = delta.checked_add(1)?;
        code += 1;
    }
    Some(output)
}

fn ascii_name(host: &str) -> bool {
    let name = host.strip_suffix('.').unwrap_or(host);
    let last = name.rsplit('.').next().unwrap_or_default();
    let numeric = last.bytes().all(|c| c.is_ascii_digit())
        || last
            .strip_prefix("0x")
            .or_else(|| last.strip_prefix("0X"))
            .is_some_and(|hex| hex.bytes().all(|c| c.is_ascii_hexdigit()));
    name.split('.').all(|label| {
        !label.is_empty()
            && label
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    }) && (!numeric || host.parse::<std::net::Ipv4Addr>().is_ok())
}

pub fn split_url(raw: &str) -> Result<(Origin, &str), OriginError> {
    let start = raw.find("://").ok_or(OriginError)? + 3;
    let end = raw[start..].find(['/', '?']).map_or(raw.len(), |index| start + index);
    let origin = target_origin(&raw[..end])?.ok_or(OriginError)?;
    let rest = &raw[end..];
    if rest.len() > 2048 || rest.bytes().any(|c| c <= b' ' || c >= 127 || matches!(c, b'#' | b'\\')) {
        return Err(OriginError);
    }
    Ok((origin, rest))
}

pub fn key(raw: &str) -> String {
    match target_origin(raw) {
        Ok(Some(origin)) => origin.key(),
        _ => raw.to_owned(),
    }
}

pub fn browser_connect_source_supported(raw: &str) -> bool {
    !raw.contains("://[")
}
