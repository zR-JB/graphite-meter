//! Braille charts of a run's traces over the plan's measured time: each trace in its stages' colours, dashed where it
//! is the upload of a bidirectional stage, with a scale and a ruler marking where each stage began.
use super::theme::Palette;
use crate::{
    events::{Point, Series},
    measure::format,
    model::Stage,
    report::vocabulary as words,
    text::{Line, Style},
};
use std::time::Duration;

/// The scale's columns left of the axis.
const SCALE: usize = 10;
/// The dot of each row and column within a braille cell.
const DOTS: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// What a chart's values measure: rates in bytes per second, labelled in bits, or round trips in milliseconds.
#[derive(Debug, Clone, Copy)]
pub enum Axis {
    Rate,
    Ms,
}

impl Axis {
    /// The factor from values to labels.
    fn scale(self) -> f64 {
        match self {
            Self::Rate => 8.0,
            Self::Ms => 1.0,
        }
    }

    /// `value` in the label's unit, the largest it reaches for rates.
    fn label(self, value: f64) -> String {
        let round = |value: f64| (value * 1000.0).round() / 1000.0;
        match self {
            Self::Rate => {
                let (value, unit) = format::tier(value, 1.0);
                format!("{} {unit}", round(value))
            }
            Self::Ms => format!("{} ms", round(value)),
        }
    }
}

/// A trace: its series, and whether its stretch in a bidirectional stage is dashed.
pub type Trace<'a> = (&'a Series, bool);

/// `traces` over `span` in `width` × `height` cells, with `marks` where stages began.
pub fn chart(
    traces: &[Trace],
    marks: &[(Duration, Stage)],
    axis: Axis,
    span: Duration,
    (width, height): (usize, usize),
    palette: &Palette,
) -> Vec<Line> {
    let (columns, rows) = (width.saturating_sub(SCALE + 1).max(4), height.saturating_sub(2).max(2));
    let stretches: Vec<_> = traces
        .iter()
        .flat_map(|&(series, dashed)| stretches(series.points(), marks, dashed))
        .collect();
    let values = stretches.iter().flat_map(|stretch| stretch.2);
    let peak = values.fold(0.0_f64, |peak, point| peak.max(point.peak).max(point.value.unwrap_or(0.0)));
    let top = nice(peak * axis.scale() * 1.05) / axis.scale();
    let mut canvas = Canvas {
        columns,
        rows,
        dots: vec![0; columns * rows],
        owner: vec![0; columns * rows],
    };
    let span = span.as_secs_f64().max(1.0);
    for (index, (_, dashed, points)) in stretches.iter().enumerate() {
        canvas.plot(index, *dashed, points, span, top);
    }
    let mut lines = Vec::new();
    for row in 0..rows {
        let middle = row == rows / 2 && rows >= 6;
        let scale = match row {
            0 if peak > 0.0 => axis.label(top * axis.scale()),
            _ if row == rows - 1 => "0".into(),
            _ if middle && peak > 0.0 => axis.label(top * axis.scale() / 2.0),
            _ => String::new(),
        };
        let scale: String = scale.chars().take(SCALE).collect();
        let mut line = Line::styled(format!("{scale:>SCALE$}"), palette.muted).and("│", palette.border);
        let cells: Vec<usize> = (row * columns..(row + 1) * columns).collect();
        let owned = |a: &usize, b: &usize| {
            (canvas.dots[*a] == 0) == (canvas.dots[*b] == 0) && canvas.owner[*a] == canvas.owner[*b]
        };
        for run in cells.chunk_by(owned) {
            let glyphs = || {
                run.iter()
                    .map(|&cell| char::from_u32(0x2800 + u32::from(canvas.dots[cell])).unwrap_or(' '))
            };
            line = match canvas.dots[run[0]] {
                0 if middle => line.and("┄".repeat(run.len()), palette.border),
                0 => line.and(" ".repeat(run.len()), Style::default()),
                _ => line.and(glyphs().collect::<String>(), palette.trace(stretches[canvas.owner[run[0]]].0)),
            };
        }
        lines.push(line);
    }
    lines.extend(ruler(marks, span, columns, palette));
    lines
}

