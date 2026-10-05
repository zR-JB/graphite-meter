//! The frame: header, the screen's body in a scrolled viewport, the footer with its keys, and panels; styled lines
//! written into the terminal's buffer.
use super::{App, FRAME, Overlay, SMALLEST, SPINNER, Screen, dialogs, keys};
use crate::{
    VERSION,
    report::{self, vocabulary as words},
    text::{Line, Profile, Style},
};
use graphite_meter_proto::text::safe;
use ratatui_core::{
    buffer::Buffer,
    style::{Color, Modifier},
};
use std::time::Instant;

impl App {
    /// Draws a frame into `buffer`.
    pub fn draw(&mut self, buffer: &mut Buffer, now: Instant) {
        self.now = now;
        let (width, height) = (usize::from(buffer.area.width), usize::from(buffer.area.height));
        if width < SMALLEST.0 || height < SMALLEST.1 {
            let notice = format!("Enlarge the terminal to at least {}×{}.", SMALLEST.0, SMALLEST.1);
            let x = width.saturating_sub(crate::text::width(&notice)) / 2;
            return put(buffer, x, height / 2, width, &Line::plain(notice), self.profile);
        }
        let inner = width - 2;
        let mut lines = self.header(inner);
        lines.extend((height > 24).then(Line::default));
        let rows = height
            .saturating_sub(lines.len() + self.footer(inner, false).len())
            .max(1);
        let (body, focus) = self.body(inner, rows);
        let hidden = body.len().saturating_sub(rows);
        if let Some(focus) = focus.filter(|_| std::mem::take(&mut self.follow)) {
            self.scroll = self.scroll.clamp((focus + 1).saturating_sub(rows), focus);
        }
        (self.scroll, self.rows) = (self.scroll.min(hidden), rows);
        let top = lines.len();
        lines.extend(body.into_iter().skip(self.scroll).take(rows));
        lines.resize(top + rows, Line::default());
        lines.extend(self.footer(inner, hidden > 0));
        for (y, line) in lines.iter().enumerate() {
            put(buffer, 1, y, inner, line, self.profile);
        }
    }

    /// The title, the status and what the test reaches.
    fn header(&self, width: usize) -> Vec<Line> {
        let palette = &self.palette;
        let (title, status) = (" Graphite Meter ", format!(" {} ", self.status()));
        let gap = width
            .saturating_sub(crate::text::width(title) + crate::text::width(&status))
            .max(1);
        let context = match self.screen {
            Screen::Run => {
                let names = self
                    .view
                    .servers
                    .iter()
                    .map(|server| words::server(&server.name, &server.location));
                names.collect::<Vec<_>>().join(", ")
            }
            _ => self.config.url.to_string(),
        };
        let first = Line::styled(title, palette.title)
            .and(" ".repeat(gap), Style::default())
            .and(status, palette.pill);
        let second = Line::styled(format!("native client {VERSION}  "), palette.muted).and(context, palette.accent);
        vec![first.fit(width), second.fit(width)]
    }

