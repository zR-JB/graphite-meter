//! Go's TUI palette (the web tokens), with the indexed and ANSI colors Go's color profiles convert each to.

use crate::model::Stage;
use ratatui::style::Color;

/// The default theme is monochrome: every colour is the terminal's own.
#[derive(Clone, Copy, Default)]
pub(crate) struct Theme {
    pub ink: Color,
    pub text: Color,
    pub muted: Color,
    pub inverse: Color,
    pub surface: Color,
    pub border: Color,
    pub ok: Color,
    pub warn: Color,
    pub err: Color,
    stages: [Color; 4],
}

#[derive(Clone, Copy)]
enum Depth {
    TrueColor,
    Indexed,
    Ansi,
}

/// Go's TUI asks for the background with OSC 11. DA1 follows, as in lipgloss's query: every
/// terminal answers it, and in order, so its reply ends the wait.
#[cfg(unix)]
const QUERY: &[u8] = b"\x1b]11;?\x1b\\\x1b[c";
/// Go's input parser takes an answer whenever it comes; one arriving after this would reach
/// crossterm as key presses, so the wait covers a slow link's round trip.
#[cfg(unix)]
const ANSWER_LIMIT: std::time::Duration = std::time::Duration::from_secs(1);

/// The terminal's answer to the TUI's query, so the report printed after it uses the same palette, as in Go.
static ANSWER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

impl Theme {
    pub fn terminal() -> Self {
        Self::background(ANSWER.get().copied())
    }

    /// The env decides first; otherwise the terminal's answer does. Call it in raw mode only.
    pub async fn ask() -> Self {
        if plain() || preference().is_some() {
            return Self::terminal();
        }
        // The answer arrives where crossterm reads keys: stdin, or the terminal device when it is redirected.
        #[cfg(unix)]
        let answer = {
            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() {
                answer(std::io::stdin(), &mut std::io::stdout(), ANSWER_LIMIT).await
            } else if let Ok(terminal) = std::fs::File::open("/dev/tty") {
                answer(&terminal, &mut std::io::stdout(), ANSWER_LIMIT).await
            } else {
                None
            }
        };
        #[cfg(not(unix))]
        let answer = None; // Windows consoles would deliver the answer as key events.
        if let Some(light) = answer {
            let _ = ANSWER.set(light);
        }
        Self::background(answer)
    }

    fn background(answer: Option<bool>) -> Self {
        if plain() {
            return Self::default();
        }
        let depth = Depth::terminal();
        if preference().or(answer).unwrap_or(false) {
            Self::light(depth)
        } else {
            Self::dark(depth)
        }
    }

    pub fn stage(&self, stage: Stage) -> Color {
        self.stages[stage as usize]
    }

    fn dark(depth: Depth) -> Self {
        Self {
            ink: tone(0xe6e8ea, 254, Color::White, depth),
            text: tone(0xeef0f3, 255, Color::White, depth),
            muted: tone(0x8e9299, 246, Color::Gray, depth),
            inverse: tone(0x0d1013, 233, Color::Black, depth),
            surface: tone(0x303236, 236, Color::Black, depth),
            border: tone(0x3e4348, 238, Color::DarkGray, depth),
            ok: tone(0x88d1a2, 115, Color::LightGreen, depth),
            warn: tone(0xe8cf83, 186, Color::LightYellow, depth),
            err: tone(0xed8b88, 210, Color::LightRed, depth),
            stages: [
                tone(0x70dbc4, 80, Color::LightCyan, depth),
                tone(0x71a3ff, 75, Color::LightBlue, depth),
                tone(0xfeb66a, 215, Color::LightRed, depth),
                tone(0xe472ac, 169, Color::LightRed, depth),
            ],
        }
    }

    fn light(depth: Depth) -> Self {
        Self {
            ink: tone(0x20242a, 235, Color::Black, depth),
            text: tone(0x171b20, 234, Color::Black, depth),
            muted: tone(0x5f646a, 241, Color::DarkGray, depth),
            inverse: tone(0xfdfdfd, 231, Color::White, depth),
            surface: tone(0xe6e6e9, 254, Color::White, depth),
            border: tone(0xcacbcf, 252, Color::White, depth),
            ok: tone(0x2e734b, 29, Color::Green, depth),
            warn: tone(0x85671f, 94, Color::Red, depth),
            err: tone(0xab413e, 131, Color::Red, depth),
            stages: [
                tone(0x1d7a73, 30, Color::Cyan, depth),
                tone(0x254ea3, 25, Color::Blue, depth),
                tone(0xa35d1d, 130, Color::Red, depth),
                tone(0x7f2456, 89, Color::Red, depth),
            ],
        }
    }
}

