//! Builds the graphite palette in the color profile the environment names, and reads the terminal's OSC 11 background.
use crate::{
    model::{Outcome, Stage},
    text::{Color, Profile, Style},
};
use std::{io, time::Duration};

/// Queries the background with OSC 11, then DA1, which every terminal answers, in order.
pub const QUERY: &[u8] = b"\x1b]11;?\x1b\\\x1b[c";
/// A terminal may take this long to answer the query.
pub const ANSWER: Duration = Duration::from_secs(2);

/// A tone has a light and a dark shade.
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
/// The canvas falls back to this when the terminal does not name its background.
const CANVAS: Tone = tone((0xfdfdfd, 231, 15), (0x0d1013, 233, 0));
const PLATE_OFF: Tone = tone((0xdcdde0, 253, 7), (0x2a2d31, 236, 8));
const BORDER: Tone = tone((0xcacbcf, 252, 15), (0x3e4348, 238, 8));
const STAGES: [Tone; Stage::ALL.len()] = [
    tone((0x1d7a73, 30, 6), (0x70dbc4, 80, 14)),
    tone((0x254ea3, 25, 4), (0x71a3ff, 75, 12)),
    tone((0xa35d1d, 130, 1), (0xfeb66a, 215, 9)),
    tone((0x7f2456, 89, 1), (0xe472ac, 169, 9)),
];
const TRACES: [Tone; Stage::ALL.len()] = [
    tone((0x0f9485, 30, 6), (0x70dbc4, 80, 14)),
    tone((0x275ac8, 26, 4), (0x71a3ff, 75, 12)),
    tone((0xca6e03, 166, 1), (0xfeb66a, 215, 9)),
    tone((0x9b2065, 125, 1), (0xe472ac, 169, 9)),
];

/// Views draw with these styles and fills.
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
    /// Draws the editor's block cursor.
    pub cursor: Style,
    /// Styles the run key as ink with the canvas on it, its faint note, and the quiet plate of Stop and of a key that
    /// cannot be pressed.
    pub plate: Style,
    pub plate_note: Style,
    pub plate_off: Style,
    /// Tiles sit one step over the canvas, plates and key caps two. The pointer's wash and the focused row's follow.
    pub surface: Color,
    pub raised: Color,
    pub hover: Color,
    pub selected: Color,
    /// Holds the terminal's own background when it said, so fills land on it exactly.
    canvas: Color,
    dark: bool,
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        Self::on(dark, CANVAS[usize::from(dark)])
    }

    /// Builds the palette over the background a terminal named.
    pub fn over(background: Background) -> Self {
        let dark = background.dark();
        let canvas = CANVAS[usize::from(dark)];
        Self::on(dark, background.0.map_or(canvas, |rgb| nearest(rgb, canvas.ansi)))
    }

    fn on(dark: bool, canvas: Color) -> Self {
        let pick = |tone: Tone| tone[usize::from(dark)];
        let shade = |tone: Tone| Style::fg(pick(tone));
        let lift = |share| mix(pick(TEXT), canvas, share);
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
            cursor: Style { bg: Some(pick(TEXT)), ..Style::fg(canvas) },
            plate: Style { bg: Some(pick(INK)), ..Style::fg(canvas) },
            plate_note: Style {
                bg: Some(pick(INK)),
                ..Style::fg(mix(pick(INK), canvas, 0.45))
            },
            plate_off: Style { bg: Some(pick(PLATE_OFF)), ..shade(SOFT) },
            surface: lift(if dark { 0.045 } else { 0.03 }),
            raised: lift(if dark { 0.09 } else { 0.07 }),
            hover: lift(if dark { 0.07 } else { 0.05 }),
            selected: lift(if dark { 0.13 } else { 0.1 }),
            canvas,
            dark,
        }
    }

    pub fn canvas(&self) -> Color {
        self.canvas
    }

    pub fn stage(&self, stage: Stage) -> Style {
        Style::fg(STAGES[stage.index()][usize::from(self.dark)])
    }

    /// Colors a stage's chart trace.
    pub fn trace(&self, stage: Stage) -> Color {
        TRACES[stage.index()][usize::from(self.dark)]
    }

    /// Mixes `share` of a stage's hue over `under`, for a tile's wash, a chart's field or a box plot's box.
    pub fn wash(&self, stage: Stage, share: f64, under: Color) -> Color {
        mix(self.trace(stage), under, share)
    }

    /// Styles an outcome's label bold in its badge's color.
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

/// Mixes `share` of `color` over `under` in sRGB, at the nearest of the 256 colors and `color`'s own of the 16.
pub fn mix(color: Color, under: Color, share: f64) -> Color {
    let channel = |at: u32| {
        let (over, below) = (f64::from((color.rgb >> at) as u8), f64::from((under.rgb >> at) as u8));
        (over * share + below * (1.0 - share)).round() as u8
    };
    nearest([channel(16), channel(8), channel(0)], color.ansi)
}

