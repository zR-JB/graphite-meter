//! Draws into a frame's buffer and records the regions a pointer can press.
use super::Target;
use crate::text::{Color, Line, Profile, Style};
use graphite_meter_proto::text::safe;
use ratatui_core::{
    buffer::Buffer,
    layout::Rect,
    style::{Color as Paint, Modifier},
};
use std::borrow::Cow;

const BITS: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

pub struct Canvas<'a> {
    buffer: &'a mut Buffer,
    profile: Profile,
    pub hits: Vec<(Rect, Target)>,
}

impl<'a> Canvas<'a> {
    pub fn new(buffer: &'a mut Buffer, profile: Profile) -> Self {
        Self {
            buffer,
            profile,
            hits: Vec::new(),
        }
    }

    pub fn area(&self) -> Rect {
        self.buffer.area
    }

    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// Fills `area` with a tile's color from 256 colors on. With fewer the canvas stays flat.
    pub fn fill(&mut self, area: Rect, color: Color) {
        if self.profile >= Profile::Ansi256 {
            self.plate(
                area,
                Style {
                    bg: Some(color),
                    ..Style::default()
                },
            );
        }
    }

    /// Paints `area` in `style`'s background in every profile that has color, as a key's plate.
    pub fn plate(&mut self, area: Rect, style: Style) {
        let Some(bg) = paint(style.bg, self.profile) else {
            return;
        };
        for (x, y) in cells(area.intersection(self.buffer.area)) {
            self.buffer[(x, y)].set_bg(bg);
        }
    }

    pub fn clear(&mut self, area: Rect) {
        for (x, y) in cells(area.intersection(self.buffer.area)) {
            self.buffer[(x, y)].reset();
        }
    }

    /// Writes `line` at `x`, `y` in at most `width` cells, unsafe characters blanked, and returns the column after it.
    pub fn line(&mut self, x: u16, y: u16, width: u16, line: &Line) -> u16 {
        let area = self.buffer.area;
        if y < area.y || y >= area.bottom() || x >= area.right() {
            return x;
        }
        let end = x.saturating_add(width).min(area.right());
        let mut at = x;
        for span in &line.0 {
            if at >= end {
                break;
            }
            let text: Cow<'_, str> = match span.text.chars().all(safe) {
                true => Cow::Borrowed(&span.text),
                false => Cow::Owned(
                    span.text
                        .chars()
                        .map(|c| if safe(c) { c } else { ' ' })
                        .collect(),
                ),
            };
            let style = self.painted(span.style);
            (at, _) = self
                .buffer
                .set_stringn(at, y, text, usize::from(end - at), style);
        }
        at
    }

    pub fn right(&mut self, x: u16, y: u16, width: u16, line: &Line) {
        let shown = u16::try_from(line.width()).unwrap_or(u16::MAX).min(width);
        self.line(x + width - shown, y, shown, line);
    }

    pub fn center(&mut self, x: u16, y: u16, width: u16, line: &Line) {
        let shown = u16::try_from(line.width()).unwrap_or(u16::MAX).min(width);
        self.line(x + (width - shown) / 2, y, shown, line);
    }

    /// Writes the cells `dots` lit as braille in their colors, over the backgrounds under them.
    pub fn dots(&mut self, dots: &Dots) {
        let area = dots.area.intersection(self.buffer.area);
        for (x, y) in cells(area) {
            let (mask, color, _) = dots.cells[usize::from(y - dots.area.y)
                * usize::from(dots.area.width)
                + usize::from(x - dots.area.x)];
            if mask == 0 {
                continue;
            }
            let cell = &mut self.buffer[(x, y)];
            cell.set_char(char::from_u32(0x2800 + u32::from(mask)).unwrap_or(' '));
            cell.fg = paint(color, self.profile).unwrap_or(Paint::Reset);
            cell.modifier = Modifier::empty();
        }
    }

    pub fn hit(&mut self, area: Rect, target: Target) {
        self.hits.push((area, target));
    }

    /// Dims the backdrop under a sheet, mixing `area` toward `canvas` by `share`, unstyled text taking `soft`.
    pub fn dim(&mut self, area: Rect, canvas: Color, soft: Color, share: f64) {
        if self.profile != Profile::TrueColor {
            return;
        }
        let rgb = |color: Color| {
            let [_, red, green, blue] = color.rgb.to_be_bytes();
            (red, green, blue)
        };
        let ((red, green, blue), soft) = (rgb(canvas), rgb(soft));
        let toward = |color: Paint| match color {
            Paint::Rgb(r, g, b) => {
                let blend = |own: u8, base: u8| {
                    (f64::from(own) * (1.0 - share) + f64::from(base) * share).round() as u8
                };
                Paint::Rgb(blend(r, red), blend(g, green), blend(b, blue))
            }
            other => other,
        };
        for (x, y) in cells(area.intersection(self.buffer.area)) {
            let cell = &mut self.buffer[(x, y)];
            if cell.fg == Paint::Reset {
                cell.fg = Paint::Rgb(soft.0, soft.1, soft.2);
            }
            (cell.fg, cell.bg) = (toward(cell.fg), toward(cell.bg));
            cell.modifier.remove(Modifier::BOLD);
        }
    }

    /// Paints `style` as this profile does. A span without a foreground takes the terminal's, bold only when asked.
    fn painted(&self, style: Style) -> ratatui_core::style::Style {
        let mut painted = ratatui_core::style::Style {
            fg: Some(paint(style.fg, self.profile).unwrap_or(Paint::Reset)),
            bg: paint(style.bg, self.profile),
            ..Default::default()
        };
        painted = match style.bold && self.profile != Profile::Plain {
            true => painted.add_modifier(Modifier::BOLD),
            false => painted.remove_modifier(Modifier::BOLD),
        };
        painted
    }
}