impl Depth {
    fn terminal() -> Self {
        let term = std::env::var("TERM").unwrap_or_default().to_ascii_lowercase();
        let color_term = std::env::var("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        // Windows consoles render 24-bit color without advertising it; Go's TUI assumes the same.
        if cfg!(windows)
            || matches!(color_term.as_str(), "truecolor" | "24bit")
            || term.ends_with("-direct")
            || term.ends_with("-truecolor")
        {
            Self::TrueColor
        } else if term.contains("256color") {
            Self::Indexed
        } else {
            Self::Ansi
        }
    }
}

const fn tone(rgb: u32, indexed: u8, ansi: Color, depth: Depth) -> Color {
    match depth {
        Depth::TrueColor => Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8),
        Depth::Indexed => Color::Indexed(indexed),
        Depth::Ansi => ansi,
    }
}

fn plain() -> bool {
    std::env::var_os("NO_COLOR").is_some() || std::env::var("TERM").is_ok_and(|term| term == "dumb")
}

/// GM_TUI_THEME, then COLORFGBG's background: whether the env picks a light background.
fn preference() -> Option<bool> {
    match std::env::var("GM_TUI_THEME").ok().as_deref() {
        Some("light") => Some(true),
        Some("dark") => Some(false),
        _ => std::env::var("COLORFGBG")
            .ok()
            .and_then(|value| value.rsplit(';').next()?.parse::<u8>().ok())
            .map(|background| matches!(background, 7 | 9..=15)),
    }
}

/// Sends QUERY and reads until DA1's reply or the limit; Some(light) once OSC 11 answered.
/// A duplicate descriptor reads the terminal without std's stdin buffer; it blocks, but each
/// read follows readiness, so it returns at once.
#[cfg(unix)]
async fn answer(
    input: impl std::os::fd::AsFd,
    output: &mut impl std::io::Write,
    limit: std::time::Duration,
) -> Option<bool> {
    use std::io::Read;
    let input = std::fs::File::from(input.as_fd().try_clone_to_owned().ok()?);
    let input = tokio::io::unix::AsyncFd::with_interest(input, tokio::io::Interest::READABLE).ok()?;
    output.write_all(QUERY).and_then(|()| output.flush()).ok()?;
    let (mut answers, mut light) = (Vec::new(), None);
    let _ = tokio::time::timeout(limit, async {
        let mut chunk = [0; 1024];
        while answers.len() < 4096 {
            let Ok(mut ready) = input.readable().await else { return };
            let Ok(read @ 1..) = ready.get_inner().read(&mut chunk) else {
                return;
            };
            ready.clear_ready();
            answers.extend_from_slice(&chunk[..read]);
            let (answer, ended) = scan(&answers);
            light = answer;
            if ended {
                return;
            }
        }
    })
    .await;
    light
}

/// The answers so far: OSC 11's background, if any, and whether DA1's reply has ended them.
#[cfg(unix)]
fn scan(answers: &[u8]) -> (Option<bool>, bool) {
    let mut light = None;
    let mut rest = answers;
    while let Some(escape) = rest.iter().position(|&byte| byte == 0x1b) {
        rest = &rest[escape + 1..];
        if let Some(body) = rest.strip_prefix(b"]") {
            // BEL or ST ends an OSC; another escape cuts it short, and Go's parser drops it.
            let Some(end) = body.iter().position(|&byte| matches!(byte, 0x07 | 0x1b)) else {
                break;
            };
            if body[end] == 0x1b && end + 1 == body.len() {
                break;
            }
            if (body[end] == 0x07 || body[end + 1] == b'\\')
                && let Some(color) = body[..end].strip_prefix(b"11;")
            {
                light = Some(bright(color));
            }
            rest = &body[end..];
        } else if let Some(body) = rest.strip_prefix(b"[") {
            let Some(end) = body.iter().position(|byte| (0x40..=0x7e).contains(byte)) else {
                break;
            };
            if body[0] == b'?' && body[end] == b'c' {
                return (light, true);
            }
            rest = &body[end + 1..];
        }
    }
    (light, false)
}

/// Go's IsDark: HSL lightness under one half is dark, and so is an unreadable color.
#[cfg(unix)]
fn bright(color: &[u8]) -> bool {
    std::str::from_utf8(color)
        .ok()
        .and_then(rgb)
        .is_some_and(|[red, green, blue]| {
            u16::from(red.max(green).max(blue)) + u16::from(red.min(green).min(blue)) >= 255
        })
}