/// Pairs `rgb` with the nearest of the 256 colors and `ansi` of the 16.
pub fn nearest(rgb: [u8; 3], ansi: u8) -> Color {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let distance = |to: [u8; 3]| {
        rgb.iter()
            .zip(to)
            .map(|(a, b)| u32::from(a.abs_diff(b)).pow(2))
            .sum::<u32>()
    };
    // The 6×6×6 cube's nearest color or the grey ramp's wins, whichever lies closer.
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
        ansi,
    }
}

/// Picks a terminal's color profile from the environment `var` reads, `tty` when output reaches one.
pub fn profile(tty: bool, var: impl Fn(&str) -> Option<String>) -> Profile {
    let set = |name| var(name).is_some_and(|value| !value.is_empty() && value != "0");
    let term = var("TERM").unwrap_or_default();
    if set("NO_COLOR") {
        return if tty { Profile::Ascii } else { Profile::Plain };
    }
    if !(set("CLICOLOR_FORCE") || set("FORCE_COLOR")) && (!tty || term == "dumb") {
        return Profile::Plain;
    }
    const DIRECT: [&str; 8] = ["alacritty", "contour", "foot", "ghostty", "kitty", "rio", "st", "wezterm"];
    let color_term = var("COLORTERM").unwrap_or_default().to_ascii_lowercase();
    // tmux and screen pass on 256 colors unless told otherwise, and screen never 24-bit.
    let multiplexer = term.starts_with("tmux") || term.starts_with("screen");
    let direct = matches!(color_term.as_str(), "truecolor" | "24bit" | "yes" | "true")
        || set("WT_SESSION")
        || term.ends_with("direct")
        || term.split('-').any(|part| DIRECT.contains(&part));
    match () {
        _ if direct && !multiplexer => Profile::TrueColor,
        _ if multiplexer || term.contains("256color") => Profile::Ansi256,
        // A Windows console sets no TERM and takes 24-bit color.
        _ if term.is_empty() && cfg!(windows) => Profile::TrueColor,
        _ => Profile::Ansi,
    }
}

/// Keeps the terminal in raw mode until dropped.
pub struct Raw;

impl Raw {
    pub fn enter() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode().map(|()| Self)
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// The background query brought the background, if named, and whether Ctrl+C was pressed meanwhile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Answer {
    pub background: Option<Background>,
    pub interrupted: bool,
}