/// Writes `color` as `profile` does. The 16-color profile writes indexed colors 0 to 15.
pub fn paint(color: Option<Color>, profile: Profile) -> Option<Paint> {
    let color = color?;
    let [_, red, green, blue] = color.rgb.to_be_bytes();
    match profile {
        Profile::Plain | Profile::Ascii => None,
        Profile::Ansi => Some(Paint::Indexed(color.ansi)),
        Profile::Ansi256 => Some(Paint::Indexed(color.ansi256)),
        Profile::TrueColor => Some(Paint::Rgb(red, green, blue)),
    }
}

fn cells(area: Rect) -> impl Iterator<Item = (u16, u16)> {
    (area.y..area.bottom()).flat_map(move |y| (area.x..area.right()).map(move |x| (x, y)))
}

/// Braille dots, two across and four down a cell; a cell shows its highest rank's dots, so a mark over a trace stays whole.
pub struct Dots {
    area: Rect,
    cells: Vec<(u8, Option<Color>, u8)>,
}

impl Dots {
    pub fn new(area: Rect) -> Self {
        Self {
            area,
            cells: vec![(0, None, 0); usize::from(area.width) * usize::from(area.height)],
        }
    }

    pub fn size(&self) -> (usize, usize) {
        (
            usize::from(self.area.width) * 2,
            usize::from(self.area.height) * 4,
        )
    }

    /// Lights the dot `x` across and `y` down in `color` at `rank`. One off the area is dropped.
    pub fn set(&mut self, x: isize, y: isize, color: Color, rank: u8) {
        let (width, height) = self.size();
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return;
        };
        if x >= width || y >= height {
            return;
        }
        let cell = &mut self.cells[y / 4 * usize::from(self.area.width) + x / 2];
        if rank < cell.2 && cell.0 != 0 {
            return;
        }
        if rank > cell.2 || cell.0 == 0 {
            *cell = (0, Some(color), rank);
        }
        cell.0 |= BITS[y % 4][x % 2];
    }
}
