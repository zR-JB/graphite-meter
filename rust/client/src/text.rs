//! Styled text for terminals: spans measured in cells, fitting, padding and wrapping, and one SGR writer per colour
//! profile.
use graphite_meter_proto::text::safe;
use std::io::{self, Write};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// A colour as each profile writes it: 24-bit, and the 256- and 16-colour indexes it converts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub rgb: u32,
    pub ansi256: u8,
    pub ansi: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
}

impl Style {
    pub const fn fg(color: Color) -> Self {
        Self { fg: Some(color), bg: None, bold: false }
    }

    pub const fn bold(self) -> Self {
        Self { bold: true, ..self }
    }
}

/// What a terminal shows of colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Profile {
    Plain,
    Ansi,
    Ansi256,
    TrueColor,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line(pub Vec<Span>);

impl Line {
    pub fn styled(text: impl Into<String>, style: Style) -> Self {
        Self(vec![Span { text: text.into(), style }])
    }

    pub fn plain(text: impl Into<String>) -> Self {
        Self::styled(text, Style::default())
    }

    /// The line with `text` added in `style`.
    pub fn and(mut self, text: impl Into<String>, style: Style) -> Self {
        self.0.push(Span { text: text.into(), style });
        self
    }

    /// The line with `other`'s spans added.
    pub fn with(mut self, other: Self) -> Self {
        self.0.extend(other.0);
        self
    }

    pub fn width(&self) -> usize {
        self.0.iter().map(|span| width(&span.text)).sum()
    }

    pub fn text(&self) -> String {
        self.0.iter().map(|span| span.text.as_str()).collect()
    }

    /// The line padded with spaces to `cells`.
    pub fn pad(self, cells: usize) -> Self {
        let fill = cells.saturating_sub(self.width());
        self.and(" ".repeat(fill), Style::default())
    }

    /// The line cut to `cells`, a longer one ending in `…` in the style it cuts.
    pub fn fit(self, cells: usize) -> Self {
        let cells = cells.max(1);
        if self.width() <= cells {
            return self;
        }
        let (mut room, mut spans) = (cells - 1, Vec::new());
        for span in self.0 {
            let mut kept = String::new();
            for c in span.text.chars() {
                let taken = c.width().unwrap_or(0);
                if taken > room {
                    spans.push(Span { text: kept + "…", style: span.style });
                    return Self(spans);
                }
                room -= taken;
                kept.push(c);
            }
            spans.push(Span { text: kept, style: span.style });
        }
        Self(spans)
    }

    /// The line without trailing spaces.
    pub fn trimmed(mut self) -> Self {
        while let Some(last) = self.0.last_mut() {
            last.text.truncate(last.text.trim_end_matches(' ').len());
            if !last.text.is_empty() {
                break;
            }
            self.0.pop();
        }
        self
    }
}

/// The cells `text` takes.
pub fn width(text: &str) -> usize {
    text.width()
}

/// `parts` joined by ` · ` while a line holds `cells`.
pub fn wrap(parts: &[String], cells: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for part in parts {
        let line = lines.last_mut().expect("one line at least");
        if line.is_empty() {
            line.clone_from(part);
        } else if width(line) + 3 + width(part) <= cells {
            line.push_str(" · ");
            line.push_str(part);
        } else {
            lines.push(part.clone());
        }
    }
    lines
}

/// `text`'s words filled into lines of at most `cells`; a longer word takes a line of its own.
pub fn fill(text: &str, cells: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if width(line) + 1 + width(word) <= cells => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_owned()),
        }
    }
    lines
}

/// Writes `lines` in `profile`, each styled span in one SGR sequence and every unsafe character blanked.
pub fn write(lines: &[Line], profile: Profile, out: &mut impl Write) -> io::Result<()> {
    for line in lines {
        for span in &line.0 {
            let text: String = span.text.chars().map(|c| if safe(c) { c } else { ' ' }).collect();
            match sgr(span.style, profile) {
                Some(codes) => write!(out, "\x1b[{codes}m{text}\x1b[m")?,
                None => out.write_all(text.as_bytes())?,
            }
        }
        out.write_all(b"\n")?;
    }
    out.flush()
}

/// The SGR parameters of `style` in `profile`; none for no style.
fn sgr(style: Style, profile: Profile) -> Option<String> {
    let color = |color: Option<Color>, base: u8| {
        let color = color?;
        Some(match profile {
            Profile::Plain => return None,
            Profile::Ansi => format!("{}", base + color.ansi % 8 + color.ansi / 8 * 60),
            Profile::Ansi256 => format!("{};5;{}", base + 8, color.ansi256),
            Profile::TrueColor => {
                let [_, red, green, blue] = color.rgb.to_be_bytes();
                format!("{};2;{red};{green};{blue}", base + 8)
            }
        })
    };
    let bold = (style.bold && profile != Profile::Plain).then(|| "1".to_owned());
    let codes: Vec<_> = [bold, color(style.fg, 30), color(style.bg, 40)]
        .into_iter()
        .flatten()
        .collect();
    (!codes.is_empty()).then(|| codes.join(";"))
}
