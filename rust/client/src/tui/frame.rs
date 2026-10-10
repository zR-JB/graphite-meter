//! The frame: header, the body in a scrolled viewport, footer keys and sections, written into the terminal's buffer.
use super::{
    App, Overlay, SMALLEST, SPIN, SPINNER, Screen,
    chrome::{Chrome, Link},
    console, dialogs, keys,
};
use crate::{
    VERSION,
    events::Check,
    report::vocabulary as words,
    run::prepare::ServerPath,
    text::{Line, Profile, Style},
};
use graphite_meter_proto::text::safe;
use ratatui_core::{
    buffer::Buffer,
    style::{Color, Modifier},
};
use std::time::Instant;

impl App {
    /// Draws a frame into `buffer`, and returns what the terminal shows beside it.
    pub fn draw(&mut self, buffer: &mut Buffer, now: Instant) -> Chrome {
        (self.now, self.area) = (now, buffer.area);
        self.stale = self.stale.filter(|at| *at >= now);
        let links = self.paint(buffer);
        let title = format!("Graphite Meter · {}", self.status().0);
        Chrome { title, progress: self.progress(), links }
    }

    /// Paints the frame, and returns where its links landed.
    fn paint(&mut self, buffer: &mut Buffer) -> Vec<Link> {
        let (width, height) = (usize::from(buffer.area.width), usize::from(buffer.area.height));
        if width < SMALLEST.0 || height < SMALLEST.1 {
            let notice = format!("Enlarge the terminal to at least {}×{}.", SMALLEST.0, SMALLEST.1);
            let x = width.saturating_sub(crate::text::width(&notice)) / 2;
            put(buffer, x, height / 2, width, &Line::plain(notice), self.profile);
            return Vec::new();
        }
        let inner = width - 2;
        let mut lines = self.header(inner);
        lines.extend((height > 24).then(Line::default));
        let chrome = lines.len() + self.footer(inner, false).len();
        let rows = height.saturating_sub(chrome).max(1);
        let (body, focus) = self.body(inner, rows);
        let hidden = body.len().saturating_sub(rows);
        if let Some(focus) = focus.filter(|_| std::mem::take(&mut self.follow)) {
            self.scroll = self.scroll.clamp((focus + 1).saturating_sub(rows), focus);
        }
        (self.scroll, self.rows) = (self.scroll.min(hidden), rows);
        let (top, end) = (lines.len(), body.len());
        lines.extend(body.into_iter().skip(self.scroll).take(rows));
        lines.resize(top + rows, Line::default());
        lines.extend(self.footer(inner, hidden > 0));
        for (y, line) in lines.iter().enumerate() {
            put(buffer, 1, y, inner, line, self.profile);
        }
        self.links(inner, top, end)
    }

    /// The sign-in link's shown lines, which end the body of `end` lines drawn from row `top`.
    fn links(&self, width: usize, top: usize, end: usize) -> Vec<Link> {
        let Some((url, texts)) = self.sign_in_link(width) else { return Vec::new() };
        let first = end - texts.len();
        let shown = texts.into_iter().enumerate().filter_map(|(index, text)| {
            let row = (first + index)
                .checked_sub(self.scroll)
                .filter(|&row| row < self.rows)?;
            let row = u16::try_from(top + row).ok()?;
            Some(Link { row, column: 1, url: url.to_owned(), text })
        });
        shown.collect()
    }

    /// The app and what it tests, with the state at the right in its stage's or outcome's tone.
    fn header(&self, width: usize) -> Vec<Line> {
        let palette = &self.palette;
        let (status, tone) = self.status();
        let status = Line::styled(format!("● {status}"), tone);
        let context = match self.screen {
            Screen::Run => self.labels().join(", "),
            _ => self.config.url.as_ref().map(ToString::to_string).unwrap_or_default(),
        };
        let left = Line::styled("◆ Graphite Meter", palette.value)
            .and("  ", Style::default())
            .and(context, palette.muted);
        let left = left.fit(width.saturating_sub(status.width() + 2));
        let gap = width.saturating_sub(left.width() + status.width()).max(1);
        vec![left.and(" ".repeat(gap), Style::default()).with(status).fit(width)]
    }

