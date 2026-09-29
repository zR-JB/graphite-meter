//! Go's TUI styles (theme.go) over the graphite palette, in the colour profile Go's
//! colorprofile.Detect picks, with the indexed and ANSI colours it converts each tone to.

use crate::model::Stage;
use ratatui::style::{Color, Modifier, Style};

/// colorprofile's profiles, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Profile {
    NoTty,
    Ascii,
    Ansi,
    Ansi256,
    TrueColor,
}

/// Go's styles. The default has no style at all, as colorprofile's NoTTY strips them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Theme {
    pub title: Style,
    pub pill: Style,
    pub selected: Style,
    pub text: Style,
    pub value: Style,
    pub muted: Style,
    pub accent: Style,
    pub ok: Style,
    pub warn: Style,
    pub err: Style,
    pub border: Style,
    pub heading: Style,
    /// textinput's static cursor: ANSI 7, reversed.
    pub cursor: Style,
    stages: [Style; 4],
    /// The complete, caution and failed badges.
    outcomes: [Style; 3],
}

/// A tone's light and dark shades: 24-bit, then as colorprofile converts them to 256 and 16 colours.
type Tone = [(u32, u8, Color); 2];

const INK: Tone = [(0x20242a, 235, Color::Black), (0xe6e8ea, 254, Color::White)];
const TEXT: Tone = [(0x171b20, 234, Color::Black), (0xeef0f3, 255, Color::White)];
const SOFT: Tone = [(0x5f646a, 241, Color::DarkGray), (0x8e9299, 246, Color::Gray)];
const GOOD: Tone = [(0x2e734b, 29, Color::Green), (0x88d1a2, 115, Color::LightGreen)];
const CAUTION: Tone = [(0x85671f, 94, Color::Red), (0xe8cf83, 186, Color::LightYellow)];
const BAD: Tone = [(0xab413e, 131, Color::Red), (0xed8b88, 210, Color::LightRed)];
const BADGE: Tone = [(0xfdfdfd, 231, Color::White), (0x0d1013, 233, Color::Black)];
const BORDER: Tone = [(0xcacbcf, 252, Color::White), (0x3e4348, 238, Color::DarkGray)];
const SELECTED: Tone = [(0xe6e6e9, 254, Color::White), (0x303236, 236, Color::Black)];
const STAGES: [Tone; 4] = [
    [(0x1d7a73, 30, Color::Cyan), (0x70dbc4, 80, Color::LightCyan)],
    [(0x254ea3, 25, Color::Blue), (0x71a3ff, 75, Color::LightBlue)],
    [(0xa35d1d, 130, Color::Red), (0xfeb66a, 215, Color::LightRed)],
    [(0x7f2456, 89, Color::Red), (0xe472ac, 169, Color::LightRed)],
];

/// Go's TUI asks for the background with OSC 11. DA1 follows, as in lipgloss's query: every
/// terminal answers it, and in order, so its reply ends the wait.
#[cfg(unix)]
const QUERY: &[u8] = b"\x1b]11;?\x1b\\\x1b[c";

/// The terminal's answer to the TUI's query, so the report printed after it uses the same palette.
static DARK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

