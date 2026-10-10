//! The graphite palette in the terminal's colour profile, which follows the environment, and the OSC 11 background.
use crate::{
    model::{Outcome, Stage},
    text::{Color, Profile, Style},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use graphite_meter_proto::flag::parse_bool;

/// The background query: OSC 11, then DA1, which every terminal answers, in order; none on Windows.
pub const QUERY: &[u8] = if cfg!(windows) { b"" } else { b"\x1b]11;?\x1b\\\x1b[c" };

/// A tone's light and dark shades.
type Tone = [Color; 2];

const fn tone(light: (u32, u8, u8), dark: (u32, u8, u8)) -> Tone {
    [
        Color { rgb: light.0, ansi256: light.1, ansi: light.2 },
        Color { rgb: dark.0, ansi256: dark.1, ansi: dark.2 },
    ]
}

const INK: Tone = tone((0x20242a, 235, 0), (0xe6e8eb, 254, 15));
const TEXT: Tone = tone((0x171b20, 234, 0), (0xeef0f3, 255, 15));
const SOFT: Tone = tone((0x5f646a, 241, 8), (0x8e9299, 246, 7));
const GOOD: Tone = tone((0x2e734b, 29, 2), (0x88d1a2, 115, 10));
const CAUTION: Tone = tone((0x85671f, 94, 1), (0xe8cf83, 186, 11));
const BAD: Tone = tone((0xab413e, 131, 1), (0xed8b88, 210, 9));
/// The canvas strips fade into and plates write on.
const CANVAS: Tone = tone((0xfdfdfd, 231, 15), (0x0d1013, 233, 0));
const PLATE_OFF: Tone = tone((0xdcdde0, 253, 7), (0x2a2d31, 236, 8));
const BORDER: Tone = tone((0xcacbcf, 252, 15), (0x3e4348, 238, 8));
const SELECTED: Tone = tone((0xe6e6e9, 254, 15), (0x303236, 236, 0));
const STAGES: [Tone; 4] = [
    tone((0x1d7a73, 30, 6), (0x70dbc4, 80, 14)),
    tone((0x254ea3, 25, 4), (0x71a3ff, 75, 12)),
    tone((0xa35d1d, 130, 1), (0xfeb66a, 215, 9)),
    tone((0x7f2456, 89, 1), (0xe472ac, 169, 9)),
];
const TRACES: [Tone; 4] = [
    tone((0x0f9485, 30, 6), (0x70dbc4, 80, 14)),
    tone((0x275ac8, 26, 4), (0x71a3ff, 75, 12)),
    tone((0xca6e03, 166, 1), (0xfeb66a, 215, 9)),
    tone((0x9b2065, 125, 1), (0xe472ac, 169, 9)),
];

/// The styles views draw with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub text: Style,
    pub value: Style,
    pub muted: Style,
    pub accent: Style,
    pub heading: Style,
    pub ok: Style,
    pub warn: Style,
    pub err: Style,
    pub border: Style,
    /// The focused row.
    pub selected: Style,
    /// The editor's block cursor.
    pub cursor: Style,
    /// The key: canvas on ink, its faint note and cap, and the key while it cannot be pressed.
    pub plate: Style,
    pub plate_note: Style,
    pub plate_off: Style,
    /// Each stage's strip, fading by row from its trace into the canvas.
    shades: [[Style; 6]; 4],
    dark: bool,
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        let pick = |tone: Tone| tone[usize::from(dark)];
        let shade = |tone: Tone| Style::fg(pick(tone));
        let on = |style: Style, tone: Tone| Style { bg: Some(pick(tone)), ..style };
        let fade =
            |trace: Tone| std::array::from_fn(|row| Style::fg(mix(pick(trace), pick(CANVAS), 0.8 - 0.12 * row as f64)));
        Self {
            text: shade(TEXT),
            value: shade(TEXT).bold(),
            muted: shade(SOFT),
            accent: shade(INK),
            heading: shade(INK).bold(),
            ok: shade(GOOD),
            warn: shade(CAUTION),
            err: shade(BAD).bold(),
            border: shade(BORDER),
            selected: on(shade(TEXT).bold(), SELECTED),
            cursor: on(shade(CANVAS), TEXT),
            plate: on(shade(CANVAS), INK),
            plate_note: on(Style::fg(mix(pick(INK), pick(CANVAS), 0.4)), INK),
            plate_off: on(shade(SOFT), PLATE_OFF),
            shades: TRACES.map(fade),
            dark,
        }
    }

    pub fn stage(&self, stage: Stage) -> Style {
        Style::fg(STAGES[stage as usize][usize::from(self.dark)])
    }

    /// A stage's chart trace.
    pub fn trace(&self, stage: Stage) -> Style {
        Style::fg(TRACES[stage as usize][usize::from(self.dark)])
    }

    /// A stage's strip shades, from the top row down.
    pub fn shades(&self, stage: Stage) -> &[Style; 6] {
        &self.shades[stage as usize]
    }

    /// An outcome's label: bold in its badge's colour.
    pub fn outcome(&self, outcome: Outcome) -> Style {
        let tone = [GOOD, CAUTION, CAUTION, SOFT, BAD][outcome as usize];
        Style::fg(tone[usize::from(self.dark)]).bold()
    }
}