/// The ruler with a tick where each stage began, and beneath it their names and the span's end.
fn ruler(marks: &[(Duration, Stage)], span: f64, columns: usize, palette: &Palette) -> [Line; 2] {
    let end: String = words::clock(Duration::from_secs_f64(span))
        .chars()
        .take(columns)
        .collect();
    let end_at = columns - end.chars().count();
    let column = |at: Duration| ((at.as_secs_f64() / span * columns as f64) as usize).min(columns - 1);
    let (mut ticks, mut names, mut written) = (vec!['─'; columns], Line::plain(" ".repeat(SCALE + 1)), 0);
    for (index, &(at, stage)) in marks.iter().enumerate() {
        let x = column(at);
        ticks[x] = '┬';
        let next = marks
            .get(index + 1)
            .map_or(end_at, |&(next, _)| column(next).min(end_at));
        let room = next.saturating_sub(x + 1);
        if room >= 3 && x >= written {
            let name = Line::styled(words::compact_stage(stage), palette.stage(stage)).fit(room);
            (names, written) = (names.and(" ".repeat(x - written), Style::default()), x + name.width());
            names = names.with(name);
        }
    }
    let names = names.and(" ".repeat(end_at.saturating_sub(written)), Style::default());
    [
        Line::plain(" ".repeat(SCALE)).and(format!("└{}", ticks.into_iter().collect::<String>()), palette.border),
        names.and(end, palette.muted),
    ]
}

/// `points` split at each mark into its stage's stretch; the upload's in a bidirectional stage is dashed.
fn stretches<'a>(
    mut points: &'a [Point],
    marks: &[(Duration, Stage)],
    upload: bool,
) -> Vec<(Stage, bool, &'a [Point])> {
    let mut stretches = Vec::new();
    for &(at, stage) in marks.iter().rev() {
        let start = points.partition_point(|point| point.at < at);
        stretches.push((stage, upload && stage == Stage::Bidirectional, &points[start..]));
        points = &points[..start];
    }
    stretches.reverse();
    stretches
}

/// The smallest of 1, 2, 2.5 and 5 times a power of ten that reaches `value`.
fn nice(value: f64) -> f64 {
    if value <= 0.0 {
        return 1.0;
    }
    let decade = 10_f64.powf(value.log10().floor());
    let mut steps = [1.0, 2.0, 2.5, 5.0].into_iter().map(|step| step * decade);
    steps.find(|ceiling| *ceiling >= value).unwrap_or(10.0 * decade)
}

/// Braille cells and the stretch that drew each last.
struct Canvas {
    columns: usize,
    rows: usize,
    dots: Vec<u8>,
    owner: Vec<usize>,
}

impl Canvas {
    /// Draws `points` as stretch `owner`: values sharing a dot column merge into their mean, lines join them and a
    /// gap breaks them.
    fn plot(&mut self, owner: usize, dashed: bool, points: &[Point], span: f64, top: f64) {
        let (width, height) = (self.columns * 2, self.rows * 4);
        let column = |point: &Point| ((point.at.as_secs_f64() / span * width as f64) as usize).min(width - 1);
        let (mut last, mut points) = (None, points.iter().peekable());
        while let Some(point) = points.next() {
            let Some(value) = point.value else {
                last = None;
                continue;
            };
            let x = column(point);
            let (mut sum, mut count) = (value * f64::from(point.count), f64::from(point.count));
            while let Some(next) = points.next_if(|next| next.value.is_some() && column(next) == x) {
                sum += next.value.unwrap_or(0.0) * f64::from(next.count);
                count += f64::from(next.count);
            }
            let share = (sum / count / top).clamp(0.0, 1.0);
            let y = height - 1 - (share * (height - 1) as f64).round() as usize;
            self.line(last.unwrap_or((x, y)), (x, y), owner, dashed);
            last = Some((x, y));
        }
    }

    /// A line of dots from `from` to `to`, every third pair of columns left out when `dashed`.
    fn line(&mut self, (mut x, mut y): (usize, usize), (x1, y1): (usize, usize), owner: usize, dashed: bool) {
        let (dx, dy) = (x1.abs_diff(x) as isize, -(y1.abs_diff(y) as isize));
        let (mut error, step_x, step_y) = (dx + dy, if x < x1 { 1 } else { -1 }, if y < y1 { 1 } else { -1 });
        loop {
            if !(dashed && x % 6 >= 4) {
                let cell = y / 4 * self.columns + x / 2;
                self.dots[cell] |= DOTS[y % 4][x % 2];
                self.owner[cell] = owner;
            }
            if (x, y) == (x1, y1) {
                return;
            }
            let doubled = 2 * error;
            if doubled >= dy {
                (error, x) = (error + dy, x.saturating_add_signed(step_x));
            }
            if doubled <= dx {
                (error, y) = (error + dx, y.saturating_add_signed(step_y));
            }
        }
    }
}