impl Theme {
    /// Go's newStyles in `profile`.
    pub fn new(profile: Profile, dark: bool) -> Self {
        if profile == Profile::NoTty {
            return Self::default();
        }
        let shade = usize::from(dark);
        let color = |tone: Tone| {
            let (rgb, indexed, ansi) = tone[shade];
            match profile {
                Profile::TrueColor => Some(Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)),
                Profile::Ansi256 => Some(Color::Indexed(indexed)),
                Profile::Ansi => Some(ansi),
                _ => None,
            }
        };
        let fg = |tone: Tone| Style {
            fg: color(tone),
            ..Style::new()
        };
        let badge = fg(BADGE).add_modifier(Modifier::BOLD);
        let on = |tone: Tone| Style {
            bg: color(tone),
            ..badge
        };
        let text = fg(TEXT);
        let accent = fg(INK);
        Self {
            title: on(INK),
            pill: on(SOFT),
            selected: Style {
                bg: color(SELECTED),
                ..text.add_modifier(Modifier::BOLD)
            },
            text,
            value: text.add_modifier(Modifier::BOLD),
            muted: fg(SOFT),
            accent,
            ok: fg(GOOD),
            warn: fg(CAUTION),
            err: fg(BAD).add_modifier(Modifier::BOLD),
            border: fg(BORDER),
            heading: accent.add_modifier(Modifier::BOLD),
            cursor: Style {
                fg: (profile > Profile::Ascii).then_some(Color::Gray),
                ..Style::new().add_modifier(Modifier::REVERSED)
            },
            stages: STAGES.map(fg),
            outcomes: [on(GOOD), on(CAUTION), on(BAD)],
        }
    }

    /// The styles for stdout, in the background the terminal's answer gave, dark without one.
    pub fn terminal() -> Self {
        use std::io::IsTerminal;
        Self::new(
            detect(std::io::stdout().is_terminal()),
            DARK.get().copied().unwrap_or(true),
        )
    }

    /// Asks the terminal for its background, then gives the TUI's styles. Call it in raw mode only.
    pub async fn ask(limit: std::time::Duration) -> Self {
        #[cfg(unix)]
        if let Some(dark) = query(limit).await {
            let _ = DARK.set(dark);
        }
        #[cfg(not(unix))]
        let _ = limit; // Windows consoles would deliver the answer as key events.
        Self::terminal()
    }

    pub fn stage(&self, stage: Stage) -> Style {
        self.stages[stage as usize]
    }

    /// The badge of a run's outcome.
    pub fn outcome(&self, phase: crate::model::Phase) -> Style {
        use crate::model::Phase;
        self.outcomes[match phase {
            Phase::Complete => 0,
            Phase::Partial | Phase::Incomplete | Phase::Cancelled => 1,
            _ => 2,
        }]
    }
}

/// Go's strconv.ParseBool of an environment variable.
fn flag(name: &str) -> bool {
    matches!(
        std::env::var(name).as_deref(),
        Ok("1" | "t" | "T" | "true" | "TRUE" | "True")
    )
}

/// colorprofile.Detect for stdout: TERM, COLORTERM, NO_COLOR, CLICOLOR(_FORCE) and TTY_FORCE.
/// Its terminfo lookup reads a named terminal as ANSI, and tmux as 256 colours, as when neither
/// reports Tc or RGB.
pub(crate) fn detect(tty: bool) -> Profile {
    let (tty, term) = (tty || flag("TTY_FORCE"), std::env::var("TERM").ok());
    let env = environment(term.as_deref().unwrap_or_default());
    // colorProfile's dumb terminal: TERM=dumb, or none outside Windows.
    let dumb = term.as_deref().map_or(!cfg!(windows), |term| term == "dumb");
    let mut profile = if tty && !dumb { env } else { Profile::NoTty };
    if flag("NO_COLOR") && tty {
        return profile.min(Profile::Ascii);
    }
    if flag("CLICOLOR_FORCE") {
        return profile.max(Profile::Ansi).max(env);
    }
    if flag("CLICOLOR") && tty && !dumb {
        profile = profile.max(Profile::Ansi);
    }
    let named = term.as_deref().filter(|term| *term != "dumb");
    let Some(term) = named.filter(|_| tty && profile != Profile::TrueColor && !flag("NO_COLOR")) else {
        return profile;
    };
    let terminfo = if term.is_empty() { Profile::NoTty } else { Profile::Ansi };
    let tmux = std::env::var_os("TMUX").is_some_and(|tmux| !tmux.is_empty());
    profile
        .max(terminfo)
        .max(if tmux { Profile::Ansi256 } else { Profile::NoTty })
}