/// `share` of `color` over `canvas` in sRGB, at the nearest of the 256 colours, and `color`'s own of the 16.
fn mix(color: Color, canvas: Color, share: f64) -> Color {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let channel = |at: u32| {
        let (over, under) = (f64::from((color.rgb >> at) as u8), f64::from((canvas.rgb >> at) as u8));
        (over * share + under * (1.0 - share)).round() as u8
    };
    let rgb = [channel(16), channel(8), channel(0)];
    let distance = |to: [u8; 3]| {
        rgb.iter()
            .zip(to)
            .map(|(a, b)| u32::from(a.abs_diff(b)).pow(2))
            .sum::<u32>()
    };
    // The 6×6×6 cube's nearest colour or the grey ramp's, whichever lies closer.
    let cube = rgb.map(|value| (0..6).min_by_key(|&index| value.abs_diff(LEVELS[index])).unwrap_or(0));
    let grey = (rgb.iter().map(|&value| u16::from(value)).sum::<u16>() / 3).saturating_sub(3) / 10;
    let grey = grey.min(23) as u8;
    let ansi256 = match distance(cube.map(|index| LEVELS[index])) <= distance([8 + 10 * grey; 3]) {
        true => 16 + (36 * cube[0] + 6 * cube[1] + cube[2]) as u8,
        false => 232 + grey,
    };
    Color {
        rgb: rgb.iter().fold(0, |rgb, &value| rgb << 8 | u32::from(value)),
        ansi256,
        ansi: color.ansi,
    }
}

/// The colour profile of a terminal, `tty` when output reaches one, from the environment `var` reads.
pub fn profile(tty: bool, var: impl Fn(&str) -> Option<String>) -> Profile {
    let flag = |name| var(name).is_some_and(|value| parse_bool(&value) == Some(true));
    let (tty, term) = (tty || flag("TTY_FORCE"), var("TERM"));
    let true_color = var("WT_SESSION").is_some_and(|session| !session.is_empty()) || flag("GOOGLE_CLOUD_SHELL");
    let env = environment(term.as_deref().unwrap_or_default(), &var("COLORTERM").unwrap_or_default(), true_color);
    let dumb = term.as_deref().map_or(!cfg!(windows), |term| term == "dumb");
    let mut profile = if tty && !dumb { env } else { Profile::Plain };
    if flag("NO_COLOR") && tty {
        return profile.min(Profile::Ascii);
    }
    if flag("CLICOLOR_FORCE") {
        profile = profile.max(Profile::Ansi).max(env);
    } else if flag("CLICOLOR") && tty && !dumb {
        profile = profile.max(Profile::Ansi);
    }
    // A named terminal reads as ANSI, and tmux as 256 colours, as terminfo and tmux report without Tc or RGB.
    match term.filter(|term| term != "dumb" && tty && profile != Profile::TrueColor) {
        Some(term) if !term.is_empty() => {
            let tmux = var("TMUX").is_some_and(|tmux| !tmux.is_empty());
            profile.max(if tmux { Profile::Ansi256 } else { Profile::Ansi })
        }
        _ => profile,
    }
}

/// The profile `TERM` and `COLORTERM` name.
fn environment(term: &str, color_term: &str, true_color_terminal: bool) -> Profile {
    let mut profile = match term {
        "" | "dumb" if cfg!(windows) => Profile::TrueColor,
        "" | "dumb" => Profile::Plain,
        _ => Profile::Ansi,
    };
    let multiplexer = term.starts_with("tmux") || term.starts_with("screen");
    if multiplexer {
        profile = profile.max(Profile::Ansi256);
    }
    let direct = ["truecolor", "24bit", "yes", "true"].contains(&color_term.to_ascii_lowercase().as_str());
    let named = ["alacritty", "contour", "foot", "ghostty", "kitty", "rio", "st", "wezterm"];
    if named.iter().any(|name| term.contains(name)) || true_color_terminal || direct && !multiplexer {
        return Profile::TrueColor;
    }
    match term.ends_with("direct") {
        true => Profile::TrueColor,
        false if term.ends_with("256color") => profile.max(Profile::Ansi256),
        false => profile,
    }
}

/// A query answer as keys: `ESC ]` as `alt+]` or split esc and `]`, characters as keys, its end as `alt+\` or `ctrl+g`.
#[derive(Debug, Default)]
pub struct Answer {
    answer: Option<Vec<u8>>,
    escaped: bool,
}