    fn status(&self) -> &'static str {
        let run = self.view.run.as_ref().filter(|_| matches!(self.screen, Screen::Run));
        match run.map(|run| (run.outcome, &run.stage)) {
            Some((Some(outcome), _)) => words::outcome_label(outcome),
            Some((None, Some((plan, Some(_))))) => words::label(plan.stage),
            Some((None, Some((_, None)))) => "Warmup",
            Some((None, None)) => "Checking paths",
            None if self.config.validate().is_err() => "Test cannot start",
            None if self.checking() => "Checking paths",
            None if self.could_not_start() => "Test could not start",
            None => "Not started",
        }
    }

    /// The notice, or the focused row's help, over the keys the screen offers.
    fn footer(&self, width: usize, more: bool) -> Vec<Line> {
        let palette = &self.palette;
        let notice = match &self.overlay {
            Overlay::Edit(editor) if editor.error().is_some() => Line::styled(&self.notice, palette.err),
            Overlay::None if self.notice.is_empty() && matches!(self.screen, Screen::Setup) => {
                Line::styled(self.show(self.row()).help, palette.muted)
            }
            _ => Line::styled(&self.notice, palette.muted),
        };
        let all = self.bindings(self.help);
        if self.help {
            return [vec![notice], dialogs::columns(&all, palette)]
                .concat()
                .into_iter()
                .map(|line| line.fit(width))
                .collect();
        }
        let mut shown = all;
        if more && matches!((&self.screen, &self.overlay), (Screen::Setup | Screen::Run, Overlay::None)) {
            shown.insert(1, keys::PAGE);
        }
        while dialogs::line(&shown, palette).width() > width && shown.len() > 2 {
            shown.remove(shown.len() - 2);
        }
        vec![notice.fit(width), dialogs::line(&shown, palette).fit(width)]
    }

    /// The keys help lists: all of them, or those the footer offers.
    fn bindings(&self, all: bool) -> Vec<keys::Binding> {
        let offered = |action| self.offers(action);
        match (&self.screen, &self.overlay) {
            (_, Overlay::Edit(_)) => keys::listed(keys::EDIT, offered),
            (Screen::Setup, _) if !all => self.hints(),
            (Screen::Setup, _) => keys::listed(keys::SETUP, offered),
            (Screen::Chooser(_), _) => keys::listed(keys::CHOOSER, offered),
            (Screen::SignIn, _) => keys::listed(keys::SIGN_IN, offered),
            (Screen::Run, _) => keys::listed(keys::RUN, offered),
        }
    }

    /// The screen's lines, and the focused one's.
    fn body(&self, width: usize, rows: usize) -> (Vec<Line>, Option<usize>) {
        match &self.screen {
            Screen::Setup => {
                let (lines, focus) = self.setup_body(width);
                (lines, Some(focus))
            }
            Screen::Chooser(chooser) => (self.chooser_body(chooser, width, rows), None),
            Screen::SignIn => {
                let Some(prompt) = &self.view.sign_in else { return (Vec::new(), None) };
                let code = Line::styled("Match this code ", self.palette.text).and(&prompt.code, self.palette.value);
                let mut lines = self.panel(&format!("Sign in to {}", prompt.issuer), vec![code], width, 0);
                lines.extend([Line::default(), Line::styled(&prompt.url, self.palette.accent)]);
                (lines, None)
            }
            Screen::Run if !self.running() => (report::report(&self.view, width, &self.palette), None),
            Screen::Run => {
                let status = format!(" {}…", self.status());
                (
                    vec![Line::styled(self.spinner(), self.palette.accent).and(status, self.palette.muted)],
                    None,
                )
            }
        }
    }

    /// `body` in a rounded border `width` wide under `title`, at least `height` lines tall.
    pub(super) fn panel(&self, title: &str, body: Vec<Line>, width: usize, height: usize) -> Vec<Line> {
        let (border, inner) = (self.palette.border, width.saturating_sub(4).max(1));
        let title = Line::styled(title, self.palette.heading).fit(width.saturating_sub(6));
        let fill = width.saturating_sub(5 + title.width());
        let mut lines = vec![
            Line::styled("╭─ ", border)
                .with(title)
                .and(format!(" {}╮", "─".repeat(fill)), border),
        ];
        let rows = body.len().max(height.saturating_sub(2));
        for line in body.into_iter().chain(std::iter::repeat(Line::default())).take(rows) {
            lines.push(
                Line::styled("│ ", border)
                    .with(line.fit(inner).pad(inner))
                    .and(" │", border),
            );
        }
        lines.push(Line::styled(format!("╰{}╯", "─".repeat(inner + 2)), border));
        lines
    }

    pub(super) fn checkbox(&self, on: bool) -> Line {
        match on {
            true => Line::styled("●", self.palette.accent),
            false => Line::styled("○", self.palette.muted),
        }
    }

    pub(super) fn spinner(&self) -> &'static str {
        let frames = self.now.saturating_duration_since(self.since).as_millis() / FRAME.as_millis();
        SPINNER[(frames % SPINNER.len() as u128) as usize]
    }
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
            Profile::Plain => None,
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
