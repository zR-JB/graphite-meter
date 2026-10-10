//! The console's parts, drawn as the browser's are (`docs/DESIGN.md`): the dial, strips and lines, cards and facts, the
//! key, the latency lanes, the stage track and sections; flat, with colour only where it names a stage or a state.
use super::theme::{self, Palette};
use crate::{
    events::Point,
    measure::{format, latency::Population},
    model::Stage,
    report::vocabulary as words,
    text::{self, Color, Line, Style},
};
use std::{f64::consts::FRAC_PI_4, time::Duration};

/// The browser's dial transfer curve (`client/src/lib/components/gaugeScale.ts`): equal sweeps for each knot.
const KNOTS: [f64; 9] = [0.0, 0.005, 0.01, 0.05, 0.1, 0.25, 0.5, 0.75, 1.0];

/// Where `value` sits on a dial reaching `scale`, from 0 to 1.
pub fn gauge_fraction(value: f64, scale: f64) -> f64 {
    let v = (value / scale).clamp(0.0, 1.0);
    if v.is_nan() {
        return 0.0;
    }
    let spans = (KNOTS.len() - 1) as f64;
    let i = (1..KNOTS.len()).find(|&i| v <= KNOTS[i]).unwrap_or(KNOTS.len() - 1);
    (i as f64 - 1.0 + (v - KNOTS[i - 1]) / (KNOTS[i] - KNOTS[i - 1])) / spans
}

/// The value at `fraction` of a dial reaching `scale`.
pub fn gauge_value(fraction: f64, scale: f64) -> f64 {
    let at = fraction.clamp(0.0, 1.0) * (KNOTS.len() - 1) as f64;
    let i = (at as usize).min(KNOTS.len() - 2);
    scale * (KNOTS[i] + (at - i as f64) * (KNOTS[i + 1] - KNOTS[i]))
}

/// The browser's axis ceiling (`client/src/lib/presentation/scales.ts`): the first step at or above `v`.
pub fn ceil_step(v: f64, steps: &[f64]) -> f64 {
    if !v.is_finite() || v <= 0.0 {
        return steps[0];
    }
    let decade = 10_f64.powf(v.log10().floor());
    steps
        .iter()
        .map(|step| step * decade)
        .find(|&step| step >= v)
        .unwrap_or(10.0 * decade)
}

/// The browser's tick label: bounded precision, no trailing zeros.
pub fn gauge_tick(v: f64) -> String {
    if v == 0.0 || v.is_nan() {
        return "0".into();
    }
    let places = (2 - v.abs().log10().floor() as i32).clamp(0, 6) as usize;
    let text = format!("{v:.places$}");
    match text.contains('.') {
        true => text.trim_end_matches('0').trim_end_matches('.').into(),
        false => text,
    }
}

pub fn icon(stage: Stage) -> &'static str {
    ["≈", "↓", "↑", "↕"][stage as usize]
}

/// One arc of the dial: how far it reaches, 0–1 of the sweep, in its hue.
pub struct Arc {
    pub to: f64,
    pub hue: Style,
}

pub struct Dial {
    pub arcs: Vec<Arc>,
    pub ticks: [String; 5],
    pub label: String,
    pub value: String,
    pub unit: &'static str,
    /// Under the unit.
    pub note: Line,
    pub hue: Style,
}

/// The dial opens downward over 270°, as the browser's does.
const START: f64 = 225.0;
const TURN: f64 = 270.0;
/// The dot of each row and column within a braille cell.
const DOTS: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

fn braille(dots: u8) -> char {
    char::from_u32(0x2800 + u32::from(dots)).unwrap_or(' ')
}

/// The ring's thickness, a share of its radius; an arc's head is a bead this much wider.
const THICK: f64 = 0.13;
const BEAD: f64 = 1.45;
/// Samples per pixel side; a pixel's colour is what its samples see, so the ring's edges are smooth.
const SAMPLES: usize = 4;

