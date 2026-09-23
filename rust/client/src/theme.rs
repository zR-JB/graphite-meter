//! Graphite Meter's carbon palette in Ratatui colors.

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

impl Theme {
    pub fn terminal() -> Self {
        if std::env::var_os("NO_COLOR").is_some() {
            return Self::monochrome();
        }
        match std::env::var("GM_TUI_THEME").ok().as_deref() {
            Some("light") => return Self::light(),
            Some("dark") => return Self::dark(),
            _ => {}
        }
        let light_background = std::env::var("COLORFGBG")
            .ok()
            .and_then(|value| value.rsplit(';').next()?.parse::<u8>().ok())
            .is_some_and(|background| matches!(background, 7 | 9..=15));
        if light_background {
            Self::light()
        } else {
            Self::dark()
        }
    }

    const fn dark() -> Self {
        Self {
            text: rgb(0xd9dce0),
            muted: rgb(0x9ba2aa),
            inverse: rgb(0x111315),
            brand: rgb(0x6db0b8),
            brand_strong: rgb(0x93cdd4),
            surface: rgb(0x23262b),
            border: rgb(0x3d4044),
            success: rgb(0x79ad91),
            warning: rgb(0xc4a568),
            error: rgb(0xd89393),
        }
    }

    const fn light() -> Self {
        Self {
            text: rgb(0x26272a),
            muted: rgb(0x454a4d),
            inverse: rgb(0xf6f5f1),
            brand: rgb(0x2f717a),
            brand_strong: rgb(0x235257),
            surface: rgb(0xeaeae4),
            border: rgb(0xc3c3bf),
            success: rgb(0x285443),
            warning: rgb(0x6f5426),
            error: rgb(0xa04a4a),
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

const fn rgb(value: u32) -> Color {
    Color::Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}