/// colorprofile's envColorProfile.
fn environment(term: &str) -> Profile {
    // Windows Terminal and cmd.exe render 24-bit colour without a TERM.
    let mut profile = match term {
        "" | "dumb" if cfg!(windows) => Profile::TrueColor,
        "" | "dumb" => Profile::NoTty,
        _ => Profile::Ansi,
    };
    let multiplexer = term.starts_with("tmux") || term.starts_with("screen");
    if multiplexer {
        profile = profile.max(Profile::Ansi256);
    }
    let color_term = std::env::var("COLORTERM").unwrap_or_default().to_ascii_lowercase();
    let direct = ["truecolor", "24bit", "yes", "true"].contains(&color_term.as_str()) && !multiplexer;
    let windows_terminal = std::env::var_os("WT_SESSION").is_some_and(|session| !session.is_empty());
    let named = "alacritty contour foot ghostty kitty rio st wezterm"
        .split(' ')
        .any(|name| term.contains(name));
    if named || windows_terminal || flag("GOOGLE_CLOUD_SHELL") || direct || term.ends_with("direct") {
        return Profile::TrueColor;
    }
    match term.ends_with("256color") {
        true => profile.max(Profile::Ansi256),
        false => profile,
    }
}

/// Sends QUERY where the keys are read (stdin, or the terminal device when it is redirected) and
/// reads until DA1's reply or the limit: Some(dark) once OSC 11 answered.
#[cfg(unix)]
async fn query(limit: std::time::Duration) -> Option<bool> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        answer(std::io::stdin(), &mut std::io::stdout(), limit).await
    } else {
        let terminal = std::fs::File::open("/dev/tty").ok()?;
        answer(&terminal, &mut std::io::stdout(), limit).await
    }
}

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
    let (mut answers, mut dark) = (Vec::new(), None);
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
            dark = answer;
            if ended {
                return;
            }
        }
    })
    .await;
    dark
}

/// The answers so far: OSC 11's background, if any, and whether DA1's reply has ended them.
#[cfg(unix)]
fn scan(answers: &[u8]) -> (Option<bool>, bool) {
    let mut dark = None;
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
    Some([0, 1, 2].map(|index| component(components[index])))
}

#[cfg(unix)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, time::Duration};

    #[test]
    fn profiles_strip_what_go_strips() {
        let dark = Theme::new(Profile::TrueColor, true);
        assert_eq!(dark.title.bg, Some(Color::Rgb(0xe6, 0xe8, 0xea)));
        assert_eq!(dark.title.fg, Some(Color::Rgb(0x0d, 0x10, 0x13)));
        assert_eq!(Theme::new(Profile::Ansi256, false).accent.fg, Some(Color::Indexed(235)));
        let ascii = Theme::new(Profile::Ascii, true);
        assert_eq!(ascii.value, Style::new().add_modifier(Modifier::BOLD));
        assert_eq!(ascii.border, Style::new());
        assert_eq!(Theme::new(Profile::NoTty, true), Theme::default());
    }

    #[test]
    fn terms_pick_go_colour_profiles() {
        for (term, profile) in [
            ("xterm-kitty", Profile::TrueColor),
            ("wezterm", Profile::TrueColor),
            ("screen", Profile::Ansi256),
            ("tmux-256color", Profile::Ansi256),
            ("xterm-256color", Profile::Ansi256),
            ("xterm", Profile::Ansi),
            ("vt100", Profile::Ansi),
            ("xterm-direct", Profile::TrueColor),
            ("dumb", Profile::NoTty),
            ("", Profile::NoTty),
        ] {
            assert_eq!(environment(term), profile, "{term}");
        }
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
            (
                &b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1b[?62;22c"[..],
                (Some(false), true),
            ),
            (b"\x1b[A\x1b]11;rgb:0000/0000/0000\x07\x1b[?6c", (Some(true), true)),
            (b"\x1b]10;rgb:ffff/ffff/ffff\x07\x1b[?6c", (None, true)),
            (b"\x1b]11;rgb:ffff/ffff/ffff\x1bx\x1b[?6c", (None, true)),
            (b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\", (Some(false), false)),
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
        assert_eq!(answer(&input, &mut sent, Duration::from_secs(5)).await, Some(false));
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
