//! Go's TUI palette (the web tokens), with the indexed and ANSI colors Go's color profiles convert each to.

use crate::model::Stage;
use ratatui::style::Color;

#[derive(Clone, Copy)]
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

impl Theme {
    pub fn terminal() -> Self {
        if std::env::var_os("NO_COLOR").is_some() || std::env::var("TERM").is_ok_and(|term| term == "dumb") {
            return Self::monochrome();
        }
        let depth = Depth::terminal();
        let light = match std::env::var("GM_TUI_THEME").ok().as_deref() {
            Some("light") => true,
            Some("dark") => false,
            _ => std::env::var("COLORFGBG")
                .ok()
                .and_then(|value| value.rsplit(';').next()?.parse::<u8>().ok())
                .is_some_and(|background| matches!(background, 7 | 9..=15)),
        };
        if light { Self::light(depth) } else { Self::dark(depth) }
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

    const fn monochrome() -> Self {
        Self {
            ink: Color::Reset,
            text: Color::Reset,
            muted: Color::Reset,
            inverse: Color::Reset,
            surface: Color::Reset,
            border: Color::Reset,
            ok: Color::Reset,
            warn: Color::Reset,
            err: Color::Reset,
            stages: [Color::Reset; 4],
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