/// The ring in half-block pixels, two to a cell, with its ticks outside it and the readout in large figures inside.
/// Where arcs overlap the shorter one shows, and each ends in a bead, so every arc's head stays visible. Each pixel
/// mixes the arcs, track and canvas its samples land on, over the terminal's own background where it said.
pub fn dial(palette: &Palette, d: &Dial, width: usize, height: usize) -> Vec<Line> {
    const MARGIN: usize = 5;
    let (cols, rows) = (width.saturating_sub(2 * MARGIN).max(12), height.saturating_sub(1).max(6));
    let (w, h) = (cols as f64, rows as f64 * 2.0);
    let radius = ((w / 2.0 - 0.5) / (1.0 + THICK / 2.0)).min((h - 1.0) / (1.0 + FRAC_PI_4.sin() + THICK));
    let thick = radius * THICK;
    let (cx, cy) = (w / 2.0, radius + thick / 2.0 + 0.5);
    let point = |f: f64| {
        let angle = (START - f * TURN).to_radians();
        (cx + radius * angle.cos(), cy - radius * angle.sin())
    };
    let start = point(0.0);
    // On the sweep from its start to `to`, with round ends: the start's as wide as the ring, the head's `head` wide.
    let on = |x: f64, y: f64, to: f64, (hx, hy): (f64, f64), head: f64| {
        let (dx, dy) = (x - cx, cy - y);
        let f = (START - dy.atan2(dx).to_degrees()).rem_euclid(360.0) / TURN;
        f <= to && (dx.hypot(dy) - radius).abs() <= thick / 2.0
            || (x - start.0).hypot(y - start.1) <= thick / 2.0
            || (x - hx).hypot(y - hy) <= head / 2.0
    };
    let mut arcs: Vec<(f64, (f64, f64), Color)> = d
        .arcs
        .iter()
        .filter_map(|arc| {
            let to = arc.to.clamp(0.0, 1.0);
            Some((to, point(to), arc.hue.fg?))
        })
        .collect();
    arcs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (canvas, track, end) = (palette.canvas(), palette.border.fg.unwrap_or(palette.canvas()), point(1.0));
    let pixel = |x: usize, y: usize| {
        // Only pixels within a bead's reach of the ring are sampled.
        let (dx, dy) = (x as f64 + 0.5 - cx, cy - (y as f64 + 0.5));
        if (dx.hypot(dy) - radius).abs() > thick * BEAD / 2.0 + 1.0 {
            return None;
        }
        let (mut sum, mut seen) = ([0_u32; 3], Vec::<(Color, u32)>::new());
        for sample in 0..SAMPLES * SAMPLES {
            let at = |index: usize, origin: usize| origin as f64 + (index as f64 + 0.5) / SAMPLES as f64;
            let (sx, sy) = (at(sample % SAMPLES, x), at(sample / SAMPLES, y));
            let color = arcs
                .iter()
                .find(|&&(to, head, _)| on(sx, sy, to, head, thick * BEAD))
                .map(|&(_, _, color)| color)
                .or_else(|| on(sx, sy, 1.0, end, thick).then_some(track))
                .unwrap_or(canvas);
            for (channel, shift) in sum.iter_mut().zip([16, 8, 0]) {
                *channel += (color.rgb >> shift) & 0xff;
            }
            match seen.iter_mut().find(|(seen, _)| *seen == color) {
                Some((_, count)) => *count += 1,
                None => seen.push((color, 1)),
            }
        }
        // Sixteen colours cannot mix, so a pixel takes the one most of its samples saw.
        let most = seen
            .iter()
            .max_by_key(|(_, count)| *count)
            .map_or(canvas, |&(color, _)| color);
        if most == canvas && seen.len() == 1 {
            return None;
        }
        let rgb = sum.map(|channel| (channel / (SAMPLES * SAMPLES) as u32) as u8);
        Some(theme::nearest(rgb, if most == canvas { canvas.ansi } else { most.ansi }))
    };
    let mut readout = vec![Line::styled(&d.label, d.hue), Line::default()];
    readout.extend(figures(&d.value).map(|row| Line::styled(row, palette.value)));
    readout.extend([Line::styled(d.unit, palette.muted), d.note.clone()]);
    let first = (cy / 2.0) as isize - readout.len() as isize / 2 + 1;
    let ring = (0..rows).map(|row| {
        let text = usize::try_from(row as isize - first)
            .ok()
            .and_then(|index| readout.get(index));
        let text = text.filter(|text| text.width() > 0);
        let at = text.map_or(usize::MAX, |text| cols.saturating_sub(text.width()) / 2);
        let mut line = Line::default();
        let mut column = 0;
        while column < cols {
            if column == at {
                let text = text.expect("a placed readout");
                line = line.with(text.clone());
                column += text.width();
                continue;
            }
            // The upper pixel is the glyph, the lower its background; a bare canvas half stays the terminal's.
            match (pixel(column, 2 * row), pixel(column, 2 * row + 1)) {
                (None, None) => line.push(' ', Style::default()),
                (Some(upper), lower) => line.push('▀', Style { fg: Some(upper), bg: lower, bold: false }),
                (None, Some(lower)) => line.push('▄', Style::fg(lower)),
            }
            column += 1;
        }
        line
    });
    let ring: Vec<Line> = ring.collect();
    // Ticks sit outside the ring at 0, ¼, ½, ¾ and the full sweep.
    let (mut left, mut right) = (vec![Line::default(); rows], vec![Line::default(); rows]);
    let mut top = Line::default();
    for (index, tick) in d.ticks.iter().enumerate() {
        let label = Line::styled(tick.chars().take(MARGIN - 1).collect::<String>(), palette.muted);
        let angle = (START - index as f64 / 4.0 * TURN).to_radians();
        let reach = radius + thick + 1.5;
        let (x, y) = (cx + reach * angle.cos(), (cy - reach * angle.sin()) / 2.0);
        let row = (y as isize).clamp(0, rows as isize - 1) as usize;
        match () {
            _ if index == 2 => {
                let indent = (cols + 2 * MARGIN).saturating_sub(label.width()) / 2;
                top = Line::plain(" ".repeat(indent)).with(label);
            }
            _ if x < cols as f64 / 2.0 => left[row] = label,
            _ => right[row] = label,
        }
    }
    let rows = ring
        .into_iter()
        .zip(left.into_iter().zip(right))
        .map(|(ring, (left, right))| {
            let indent = " ".repeat(MARGIN.saturating_sub(left.width() + 1));
            Line::plain(indent)
                .with(left)
                .and(" ", Style::default())
                .with(ring)
                .and(" ", Style::default())
                .with(right)
        });
    std::iter::once(top).chain(rows).collect()
}

