//! The graphite palette in the terminal's colour profile, which follows the environment as Go's colorprofile does,
//! and the background the terminal reports to an OSC 11 query.
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
const BADGE: Tone = tone((0xfdfdfd, 231, 15), (0x0d1013, 233, 0));
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
    /// The title badge, and the status pill without a colour of its own.
    pub title: Style,
    pub pill: Style,
    /// The editor's block cursor.
    pub cursor: Style,
    dark: bool,
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        let shade = |tone: Tone| Style::fg(tone[usize::from(dark)]);
        let on = |style: Style, tone: Tone| Style { bg: Some(tone[usize::from(dark)]), ..style };
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
            title: on(shade(BADGE).bold(), INK),
            pill: on(shade(BADGE).bold(), SOFT),
            cursor: on(shade(BADGE), TEXT),
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

    /// A pill on `style`'s colour.
    pub fn badge(&self, style: Style) -> Style {
        Style { bg: style.fg, ..self.pill }
    }

    /// An outcome's label: bold in its badge's colour.
    pub fn outcome(&self, outcome: Outcome) -> Style {
        let tone = match outcome {
            Outcome::Complete => GOOD,
            Outcome::Partial | Outcome::Incomplete => CAUTION,
            Outcome::Stopped => SOFT,
            Outcome::Failed => BAD,
        };
        Style::fg(tone[usize::from(self.dark)]).bold()
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
        return Profile::Plain;
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

/// Asks the terminal on stdin and stdout for its background in raw mode, reading until the DA1 reply or `limit`:
/// whether it is dark, if it answered.
#[cfg(unix)]
pub async fn background(limit: std::time::Duration) -> Option<bool> {
    use std::{
        io::{Read, Write},
        os::fd::AsFd,
    };
    let input = std::fs::File::from(std::io::stdin().as_fd().try_clone_to_owned().ok()?);
    let input = tokio::io::unix::AsyncFd::with_interest(input, tokio::io::Interest::READABLE).ok()?;
    crossterm::terminal::enable_raw_mode().ok()?;
    let mut output = std::io::stdout();
    let (mut answers, mut dark) = (Vec::new(), None);
    if output.write_all(QUERY).and_then(|()| output.flush()).is_ok() {
        let _ = tokio::time::timeout(limit, async {
            let mut chunk = [0; 1024];
            while answers.len() < 4096 {
                let Ok(mut ready) = input.readable().await else { return };
                let Ok(read @ 1..) = ready.get_inner().read(&mut chunk) else { return };
                ready.clear_ready();
                answers.extend_from_slice(&chunk[..read]);
                let ended;
                (dark, ended) = scan(&answers);
                if ended {
                    return;
                }
            }
        })
        .await;
    }
    let _ = crossterm::terminal::disable_raw_mode();
    dark
}

/// An answer to the query arriving as keys: its `ESC ]` as `alt+]`, its characters as keys, its end as `alt+\` or
/// `ctrl+g`.
#[derive(Debug, Default)]
pub struct Answer(Option<Vec<u8>>);

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
        let Some(answer) = &mut self.0 else {
            let opens = key.code == KeyCode::Char(']') && key.modifiers == KeyModifiers::ALT;
            self.0 = opens.then(|| b"\x1b]".to_vec());
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
                self.0 = None;
                return Taken::Not;
            }
        };
        answer.extend_from_slice(end);
        let (dark, _) = scan(answer);
        self.0 = None;
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
            let value = |pair: &str| {
                u8::from_str_radix(pair, 16)
                    .ok()
                    .map(|value| if wide == 1 { value * 17 } else { value })
            };
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
