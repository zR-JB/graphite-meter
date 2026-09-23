//! Graphite Meter's carbon palette, adapted to the terminal's color depth.

use ratatui::style::Color;

#[derive(Clone, Copy)]
pub(crate) struct Theme {
    pub text: Color,
    pub muted: Color,
    pub inverse: Color,
    pub brand: Color,
    pub brand_strong: Color,
    pub surface: Color,
    pub border: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
}

#[derive(Clone, Copy)]
enum Depth {
    TrueColor,
    Indexed,
    Ansi,
}

impl Theme {
    pub fn terminal() -> Self {
        if std::env::var_os("NO_COLOR").is_some()
            || std::env::var("TERM").is_ok_and(|term| term == "dumb")
        {
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
        if light {
            Self::light(depth)
        } else {
            Self::dark(depth)
        }
    }

    fn dark(depth: Depth) -> Self {
        Self {
            text: tone(0xd9dce0, 253, Color::White, depth),
            muted: tone(0x9ba2aa, 247, Color::Gray, depth),
            inverse: tone(0x111315, 233, Color::Black, depth),
            brand: tone(0x6db0b8, 73, Color::LightCyan, depth),
            brand_strong: tone(0x93cdd4, 116, Color::LightCyan, depth),
            surface: tone(0x23262b, 235, Color::Black, depth),
            border: tone(0x3d4044, 238, Color::DarkGray, depth),
            success: tone(0x79ad91, 108, Color::LightGreen, depth),
            warning: tone(0xc4a568, 179, Color::LightYellow, depth),
            error: tone(0xd89393, 174, Color::LightRed, depth),
        }
    }

    fn light(depth: Depth) -> Self {
        Self {
            text: tone(0x26272a, 235, Color::Black, depth),
            muted: tone(0x454a4d, 239, Color::DarkGray, depth),
            inverse: tone(0xf6f5f1, 255, Color::White, depth),
            brand: tone(0x2f717a, 23, Color::Cyan, depth),
            brand_strong: tone(0x235257, 23, Color::Cyan, depth),
            surface: tone(0xeaeae4, 254, Color::Gray, depth),
            border: tone(0xc3c3bf, 251, Color::Gray, depth),
            success: tone(0x285443, 22, Color::Green, depth),
            warning: tone(0x6f5426, 58, Color::Yellow, depth),
            error: tone(0xa04a4a, 95, Color::Red, depth),
        }
    }

    const fn monochrome() -> Self {
        Self {
            text: Color::Reset,
            muted: Color::Reset,
            inverse: Color::Reset,
            brand: Color::Reset,
            brand_strong: Color::Reset,
            surface: Color::Reset,
            border: Color::Reset,
            success: Color::Reset,
            warning: Color::Reset,
            error: Color::Reset,
        }
    }
}

impl Depth {
    fn terminal() -> Self {
        let term = std::env::var("TERM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let color_term = std::env::var("COLORTERM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(color_term.as_str(), "truecolor" | "24bit")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_uses_go_fallbacks_at_each_depth() {
        assert_eq!(
            Theme::dark(Depth::TrueColor).brand,
            Color::Rgb(109, 176, 184)
        );
        assert_eq!(Theme::dark(Depth::Indexed).brand, Color::Indexed(73));
        assert_eq!(Theme::light(Depth::Ansi).brand, Color::Cyan);
    }
}