    /// The state and its tone: the outcome's, the measuring stage's, the warning's when the test cannot start.
    pub(super) fn status(&self) -> (&'static str, Style) {
        let (run, palette) = (self.view.run.as_ref().filter(|_| matches!(self.screen, Screen::Run)), &self.palette);
        let label = match run.map(|run| (run.outcome, &run.stage)) {
            Some((Some(outcome), _)) => {
                return (words::outcome_label(outcome), Style { bold: false, ..palette.outcome(outcome) });
            }
            Some((None, Some((plan, Some(_))))) => return (words::label(plan.stage), palette.stage(plan.stage)),
            Some((None, Some((_, None)))) => "Warmup",
            Some((None, None)) => "Checking paths",
            None if self.config.url.is_none() => "Not started",
            None if self.config.validate().is_err() => return ("Test cannot start", palette.warn),
            None => match &self.screen {
                Screen::SignIn(sign_in) if sign_in.opened => "Checking sign-in",
                Screen::SignIn(_) => "Sign in",
                _ if self.view.check == Check::SignIn => "Sign in",
                _ if self.checking() => "Checking paths",
                _ if self.could_not_start() => return ("Test could not start", palette.warn),
                _ => "Not started",
            },
        };
        (label, palette.muted)
    }

    /// The notice, or the focused row's help, over the keys the screen offers.
    fn footer(&self, width: usize, more: bool) -> Vec<Line> {
        let palette = &self.palette;
        let notice = match &self.overlay {
            Overlay::Edit(editor) if editor.error.is_some() => Line::styled(&self.notice, palette.err),
            Overlay::None if self.notice.is_empty() && matches!(self.screen, Screen::Setup) => {
                Line::styled(self.show(self.row()).help, palette.muted)
            }
            _ => Line::styled(&self.notice, palette.muted),
        };
        let all = match (&self.screen, &self.overlay) {
            (Screen::Setup, Overlay::None) if !self.help => self.hints(),
            _ => keys::listed(self.table(), |action| self.offers(action)),
        };
        if self.help {
            let lines = [vec![notice], dialogs::columns(&all, palette)].concat();
            return lines.into_iter().map(|line| line.fit(width)).collect();
        }
        let mut shown = all;
        if more && matches!((&self.screen, &self.overlay), (Screen::Setup | Screen::Run, Overlay::None)) {
            shown.insert(1, keys::PAGE);
        }
        while dialogs::line(&shown, palette).width() > width && shown.len() > 2 {
            shown.remove(shown.len() - 2);
        }
        let (mut hints, version) = (dialogs::line(&shown, palette), format!("native {VERSION}"));
        let gap = width.saturating_sub(hints.width() + crate::text::width(&version));
        if gap >= 3 {
            hints = hints.and(" ".repeat(gap), Style::default()).and(version, palette.muted);
        }
        vec![notice.fit(width), hints.fit(width)]
    }

    /// The screen's lines, and the focused one's.
    fn body(&self, width: usize, rows: usize) -> (Vec<Line>, Option<usize>) {
        match &self.screen {
            Screen::Setup => {
                let (lines, focus) = self.setup_body(width);
                (lines, Some(focus))
            }
            Screen::Chooser(chooser) => (self.chooser_body(chooser, width, rows), None),
            Screen::SignIn(sign_in) => (self.sign_in_body(sign_in, width), None),
            Screen::Run => (self.run_body(width, rows), None),
        }
    }

    /// `body` as a section `width` wide under `title`, padded to `width` and at least `height` lines tall.
    pub(super) fn panel(&self, title: &str, body: Vec<Line>, width: usize, height: usize) -> Vec<Line> {
        let mut lines = console::section(&self.palette, title, width).to_vec();
        lines.extend(body.into_iter().map(|line| line.fit(width)));
        lines.resize(lines.len().max(height), Line::default());
        lines.into_iter().map(|line| line.pad(width)).collect()
    }