/// The dial without its ring, for a console too narrow for one.
pub fn readout(palette: &Palette, d: &Dial) -> Vec<Line> {
    let mut lines = vec![Line::styled(&d.label, d.hue)];
    for (index, row) in figures(&d.value).into_iter().enumerate() {
        let mut line = Line::styled(row, palette.value);
        if index == 2 {
            line = line.and(format!("  {}  ", d.unit), palette.muted).with(d.note.clone());
        }
        lines.push(line);
    }
    lines
}

/// `text` in figures three rows tall, in rounded strokes; characters without a figure are left out.
fn figures(text: &str) -> [String; 3] {
    let glyph = |c| -> Option<[&str; 3]> {
        Some(match c {
            '0' => ["╭─╮", "│ │", "╰─╯"],
            '1' => ["╶┐ ", " │ ", "╶┴╴"],
            '2' => ["╶─╮", "╭─╯", "╰─╴"],
            '3' => ["╶─╮", " ─┤", "╶─╯"],
            '4' => ["╷ ╷", "╰─┤", "  ╵"],
            '5' => ["╭─╴", "╰─╮", "╶─╯"],
            '6' => ["╭─╴", "├─╮", "╰─╯"],
            '7' => ["╶─┐", "  │", "  ╵"],
            '8' => ["╭─╮", "├─┤", "╰─╯"],
            '9' => ["╭─╮", "╰─┤", "╶─╯"],
            '.' => [" ", " ", "•"],
            '<' => ["  ", "╱ ", "╲ "],
            '—' => ["   ", "───", "   "],
            ' ' => [" ", " ", " "],
            _ => return None,
        })
    };
    let mut rows = [String::new(), String::new(), String::new()];
    for (index, glyph) in text
        .chars()
        .enumerate()
        .filter_map(|(index, c)| Some((index, glyph(c)?)))
    {
        for (row, part) in rows.iter_mut().zip(glyph) {
            if index > 0 {
                row.push(' ');
            }
            row.push_str(part);
        }
    }
    rows
}