/// Go's ansi.XParseColor for the forms terminals answer with: a 16-bit component keeps its
/// high byte and a malformed one reads as zero.
#[cfg(unix)]
fn rgb(color: &str) -> Option<[u8; 3]> {
    let hex = |digits: &str| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_hexdigit());
    if let Some(digits) = color.strip_prefix('#') {
        let width = if digits.len() == 3 { 1 } else { 2 };
        let mut channels = [0; 3];
        for (index, value) in channels.iter_mut().enumerate() {
            let pair = digits
                .get(index * width..(index + 1) * width)
                .filter(|pair| hex(pair))?;
            *value = u8::from_str_radix(pair, 16).ok()? * if width == 1 { 17 } else { 1 };
        }
        return Some(channels);
    }
    let (form, components) = color.split_once(':')?;
    let components: Vec<&str> = components.split('/').collect();
    if !matches!((form, components.len()), ("rgb", 3) | ("rgba", 4)) {
        return None;
    }
    let component = |part: &str| {
        let value = if hex(part) {
            u32::from_str_radix(part, 16).unwrap_or(u32::MAX)
        } else {
            0
        };
        (if value > 0xff { value >> 8 } else { value }) as u8
    };
    Some([
        component(components[0]),
        component(components[1]),
        component(components[2]),
    ])
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{io::Write, time::Duration};

    #[test]
    fn the_report_after_the_tui_keeps_the_terminal_answer() {
        let _ = ANSWER.set(true);
        assert_eq!(Theme::terminal().ink, Theme::background(Some(true)).ink);
    }

    #[test]
    fn backgrounds_read_light_or_dark_as_go_parses_them() {
        for (color, light) in [
            ("rgb:ffff/ffff/ffff", true),
            ("rgb:0000/0000/0000", false),
            ("rgb:8080/7f7f/7f7f", true),
            ("rgb:7f7f/7f7f/7f7f", false),
            ("rgba:fdfd/f6f6/e3e3/0000", true),
            ("rgb:ff/ff/ff", true),
            ("rgb:fff/fff/fff", false),
            ("rgb:ffff/ffff", false),
            ("#fff", true),
            ("#1d1f21", false),
            ("white", false),
        ] {
            assert_eq!(bright(color.as_bytes()), light, "{color}");
        }
    }

    #[test]
    fn device_attributes_end_the_answers_and_only_whole_background_answers_count() {
        for (answers, expected) in [
            (&b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1b[?62;22c"[..], (Some(true), true)),
            (b"\x1b[A\x1b]11;rgb:0000/0000/0000\x07\x1b[?6c", (Some(false), true)),
            (b"\x1b]10;rgb:ffff/ffff/ffff\x07\x1b[?6c", (None, true)),
            (b"\x1b]11;rgb:ffff/ffff/ffff\x1bx\x1b[?6c", (None, true)),
            (b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\", (Some(true), false)),
            (b"\x1b]11;rgb:ffff/ffff/ffff\x1b", (None, false)),
            (b"\x1b[?62;2", (None, false)),
        ] {
            assert_eq!(scan(answers), expected, "{}", answers.escape_ascii());
        }
    }

    #[tokio::test]
    async fn the_terminal_is_asked_once_and_read_until_device_attributes() {
        let (terminal, input) = std::os::unix::net::UnixStream::pair().unwrap();
        let answering = std::thread::spawn(move || {
            let mut terminal = terminal;
            for chunk in [&b"typed\x1b]11;rgb:fd"[..], b"fd/f6f6/e3e3\x1b", b"\\\x1b[?62;22c"] {
                std::thread::sleep(Duration::from_millis(20));
                terminal.write_all(chunk).unwrap();
            }
            terminal
        });
        let mut sent = Vec::new();
        let started = tokio::time::Instant::now();
        assert_eq!(answer(&input, &mut sent, Duration::from_secs(5)).await, Some(true));
        assert_eq!(sent, QUERY);
        let mut terminal = answering.join().unwrap();
        terminal.write_all(b"\x1b[?6c").unwrap();
        assert_eq!(answer(&input, &mut Vec::new(), Duration::from_secs(5)).await, None);
        assert!(started.elapsed() < Duration::from_secs(2));
        let silent = tokio::time::Instant::now();
        assert_eq!(answer(&input, &mut Vec::new(), Duration::from_millis(100)).await, None);
        assert!(silent.elapsed() >= Duration::from_millis(100));
    }
}
