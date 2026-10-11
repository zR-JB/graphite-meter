//! The dial: a 270° ring of braille dots opening downward, on the browser's transfer curve and scale, with nine ticks
//! and five labels outside it and the readout inside it in figures drawn with heavy lines.
use super::{
    paint::{Canvas, Dots},
    theme::Palette,
};
use crate::text::{Color, Line};
use ratatui_core::layout::Rect;

/// A cell's width and height in pixels, as most monospace fonts draw them; the ring is round at this aspect.
const CELL: (f64, f64) = (9.0, 20.0);
/// The sweep starts at 225° and turns 270° clockwise.
const START: f64 = 225.0;
const TURN: f64 = 270.0;
/// Columns kept clear at each side for the labels.
const MARGIN: u16 = 6;
/// The ring's shares of its radius: an arc's band, the track's and a head's bead.
const BAND: f64 = 0.08;
const TRACK: f64 = 0.03;
const BEAD: f64 = 0.16;

/// One arc: how far it reaches, 0–1 of the sweep, in its colour.
pub struct Arc {
    pub to: f64,
    pub color: Color,
}

pub struct Dial {
    /// Each drawn to its end, the shorter over the longer so every head shows.
    pub arcs: Vec<Arc>,
    pub track: Color,
    pub ticks: [String; 5],
    /// Over the figures: the stage's mark and name.
    pub label: Line,
    pub value: String,
    pub unit: String,
    /// Under the ring.
    pub note: Line,
}

/// The smallest area a ring is drawn in; a smaller dial shows only its readout.
pub const SMALLEST: (u16, u16) = (40, 14);

/// Draws `dial` across `area`: the ring when it fits, else the readout alone.
pub fn draw(canvas: &mut Canvas, area: Rect, palette: &Palette, dial: &Dial) {
    if area.width < SMALLEST.0 || area.height < SMALLEST.1 {
        return readout(canvas, area, palette, dial);
    }
    let (w, h) = (f64::from(area.width - 2 * MARGIN) * CELL.0, f64::from(area.height - 3) * CELL.1);
    let radius = (w / 2.0 - 8.0).min((h - 12.0) / (1.0 + std::f64::consts::FRAC_1_SQRT_2));
    // The label over the crown, the ring, and the note two rows under its ends stand centred in the area.
    let tall = CELL.1 + radius * (1.0 + std::f64::consts::FRAC_1_SQRT_2) + 16.0 + 2.0 * CELL.1;
    let top = ((f64::from(area.height) * CELL.1 - tall) / 2.0 / CELL.1)
        .floor()
        .max(0.0)
        * CELL.1;
    let (cx, cy) = (f64::from(area.width) * CELL.0 / 2.0, top + CELL.1 + radius + 8.0);
    let point = |fraction: f64, reach: f64| {
        let angle = (START - fraction * TURN).to_radians();
        (cx + reach * angle.cos(), cy - reach * angle.sin())
    };
    let (band, track, bead) = ((radius * BAND).max(9.0), (radius * TRACK).max(4.5), (radius * BEAD).max(14.0));
    let mut arcs: Vec<_> = dial
        .arcs
        .iter()
        .map(|arc| (arc.to.clamp(0.0, 1.0), arc.color))
        .collect();
    arcs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let heads: Vec<_> = arcs.iter().map(|&(to, color)| (point(to, radius), color)).collect();
    let ticks: Vec<_> = (0..9)
        .map(|tick| (START - f64::from(tick) / 8.0 * TURN).to_radians().sin_cos())
        .collect();
    let mut dots = Dots::new(area);
    let (across, down) = dots.size();
    for y in 0..down {
        for x in 0..across {
            let (px, py) = ((x as f64 + 0.5) * CELL.0 / 2.0, (y as f64 + 0.5) * CELL.1 / 4.0);
            let (dx, dy) = (px - cx, cy - py);
            let distance = dx.hypot(dy) - radius;
            if distance.abs() > bead.max(radius * 0.2) {
                continue;
            }
            let fraction = (START - dy.atan2(dx).to_degrees()).rem_euclid(360.0) / TURN;
            let head = heads
                .iter()
                .find(|((hx, hy), _)| (px - hx).hypot(py - hy) <= bead / 2.0);
            let arc = arcs
                .iter()
                .find(|&&(to, _)| fraction <= to && distance.abs() <= band / 2.0);
            let tick = ticks.iter().position(|&(sin, cos)| {
                let (along, across) = (dx * cos + dy * sin, (dy * cos - dx * sin).abs());
                let from = radius + band / 2.0 + 4.0;
                (from..=from + radius * 0.07).contains(&along) && across <= 2.4
            });
            let lit = match (head, arc, tick) {
                (Some(&(_, color)), ..) => Some((color, 4)),
                (_, Some(&(_, color)), _) => Some((color, 3)),
                _ if fraction <= 1.0 && distance.abs() <= track / 2.0 => Some((dial.track, 1)),
                (.., Some(tick)) => {
                    let style = if tick % 2 == 0 { palette.muted } else { palette.border };
                    style.fg.map(|color| (color, 2))
                }
                _ => None,
            };
            if let Some((color, rank)) = lit {
                dots.set(x as isize, y as isize, color, rank);
            }
        }
    }
    canvas.dots(&dots);
    // The labels sit beyond the ticks: the half's over the crown, the others beside their ticks.
    for (index, tick) in dial.ticks.iter().enumerate() {
        let (lx, ly) = point(index as f64 / 4.0, radius + band / 2.0 + radius * 0.17 + 4.0);
        let text = Line::styled(tick, palette.muted);
        let width = text.width() as f64;
        let column = match index {
            2 => lx / CELL.0 - width / 2.0,
            _ if lx < cx => lx / CELL.0 - width + 0.5,
            _ => lx / CELL.0 - 0.5,
        };
        let row = (ly / CELL.1).floor().clamp(0.0, f64::from(area.height - 2));
        let column = column.round().clamp(0.0, f64::from(area.width) - width);
        canvas.line(area.x + column as u16, area.y + row as u16, width as u16, &text);
    }
    let middle = area.y + (cy / CELL.1) as u16;
    stack(canvas, Rect::new(area.x, middle.saturating_sub(3), area.width, 7), palette, dial);
    let under = ((cy + radius * std::f64::consts::FRAC_1_SQRT_2) / CELL.1).ceil() as u16 + 1;
    canvas.center(area.x, (area.y + under).min(area.bottom() - 1), area.width, &dial.note);
}