const RISES: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Where `point` falls among `cols` columns over `t0`–`t1` seconds, if it does.
fn column(point: &Point, (t0, t1): (f64, f64), cols: usize) -> Option<usize> {
    let x = (point.at.as_secs_f64() - t0) / (t1 - t0) * cols as f64;
    usize::try_from(x as isize).ok().filter(|&x| x < cols)
}

/// A stage's rate over its window, filled to an eighth of a cell and fading by row from the stage's hue into the canvas,
/// as the browser's strip is. A second band (bidirectional upload) stacks on the first in a flat tint; where they meet
/// the lower band rises over the upper as background. An empty column keeps a baseline.
pub fn strip(
    palette: &Palette,
    stage: Stage,
    bands: &[&[Point]],
    top: f64,
    window: (f64, f64),
    (cols, rows): (usize, usize),
) -> Vec<Line> {
    let mut heights = vec![[0.0; 2]; cols];
    for (band, points) in bands.iter().take(2).enumerate() {
        let (mut sums, mut counts) = (vec![0.0; cols], vec![0_u32; cols]);
        for point in points.iter() {
            if let (Some(x), Some(value)) = (column(point, window, cols), point.value) {
                (sums[x], counts[x]) = (sums[x] + value * f64::from(point.count), counts[x] + point.count);
            }
        }
        for x in (0..cols).filter(|&x| counts[x] > 0) {
            heights[x][band] = (sums[x] / f64::from(counts[x]) / top).min(1.0) * (rows * 8) as f64;
        }
    }
    let shades = palette.shades(stage);
    let upper = shades[1];
    let rise = |height: f64| RISES[(height.round() as usize).clamp(1, 8)];
    let lines = (0..rows).map(|row| {
        let floor = ((rows - 1 - row) * 8) as f64;
        let shade = shades[(row * shades.len() / rows).min(shades.len() - 1)];
        let mut line = Line::default();
        for &[lower, second] in &heights {
            let (total, stacked) = (lower + second, second > 0.0);
            let (glyph, style) = match () {
                _ if total <= floor && row == rows - 1 => ('▁', palette.border),
                _ if total <= floor => (' ', shade),
                _ if total <= floor + 8.0 => {
                    (rise(total - floor), if stacked && lower <= floor + 4.0 { upper } else { shade })
                }
                _ if stacked && lower > floor && lower < floor + 8.0 => {
                    (rise(lower - floor), Style { bg: upper.fg, ..shade })
                }
                _ if stacked && lower <= floor => ('█', upper),
                _ => ('█', shade),
            };
            line.push(glyph, style);
        }
        line
    });
    lines.collect()
}

/// `points` over a window as a braille line in `style`, broken where a value is missing, over a baseline.
pub fn line(
    palette: &Palette,
    points: &[Point],
    top: f64,
    (t0, t1): (f64, f64),
    (cols, rows): (usize, usize),
    style: Style,
) -> Vec<Line> {
    let (dot_w, dot_h) = (cols * 2, rows * 4);
    let mut dots = vec![0_u8; cols * rows];
    let mut last: Option<(isize, isize)> = None;
    for point in points {
        let x = ((point.at.as_secs_f64() - t0) / (t1 - t0) * dot_w as f64) as isize;
        let Some(value) = point.value.filter(|_| (0..dot_w as isize).contains(&x)) else {
            last = None;
            continue;
        };
        let y = dot_h as isize - 1 - ((value / top).clamp(0.0, 1.0) * (dot_h - 1) as f64).round() as isize;
        // Bresenham from the previous point.
        let (mut px, mut py) = last.unwrap_or((x, y));
        let (dx, dy, sx, sy) = ((x - px).abs(), -(y - py).abs(), (x - px).signum(), (y - py).signum());
        let mut error = dx + dy;
        loop {
            let (cx, cy) = (px as usize, py as usize);
            dots[cy / 4 * cols + cx / 2] |= DOTS[cy % 4][cx % 2];
            if (px, py) == (x, y) {
                break;
            }
            let doubled = 2 * error;
            if doubled >= dy {
                (error, px) = (error + dy, px + sx);
            }
            if doubled <= dx {
                (error, py) = (error + dx, py + sy);
            }
        }
        last = Some((x, y));
    }
    let lines = dots.chunks(cols.max(1)).enumerate().map(|(row, cells)| {
        let mut line = Line::default();
        for &cell in cells {
            match cell {
                0 if row == rows - 1 => line.push('▁', palette.border),
                0 => line.push(' ', Style::default()),
                dots => line.push(braille(dots), style),
            }
        }
        line
    });
    lines.collect()
}