/// What a key was to an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Taken {
    Not,
    Part,
    /// The answer's end, naming a dark background or not.
    Background(bool),
}

impl Answer {
    pub fn take(&mut self, key: KeyEvent) -> Taken {
        let escaped = std::mem::replace(&mut self.escaped, key.code == KeyCode::Esc);
        let Some(answer) = &mut self.answer else {
            let opens = key.code == KeyCode::Char(']')
                && (key.modifiers == KeyModifiers::ALT || escaped && key.modifiers == KeyModifiers::NONE);
            self.answer = opens.then(|| b"\x1b]".to_vec());
            return if opens { Taken::Part } else { Taken::Not };
        };
        let end: &[u8] = match (key.code, key.modifiers) {
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) if answer.len() < 64 => {
                answer.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                return Taken::Part;
            }
            (KeyCode::Char('\\'), KeyModifiers::ALT) => b"\x1b\\",
            (KeyCode::Char('g'), KeyModifiers::CONTROL) => b"\x07",
            _ => {
                self.answer = None;
                return Taken::Not;
            }
        };
        answer.extend_from_slice(end);
        let (dark, _) = scan(answer);
        self.answer = None;
        dark.map_or(Taken::Part, Taken::Background)
    }
}

/// The answers so far: OSC 11's background, if any, and whether DA1's reply ended them.
pub fn scan(answers: &[u8]) -> (Option<bool>, bool) {
    let (mut dark, mut rest) = (None, answers);
    while let Some(escape) = rest.iter().position(|&byte| byte == 0x1b) {
        rest = &rest[escape + 1..];
        if let Some(body) = rest.strip_prefix(b"]") {
            // BEL or ST ends an OSC; another escape cuts it short.
            let Some(end) = body.iter().position(|&byte| matches!(byte, 0x07 | 0x1b)) else {
                break;
            };
            if body[end] == 0x1b && end + 1 == body.len() {
                break;
            }
            if (body[end] == 0x07 || body[end + 1] == b'\\')
                && let Some(color) = body[..end].strip_prefix(b"11;")
            {
                dark = Some(!bright(color));
            }
            rest = &body[end..];
        } else if let Some(body) = rest.strip_prefix(b"[") {
            let Some(end) = body.iter().position(|byte| (0x40..=0x7e).contains(byte)) else {
                break;
            };
            if body[0] == b'?' && body[end] == b'c' {
                return (dark, true);
            }
            rest = &body[end + 1..];
        }
    }
    (dark, false)
}

/// Whether an `rgb:`/`rgba:` or `#` colour has an HSL lightness of at least one half; an unreadable one is dark.
fn bright(color: &[u8]) -> bool {
    let hex = |digits: &str| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_hexdigit());
    let color = std::str::from_utf8(color).unwrap_or_default();
    let channels: Option<Vec<u8>> = match color.strip_prefix('#') {
        Some(digits) => {
            let wide = if digits.len() == 3 { 1 } else { 2 };
            let channel = |index: usize| digits.get(index * wide..(index + 1) * wide).filter(|pair| hex(pair));
            let value = |pair: &str| Some(u8::from_str_radix(pair, 16).ok()? * if wide == 1 { 17 } else { 1 });
            (0..3).map(|index| channel(index).and_then(value)).collect()
        }
        None => {
            let (form, components) = color.split_once(':').unwrap_or_default();
            let components: Vec<_> = components.split('/').collect();
            let channel = |part: &str| {
                let value = if hex(part) { u32::from_str_radix(part, 16).unwrap_or(u32::MAX) } else { 0 };
                (if value > 0xff { value >> 8 } else { value }) as u8
            };
            matches!((form, components.len()), ("rgb", 3) | ("rgba", 4))
                .then(|| components[..3].iter().map(|part| channel(part)).collect())
        }
    };
    channels.is_some_and(|rgb| {
        let (high, low) = (rgb.iter().max().copied().unwrap_or(0), rgb.iter().min().copied().unwrap_or(0));
        u16::from(high) + u16::from(low) >= 255
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_attributes_end_the_answers_and_only_whole_background_answers_count() {
        for (answers, expected) in [
            (&b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1b[?62;22c"[..], (Some(false), true)),
            (b"\x1b[A\x1b]11;rgb:0000/0000/0000\x07\x1b[?6c", (Some(true), true)),
            (b"\x1b]11;#fdf6e3\x07\x1b[?6c", (Some(false), true)),
            (b"\x1b]10;rgb:ffff/ffff/ffff\x07\x1b[?6c", (None, true)),
            (b"\x1b]11;rgb:ffff/ffff/ffff\x1bx\x1b[?6c", (None, true)),
            (b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\", (Some(false), false)),
            (b"\x1b]11;rgb:ffff/ffff/ffff\x1b", (None, false)),
        ] {
            assert_eq!(scan(answers), expected, "{}", answers.escape_ascii());
        }
    }
}
