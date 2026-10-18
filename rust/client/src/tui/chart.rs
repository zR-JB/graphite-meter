//! Draws a stage's traces in braille: the rate as a line, the replies as one bar a cell, fastest to slowest.
use super::paint::{Canvas, Dots};
use crate::{
    events::Point,
    text::{Color, Line, Style},
};
use ratatui_core::layout::Rect;

/// Draws `points` over `window` seconds as a line up to `head`, the live value now, its tip in `tip` and the area
/// under it fading into `field`. A gap breaks it.
pub fn line(
    canvas: &mut Canvas,
    area: Rect,
    points: &[Point],
    head: Option<(f64, f64)>,
    (top, window): (f64, (f64, f64)),
    (color, tip, field): (Color, Color, Option<Color>),
) {
    let mut dots = Dots::new(area);
    let (across, down) = dots.size();
    let span = (window.1 - window.0).max(f64::EPSILON);
    let height = |value: f64| {
        let share = (value / top).clamp(0.0, 1.0);
        (down - 1) as isize - (share * (down - 1) as f64).round() as isize
    };
    let last = points
        .last()
        .map_or(f64::NEG_INFINITY, |point| point.at.as_secs_f64());
    let head = head.filter(|&(at, _)| at > last);
    let (mut next, mut previous, mut tipped) = (0, None::<isize>, None);
    // Tracks each cell column's highest dot of the line.
    let mut crests = vec![None::<isize>; usize::from(area.width)];
    for x in 0..across {
        let at = window.0 + (x as f64 + 0.5) / across as f64 * span;
        while next < points.len() && points[next].at.as_secs_f64() <= at {
            next += 1;
        }
        let before = next.checked_sub(1).map(|index| &points[index]);
        let value = match (before, points.get(next)) {
            (Some(before), _) if before.value.is_none() => None,
            (Some(before), Some(after)) => {
                Some(between(before, after.at.as_secs_f64(), after.value, at))
            }
            (Some(before), None) => match head {
                Some((head_at, head_value)) if at <= head_at => {
                    Some(between(before, head_at, Some(head_value), at))
                }
                _ if at <= last + span / across as f64 => before.value,
                _ => None,
            },
            (None, _) => None,
        };
        let Some(value) = value else {
            previous = None;
            continue;
        };
        let y = height(value);
        let crest = &mut crests[x / 2];
        *crest = Some(crest.map_or(y, |crest| crest.min(y)));
        // A steep step fills the column between, so the line stays whole.
        let from = previous.map_or(y, |previous| {
            if (previous - y).abs() > 1 {
                previous
            } else {
                y
            }
        });
        for dot in from.min(y)..=from.max(y) {
            dots.set(x as isize, dot, color, 1);
        }
        (previous, tipped) = (Some(y), Some((x, y)));
    }
    if let Some((x, y)) = tipped.filter(|_| head.is_some()) {
        dots.set(x as isize, y, tip, 2);
    }
    if let Some(field) = field {
        // The area starts in the line's own cell when the line runs in its upper half, else in the cell under it.
        let rows = area.height;
        for (column, crest) in crests.iter().enumerate() {
            let Some(crest) = *crest else { continue };
            let first = (crest / 4 + isize::from(crest % 4 > 1)) as u16;
            for row in first..rows {
                let share = 0.05 + 0.2 * (1.0 - f64::from(row) / f64::from(rows));
                let cell = Rect::new(area.x + column as u16, area.y + row, 1, 1);
                canvas.fill(cell, crate::style::mix(color, field, share));
            }
        }
    }
    canvas.dots(&dots);
}

/// Interpolates the value at `at` between `before` and a later point at `after_at`, along the line between them.
fn between(before: &Point, after_at: f64, after: Option<f64>, at: f64) -> f64 {
    let (from, to) = (before.at.as_secs_f64(), after_at);
    match (before.value, after) {
        (Some(a), Some(b)) if to > from => {
            a + (b - a) * ((at - from) / (to - from)).clamp(0.0, 1.0)
        }
        (Some(a), _) => a,
        _ => 0.0,
    }
}

/// Draws `points` over `window` seconds as one bar a cell from the fastest to the slowest value it holds, on a scale
/// to `top`. A bar past it ends at the top in an arrowhead.
pub fn bars(
    canvas: &mut Canvas,
    area: Rect,
    points: &[Point],
    (top, window): (f64, (f64, f64)),
    color: Color,
) {
    let mut dots = Dots::new(area);
    let (_, down) = dots.size();
    let span = (window.1 - window.0).max(f64::EPSILON);
    let cells = usize::from(area.width);
    let mut ranges = vec![None::<(f64, f64)>; cells];
    for point in points.iter().filter(|point| point.value.is_some()) {
        let share = (point.at.as_secs_f64() - window.0) / span;
        let Some(range) = ranges.get_mut((share * cells as f64).floor().max(0.0) as usize) else {
            continue;
        };
        let (low, high) = range.get_or_insert((point.low, point.peak));
        (*low, *high) = (low.min(point.low), high.max(point.peak));
    }
    let row = |value: f64| {
        (down - 1) as isize - ((value / top).clamp(0.0, 1.0) * (down - 1) as f64).round() as isize
    };
    let mut over = Vec::new();
    for (cell, range) in ranges.iter().enumerate() {
        let Some(&(low, high)) = range.as_ref() else {
            continue;
        };
        for dot in row(high)..=row(low) {
            dots.set(2 * cell as isize, dot, color, 1);
        }
        if high > top {
            over.push(cell as u16);
        }
    }
    canvas.dots(&dots);
    let arrow = Line::styled("▴", Style::fg(color));
    for cell in over {
        canvas.line(area.x + cell, area.y, 1, &arrow);
    }
}