    /// Whether the cell at `column` and `row` shows a key's plate: its background, or a half-block cap's foreground.
    /// It paints in full colour, since 256 or 16 colours would give other cells the plates' colour.
    pub(super) fn on_key(&mut self, column: u16, row: u16) -> bool {
        let profile = std::mem::replace(&mut self.profile, Profile::TrueColor);
        let mut buffer = Buffer::empty(self.area);
        self.paint(&mut buffer);
        self.profile = profile;
        let Some(cell) = buffer.cell((column, row)) else { return false };
        let shown = if matches!(cell.symbol(), "▄" | "▀") { cell.fg } else { cell.bg };
        let plates = [self.palette.plate, self.palette.plate_off].map(|plate| paint(plate, Profile::TrueColor).bg);
        plates.contains(&Some(shown))
    }

    pub(super) fn checkbox(&self, on: bool) -> Line {
        match on {
            true => Line::styled("●", self.palette.accent),
            false => Line::styled("○", self.palette.muted),
        }
    }

    /// The selected servers' names with their locations.
    pub(super) fn labels(&self) -> Vec<String> {
        let label = |server: &ServerPath| words::server(&server.name, &server.location);
        self.view.servers.iter().map(label).collect()
    }

    /// The spinner beside "Checking paths…".
    pub(super) fn checking_line(&self) -> Line {
        Line::styled(self.spinner(), self.palette.accent).and(" Checking paths…", self.palette.muted)
    }

    pub(super) fn spinner(&self) -> &'static str {
        let frames = self.now.saturating_duration_since(self.since).as_millis() / SPIN.as_millis();
        SPINNER[(frames % SPINNER.len() as u128) as usize]
    }
}

/// `left` and `right`, as tall as each other, side by side.
pub(super) fn beside(left: Vec<Line>, right: Vec<Line>) -> Vec<Line> {
    let joined = |(left, right): (Line, Line)| left.and(" ", Style::default()).with(right);
    left.into_iter().zip(right).map(joined).collect()
}

/// `labels` without repeats, in order.
pub(super) fn unique(labels: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut unique = Vec::new();
    for label in labels {
        if !unique.contains(&label) {
            unique.push(label);
        }
    }
    unique
}

/// `line` on the focused row's background, bold.
pub(super) fn highlight(line: Line, selected: Style) -> Line {
    let spans = line.0.into_iter().map(|mut span| {
        span.style = Style {
            fg: span.style.fg.or(selected.fg),
            bg: selected.bg,
            bold: true,
        };
        span
    });
    Line(spans.collect())
}

/// Writes `line` from column `x` of row `y`, at most `width` cells, with every unsafe character blanked.
fn put(buffer: &mut Buffer, x: usize, y: usize, width: usize, line: &Line, profile: Profile) {
    let (Ok(mut x), Ok(y)) = (u16::try_from(x), u16::try_from(y)) else { return };
    let end = x.saturating_add(u16::try_from(width).unwrap_or(u16::MAX));
    for span in &line.0 {
        let text: String = span.text.chars().map(|c| if safe(c) { c } else { ' ' }).collect();
        (x, _) = buffer.set_stringn(x, y, text, usize::from(end.saturating_sub(x)), paint(span.style, profile));
    }
}

/// `style` as `profile` shows it; the 16-colour profile writes indexed colours 0–15.
fn paint(style: Style, profile: Profile) -> ratatui_core::style::Style {
    let color = |color: Option<crate::text::Color>| {
        let color = color?;
        let [_, red, green, blue] = color.rgb.to_be_bytes();
        match profile {
            Profile::Plain | Profile::Ascii => None,
            Profile::Ansi => Some(Color::Indexed(color.ansi)),
            Profile::Ansi256 => Some(Color::Indexed(color.ansi256)),
            Profile::TrueColor => Some(Color::Rgb(red, green, blue)),
        }
    };
    let mut painted = ratatui_core::style::Style {
        fg: color(style.fg),
        bg: color(style.bg),
        ..Default::default()
    };
    if style.bold && profile != Profile::Plain {
        painted = painted.add_modifier(Modifier::BOLD);
    }
    painted
}