/// A stage's panel: a rule in its hue over its title, then `body`.
pub fn card(palette: &Palette, stage: Stage, body: Vec<Line>, width: usize) -> Vec<Line> {
    let hue = palette.stage(stage);
    let title = format!("{} {}", icon(stage), words::label(stage));
    let head = [Line::styled("━".repeat(width), hue), Line::styled(title, hue.bold())];
    head.into_iter()
        .chain(body.into_iter().map(|line| line.fit(width)))
        .collect()
}

/// A neutral panel's head: a rule in the border tone over its title.
pub fn section(palette: &Palette, title: &str, width: usize) -> [Line; 2] {
    [
        Line::styled("━".repeat(width), palette.border),
        Line::styled(title, palette.heading).fit(width),
    ]
}

/// A label and its figure on one row, the figure flush right.
pub fn fact(palette: &Palette, label: &str, value: &str, width: usize) -> Line {
    let gap = width.saturating_sub(text::width(label) + text::width(value)).max(1);
    Line::styled(label, palette.muted)
        .and(" ".repeat(gap), Style::default())
        .and(value, palette.text)
}

/// The transport key: a plate in ink three rows tall, its key cap at the right.
pub fn key(palette: &Palette, label: &str, note: &str, cap: &str, width: usize, enabled: bool) -> [Line; 3] {
    let (plate, faint) = match enabled {
        true => (palette.plate, palette.plate_note),
        false => (palette.plate_off, palette.plate_off),
    };
    let mut text = Line::styled(label, plate.bold());
    if !note.is_empty() {
        text = text.and("  ", plate).and(note, faint);
    }
    let cap_width = text::width(cap) + 2;
    let gap = width.saturating_sub(text.width() + 2 * cap_width);
    let middle = Line::styled(" ".repeat(cap_width + gap / 2), plate)
        .with(text)
        .and(" ".repeat(gap - gap / 2), plate)
        .and(cap, faint)
        .and("  ", plate);
    let blank = Line::styled(" ".repeat(width.max(middle.width())), plate);
    [blank.clone(), middle, blank]
}

/// A latency table row: its population once it has a median, else a note saying why not.
pub struct Lane {
    pub stage: Stage,
    pub population: Option<Population>,
    pub note: Line,
}