/// Asks a raw terminal on stdin and stdout for its background until DA1 answers, Ctrl+C or `limit` passed.
#[cfg(unix)]
pub fn ask(limit: Duration) -> Answer {
    use rustix::{
        event::{PollFd, PollFlags, Timespec, poll},
        io::Errno,
    };
    use std::{io::Write, time::Instant};
    let mut out = io::stdout();
    if out.write_all(QUERY).and_then(|()| out.flush()).is_err() {
        return Answer::default();
    }
    let (stdin, deadline) = (io::stdin(), Instant::now() + limit);
    let (mut answers, mut chunk) = (Vec::new(), [0; 1024]);
    while answers.len() < 4096 {
        let left = Timespec::try_from(deadline.saturating_duration_since(Instant::now())).unwrap_or_default();
        match poll(&mut [PollFd::new(&stdin, PollFlags::IN)], Some(&left)) {
            Err(Errno::INTR) => continue,
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(read @ 1..) = rustix::io::read(&stdin, &mut chunk) else { break };
        answers.extend_from_slice(&chunk[..read]);
        // Raw mode turns Ctrl+C into a byte, which interrupts as the signal would.
        let (background, ended) = scan(&answers);
        let interrupted = answers.contains(&0x03);
        if ended || interrupted {
            return Answer { background, interrupted };
        }
    }
    Answer { background: scan(&answers).0, interrupted: false }
}

#[cfg(not(unix))]
pub fn ask(_: Duration) -> Answer {
    Answer::default()
}

/// A terminal says its background is this color, unless it could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Background(pub Option<[u8; 3]>);

impl Background {
    /// Tells whether the background has an HSL lightness under one half. An unreadable one is dark.
    pub fn dark(self) -> bool {
        self.0.is_none_or(|rgb| {
            let (high, low) = (rgb.iter().max().copied().unwrap_or(0), rgb.iter().min().copied().unwrap_or(0));
            u16::from(high) + u16::from(low) < 255
        })
    }
}

/// Scans the answers so far for OSC 11's background, if any, and whether DA1's reply ended them.
pub fn scan(answers: &[u8]) -> (Option<Background>, bool) {
    let (mut background, mut rest) = (None, answers);
    while let Some(escape) = rest.iter().position(|&byte| byte == 0x1b) {
        rest = &rest[escape + 1..];
        if let Some(body) = rest.strip_prefix(b"]") {
            // BEL or ST ends an OSC. Another escape cuts it short.
            let Some(end) = body.iter().position(|&byte| matches!(byte, 0x07 | 0x1b)) else {
                break;
            };
            if body[end] == 0x1b && end + 1 == body.len() {
                break;
            }
            if (body[end] == 0x07 || body[end + 1] == b'\\')
                && let Some(color) = body[..end].strip_prefix(b"11;")
            {
                background = Some(Background(rgb(color)));
            }
            rest = &body[end..];
        } else if let Some(body) = rest.strip_prefix(b"[") {
            let Some(end) = body.iter().position(|byte| (0x40..=0x7e).contains(byte)) else {
                break;
            };
            if body[0] == b'?' && body[end] == b'c' {
                return (background, true);
            }
            rest = &body[end + 1..];
        }
    }
    (background, false)
}

/// Reads an `rgb:`/`rgba:` or `#` color's channels.
fn rgb(color: &[u8]) -> Option<[u8; 3]> {
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
    channels.and_then(|rgb| rgb.try_into().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_profile_follows_the_terminal_its_overrides_and_multiplexers() {
        use Profile::*;
        type Vars = &'static [(&'static str, &'static str)];
        #[rustfmt::skip]
        let rows: [(bool, Vars, Profile); 16] = [
            (true, &[("TERM", "xterm")], Ansi),
            (true, &[("TERM", "xterm-256color")], Ansi256),
            (true, &[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")], TrueColor),
            (true, &[("TERM", "xterm-256color"), ("COLORTERM", "yes")], TrueColor),
            (true, &[("TERM", "xterm-kitty")], TrueColor),
            (true, &[("TERM", "st-256color")], TrueColor),
            (true, &[("TERM", "xterm-direct")], TrueColor),
            (true, &[("TERM", "xterm-256color"), ("WT_SESSION", "1")], TrueColor),
            (true, &[("TERM", "screen")], Ansi256),
            (true, &[("TERM", "screen-256color"), ("COLORTERM", "truecolor")], Ansi256),
            (true, &[("TERM", "tmux-256color"), ("COLORTERM", "truecolor")], Ansi256),
            (true, &[("TERM", "dumb")], Plain),
            (false, &[("TERM", "xterm-256color")], Plain),
            (false, &[("TERM", "xterm-256color"), ("FORCE_COLOR", "1")], Ansi256),
            (true, &[("TERM", "xterm-kitty"), ("NO_COLOR", "1")], Ascii),
            (false, &[("TERM", "xterm"), ("CLICOLOR_FORCE", "1"), ("NO_COLOR", "1")], Plain),
        ];
        for (tty, vars, expected) in rows {
            let var = |name: &str| {
                vars.iter()
                    .find(|(own, _)| *own == name)
                    .map(|(_, value)| (*value).to_owned())
            };
            assert_eq!(profile(tty, var), expected, "{tty} {vars:?}");
        }
    }

    #[test]
    fn device_attributes_end_the_answers_and_only_whole_background_answers_count() {
        for (answers, expected) in [
            (&b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1b[?62;22c"[..], (Some(Some([255; 3])), true)),
            (b"\x1b[A\x1b]11;rgb:0000/0000/0000\x07\x1b[?6c", (Some(Some([0; 3])), true)),
            (b"\x1b]11;#fdf6e3\x07\x1b[?6c", (Some(Some([0xfd, 0xf6, 0xe3])), true)),
            (b"\x1b]11;rgb:1010/1414/1818\x07\x1b[?6c", (Some(Some([0x10, 0x14, 0x18])), true)),
            (b"\x1b]11;nonsense\x07\x1b[?6c", (Some(None), true)),
            (b"\x1b]10;rgb:ffff/ffff/ffff\x07\x1b[?6c", (None, true)),
            (b"\x1b]11;rgb:ffff/ffff/ffff\x1bx\x1b[?6c", (None, true)),
            (b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\", (Some(Some([255; 3])), false)),
            (b"\x1b]11;rgb:ffff/ffff/ffff\x1b", (None, false)),
        ] {
            let (background, ended) = scan(answers);
            assert_eq!((background.map(|background| background.0), ended), expected, "{}", answers.escape_ascii());
        }
        assert!(!Background(Some([0xfd, 0xf6, 0xe3])).dark() && Background(Some([0x10, 0x14, 0x18])).dark());
        assert!(Background(None).dark());
        let named = Palette::over(Background(Some([0x10, 0x14, 0x18])));
        assert_eq!(named.canvas().rgb, 0x101418, "blends land on the terminal's own background");
    }
}