/// The readout without its ring, centred in `area`: the label, the figures, the unit and the note.
pub fn readout(canvas: &mut Canvas, area: Rect, palette: &Palette, dial: &Dial) {
    let top = area.y + area.height.saturating_sub(7) / 2;
    stack(canvas, Rect::new(area.x, top, area.width, 7), palette, dial);
    canvas.center(area.x, (top + 7).min(area.bottom() - 1), area.width, &dial.note);
}

/// The label, the figures three rows tall and the unit, in seven rows.
fn stack(canvas: &mut Canvas, area: Rect, palette: &Palette, dial: &Dial) {
    canvas.center(area.x, area.y, area.width, &dial.label);
    let rows = figures(&dial.value);
    let width = rows.iter().map(|row| crate::text::width(row)).max().unwrap_or(0) as u16;
    let style = if dial.value == crate::report::vocabulary::MISSING {
        palette.muted
    } else {
        palette.text
    };
    for (index, row) in rows.iter().enumerate() {
        let x = area.x + area.width.saturating_sub(width) / 2;
        canvas.line(x, area.y + 2 + index as u16, width, &Line::styled(row, style));
    }
    canvas.center(area.x, area.y + 6, area.width, &Line::styled(&dial.unit, palette.muted));
}

/// `text` in figures three rows tall drawn with heavy lines, a cell between them; characters without one are skipped.
pub fn figures(text: &str) -> [String; 3] {
    let glyph = |c| -> Option<[&str; 3]> {
        Some(match c {
            '0' => ["┏━┓", "┃ ┃", "┗━┛"],
            '1' => [" ┓ ", " ┃ ", " ┻ "],
            '2' => ["╺━┓", "┏━┛", "┗━╸"],
            '3' => ["╺━┓", " ━┫", "╺━┛"],
            '4' => ["╻ ╻", "┗━┫", "  ╹"],
            '5' => ["┏━╸", "┗━┓", "╺━┛"],
            '6' => ["┏━╸", "┣━┓", "┗━┛"],
            '7' => ["╺━┓", "  ┃", "  ╹"],
            '8' => ["┏━┓", "┣━┫", "┗━┛"],
            '9' => ["┏━┓", "┗━┫", "╺━┛"],
            '.' => [" ", " ", "•"],
            '<' => [" ", "<", " "],
            '—' | '-' => ["   ", "╺━╸", "   "],
            _ => return None,
        })
    };
    let mut rows = [String::new(), String::new(), String::new()];
    let mut previous = None;
    for (c, glyph) in text.chars().filter_map(|c| Some((c, glyph(c)?))) {
        // The point hugs both its neighbours.
        let gap = previous.is_some() && previous != Some('.') && c != '.';
        for (row, part) in rows.iter_mut().zip(glyph) {
            if gap {
                row.push(' ');
            }
            row.push_str(part);
        }
        previous = Some(c);
    }
    rows
}