/// The latency table: figures beside a lane from the median to the 95th percentile, the idle median marked down every
/// lane so that added latency reads as a distance. Under ten columns the lane gives way to the figures.
pub fn lanes(palette: &Palette, rows: &[Lane], width: usize) -> Vec<Line> {
    const LABEL: usize = 14;
    const FIGURE: usize = 10;
    let lane = Some(width.saturating_sub(LABEL + 4 * FIGURE + 2))
        .filter(|&lane| lane >= 10)
        .unwrap_or(0);
    let ms = |duration: Duration| duration.as_secs_f64() * 1e3;
    let measured = |row: &Lane| Some((row.population?.median()?, row.population?.summary));
    let peaks = rows
        .iter()
        .filter_map(measured)
        .map(|(median, summary)| ms(median).max(summary.p95.map_or(0.0, ms)));
    let scale = ceil_step((peaks.fold(0.0, f64::max) * 1.1).max(1.0), &[1.0, 2.0, 4.0]);
    let idle = rows
        .iter()
        .find(|row| row.stage == Stage::Latency)
        .and_then(measured)
        .map(|(median, _)| median);
    let any = rows.iter().any(|row| measured(row).is_some());
    let at =
        |duration: Duration| ((ms(duration) / scale * (lane as f64 - 1.0) + 0.5) as usize).min(lane.saturating_sub(1));
    let gutter = if lane > 0 { "  " } else { "" };
    let right = |text: &str| format!("{text:>FIGURE$}");
    let mut lines = Vec::new();
    // Until a row is measured the rows say why, without headings over empty columns.
    if any {
        let headings = ["Median", "Jitter", "Timeouts"].map(right).concat();
        let heading = format!("{}{headings}{gutter}{}{}", " ".repeat(LABEL), " ".repeat(lane), right("Added"));
        lines.push(Line::styled(heading, palette.muted));
    }
    for row in rows {
        let label = Line::styled(format!("{} ", icon(row.stage)), palette.stage(row.stage))
            .and(format!("{:<w$}", words::compact_population(row.stage), w = LABEL - 2), palette.text);
        let Some((median, summary)) = measured(row) else {
            lines.push(label.with(row.note.clone()));
            continue;
        };
        let hue = palette.trace(row.stage);
        let timeouts = summary
            .timeout_ratio()
            .map_or(words::MISSING.into(), |ratio| format!("{:.1}%", ratio * 100.0));
        let jitter = summary
            .jitter
            .filter(|_| summary.jitter_pairs > 0)
            .map_or(words::MISSING.into(), words::ms);
        let added = idle.filter(|_| row.stage != Stage::Latency);
        let added = added.map(|idle| format!("{} ms", format::added(ms(median) - ms(idle))));
        let mut line = label
            .and(right(&words::ms(median)), palette.value)
            .and(right(&jitter), palette.text)
            .and(right(&timeouts), palette.text)
            .and(gutter, Style::default());
        if lane > 0 {
            let mut track = vec![(' ', Style::default()); lane];
            let (mid, to) = (at(median), at(summary.p95.unwrap_or(median)));
            if let (Some(idle), Some(_)) = (idle, &added) {
                let from = at(idle);
                track[from.min(mid)..from.max(mid)].fill(('─', hue));
                track[from] = ('┊', palette.stage(Stage::Latency));
            }
            track[mid + 1..=to.max(mid)].fill(('━', hue));
            track[mid] = ('●', hue);
            for (glyph, style) in track {
                line.push(glyph, style);
            }
        }
        lines.push(line.and(right(added.as_deref().unwrap_or_default()), palette.stage(row.stage)));
    }
    if lane > 0 && any {
        let end = format!("{} ms", gauge_tick(scale));
        let axis = format!("0{}{end}", " ".repeat(lane.saturating_sub(1 + text::width(&end)).max(1)));
        lines.push(Line::plain(" ".repeat(LABEL + 3 * FIGURE + 2)).and(axis, palette.muted));
    }
    lines
}

/// A stage on the track: how far it ran, 0–1, and its status.
pub struct Chip {
    pub stage: Stage,
    pub progress: f64,
    pub status: Line,
}

/// The stage track: each stage's rule fills in its hue as the stage runs, over its name and status.
pub fn chips(palette: &Palette, chips: &[Chip], width: usize) -> Vec<Line> {
    const GAP: usize = 2;
    if chips.is_empty() {
        return Vec::new();
    }
    let cell = width.saturating_sub(GAP * (chips.len() - 1)) / chips.len();
    // A cramped track names every stage in its hue instead of beside its icon.
    let fits = |chip: &Chip| text::width(words::label(chip.stage)) + chip.status.width() + 3 <= cell;
    let icons = chips.iter().all(fits);
    let (mut rule, mut name) = (Line::default(), Line::default());
    for (index, chip) in chips.iter().enumerate() {
        if index > 0 {
            rule = rule.and(" ".repeat(GAP), Style::default());
            name = name.and(" ".repeat(GAP), Style::default());
        }
        let hue = palette.stage(chip.stage);
        let lit = (chip.progress.clamp(0.0, 1.0) * cell as f64).round() as usize;
        rule = rule
            .and("━".repeat(lit), hue)
            .and("━".repeat(cell - lit), palette.border);
        let title = match icons {
            true => Line::styled(icon(chip.stage), hue)
                .and(" ", Style::default())
                .and(words::label(chip.stage), palette.text),
            false => Line::styled(words::compact_stage(chip.stage), hue),
        };
        let title = title.fit(cell.saturating_sub(chip.status.width() + 1).max(1));
        let gap = cell.saturating_sub(title.width() + chip.status.width()).max(1);
        name = name
            .with(title)
            .and(" ".repeat(gap), Style::default())
            .with(chip.status.clone());
    }
    vec![rule, name]
}
