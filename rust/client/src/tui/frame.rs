//! The frame: the top bar, the console, a sheet or card over it on a dimmed backdrop, and the bottom bar.
use super::{
    App, Overlay, SMALLEST, Target,
    chrome::{Chrome, Link},
    console::icon,
    keys::{self, Action},
    paint::Canvas,
    paths::Readiness,
};
use crate::{
    VERSION,
    events::{Check, Running},
    format::{self, MISSING, round_trip},
    model::Stage,
    text::{self, Line, Style, fill},
};
use ratatui_core::{buffer::Buffer, layout::Rect};
use std::time::Instant;

/// A sheet's line, and for an item's its index and what a press on it does.
type SheetLine = (Line, Option<(usize, Option<Target>)>);

/// An entry of a sheet.
pub(super) enum Entry {
    Heading(String),
    /// A label and its value, pressable as `target` when it has one.
    Item {
        label: Line,
        value: Line,
        target: Option<Target>,
    },
    Text(Line),
    Gap,
}

impl App {
    /// Draws a frame into `buffer`, and returns what the terminal shows beside it.
    pub fn draw(&mut self, buffer: &mut Buffer, now: Instant) -> Chrome {
        (self.now, self.area) = (now, buffer.area);
        let mut canvas = Canvas::new(buffer, self.profile);
        let links = self.paint(&mut canvas);
        self.hits = std::mem::take(&mut canvas.hits);
        let title = format!("Graphite Meter · {}", self.status().0);
        Chrome { title, progress: self.progress(), links }
    }

    /// Paints the frame, and returns where its links landed.
    fn paint(&mut self, canvas: &mut Canvas) -> Vec<Link> {
        let area = canvas.area();
        if area.width < SMALLEST.0 || area.height < SMALLEST.1 {
            let notice = format!("Enlarge the terminal to at least {}×{}.", SMALLEST.0, SMALLEST.1);
            canvas.center(area.x, area.y + area.height / 2, area.width, &Line::styled(notice, self.palette.muted));
            return Vec::new();
        }
        let body = Rect::new(area.x, area.y + 1, area.width, area.height - 2);
        self.top_bar(canvas, Rect::new(area.x, area.y, area.width, 1));
        self.console(canvas, body);
        let mut links = Vec::new();
        if !matches!(self.overlay, Overlay::None) {
            // What opens over the console takes every press; a press beside it closes it.
            let (canvas_color, soft) = (self.palette.canvas(), self.palette.muted.fg.unwrap_or(self.palette.canvas()));
            canvas.dim(body, canvas_color, soft, 0.55);
            canvas.hits.clear();
            canvas.hit(body, Target::Backdrop);
            match &self.overlay {
                Overlay::Settings { .. } => {
                    let (entries, focus) = self.settings_entries();
                    let footer = self.settings_footer();
                    self.sheet(canvas, body, "Settings", &entries, Some(focus), footer);
                }
                Overlay::Details => {
                    let entries = self.details_entries();
                    self.sheet(canvas, body, "Details", &entries, None, Vec::new());
                }
                Overlay::Help => {
                    let entries = self.help_entries();
                    self.sheet(canvas, body, "Keys", &entries, None, Vec::new());
                }
                Overlay::Chooser(_) => {
                    let (entries, focus) = self.chooser_entries();
                    let title = self.chooser_title();
                    self.sheet(canvas, body, &title, &entries, Some(focus), Vec::new());
                }
                Overlay::SignIn(_) => links = self.sign_in_card(canvas, body),
                Overlay::Connect(_) => self.connect_card(canvas, body),
                Overlay::None => {}
            }
        }
        self.bottom_bar(canvas, Rect::new(area.x, area.bottom() - 1, area.width, 1));
        links
    }

    /// The brand and the server tested, its readiness at the right.
    fn top_bar(&self, canvas: &mut Canvas, area: Rect) {
        let palette = &self.palette;
        canvas.fill(area, palette.surface);
        let host = self.config.url.as_ref().map(format::authority);
        let brand = Line::styled("◆", palette.stage(Stage::Latency))
            .and(" Graphite Meter", palette.heading)
            .and("   ", Style::default())
            .and(host.unwrap_or_default(), palette.muted);
        canvas.line(area.x + 1, area.y, area.width.saturating_sub(2), &brand);
        let (dot, text, style) = self.readiness_line();
        let hovered = self.hover == Some(Target::Server);
        let readiness = dot
            .and(" ", Style::default())
            .and(text, if hovered { palette.text } else { style });
        let width = readiness.width() as u16;
        let x = area.right().saturating_sub(width + 1);
        if x > area.x + brand.width() as u16 + 2 {
            canvas.line(x, area.y, width, &readiness);
            canvas.hit(Rect::new(x, area.y, width, 1), Target::Server);
        }
    }

    /// The selected servers' readiness as the top bar shows it: a dot and its words.
    fn readiness_line(&self) -> (Line, String, Style) {
        let palette = &self.palette;
        let names = self.labels().join(", ");
        let dot = |style: Style| Line::styled("●", style);
        match () {
            _ if self.config.url.is_none() => (Line::styled("○", palette.muted), "No server".into(), palette.muted),
            _ if self.checking() && !self.running() => {
                (Line::styled(self.spinner(), palette.accent), "Checking".into(), palette.muted)
            }
            _ if self.view.check == Check::SignIn || self.view.sign_in.is_some() => {
                (dot(palette.warn), "Sign in".into(), palette.warn)
            }
            _ if matches!(self.view.check, Check::Failed(_)) || self.view.servers.iter().all(|s| s.path.is_err()) => {
                let unreached = if self.view.servers.is_empty() { "Failed".into() } else { names };
                (dot(Style { bold: false, ..palette.err }), unreached, palette.text)
            }
            _ => {
                let readiness: Vec<_> = self.view.servers.iter().map(|server| self.readiness(server)).collect();
                let ready = readiness.iter().all(|state| matches!(state, Readiness::Ready));
                let tone = if ready || self.running() { palette.ok } else { palette.warn };
                (dot(tone), names, palette.text)
            }
        }
    }

    /// The run's state, time and bytes at the left; the keys of what is on top at the right.
    fn bottom_bar(&self, canvas: &mut Canvas, area: Rect) {
        let palette = &self.palette;
        canvas.fill(area, palette.surface);
        let (word, tone) = self.status();
        let mut status = Line::styled(word, tone.bold());
        if let Some(run) = self
            .view
            .run
            .as_ref()
            .filter(|run| run.at.is_some() && !self.view.starting)
        {
            let elapsed = match run.outcome {
                Some(_) => run.elapsed,
                None => run.at.map_or_default(|at| self.now.saturating_duration_since(at)),
            };
            status = status.and(format!("   {}", format::duration(elapsed)), palette.text);
            status = status.and(format!("   {}", format::bytes(run.moved as u64)), palette.text);
            if let Some(left) = self.left() {
                status = status.and(format!("   {} left", format::duration(left)), palette.muted);
            }
        }
        canvas.line(area.x + 1, area.y, area.width.saturating_sub(2), &status);
        let bindings = self.bar_keys();
        let mut caps: Vec<(Line, Action)> = Vec::new();
        for binding in &bindings {
            let Some((key, mut does)) = binding.help else { continue };
            if binding.action == Action::Start && self.view.run.is_some() {
                does = "run again";
            }
            caps.push((self.cap(key, does, self.hover == Some(Target::Cap(binding.action))), binding.action));
        }
        let room = area.width.saturating_sub(status.width() as u16 + 6);
        while caps.len() > 1 && caps.iter().map(|(cap, _)| cap.width() as u16 + 2).sum::<u16>() > room {
            caps.remove(caps.len() - 2);
        }
        let total: u16 = caps.iter().map(|(cap, _)| cap.width() as u16 + 2).sum();
        let mut x = area.right().saturating_sub(total + 1);
        // A notice takes the bar's middle while it lasts, the build otherwise.
        let free = (area.x + status.width() as u16 + 4, x.saturating_sub(2));
        match &self.toast {
            Some(toast) if free.1 > free.0 + 10 => {
                let room = free.1 - free.0;
                let text = toast.text.clone().fit(usize::from(room.saturating_sub(4)));
                let width = text.width() as u16 + 4;
                let at = Rect::new(free.0 + (room - width) / 2, area.y, width, 1);
                canvas.fill(at, palette.raised);
                canvas.line(at.x + 2, at.y, width - 4, &text);
            }
            _ => {
                let version = Line::styled(format!("Client {VERSION}"), palette.muted);
                if free.1 > free.0 + version.width() as u16 + 2 {
                    canvas.line(x - version.width() as u16 - 3, area.y, version.width() as u16, &version);
                }
            }
        }
        for (cap, action) in caps {
            let width = cap.width() as u16;
            canvas.line(x, area.y, width, &cap);
            canvas.hit(Rect::new(x, area.y, width, 1), Target::Cap(action));
            x += width + 2;
        }
    }

    /// The keys the bottom bar names for what is on top.
    fn bar_keys(&self) -> Vec<keys::Binding> {
        if self.editor().is_some() {
            return keys::listed(keys::EDIT, |_| true);
        }
        let wanted: &[Action] = match &self.overlay {
            Overlay::None | Overlay::Connect(_) if self.running() => &[Action::Stop, Action::Details, Action::Help],
            Overlay::None | Overlay::Connect(_) => &[Action::Start, Action::Settings, Action::Details, Action::Help],
            Overlay::Settings { .. } => &[Action::Move, Action::Adjust, Action::Toggle, Action::Open, Action::Close],
            _ => &[
                Action::Move,
                Action::Scroll,
                Action::Select,
                Action::Apply,
                Action::Browse,
                Action::Cancel,
                Action::Close,
            ],
        };
        let listed = keys::listed(self.table(), |action| self.offers(action));
        let mut shown: Vec<_> = listed
            .into_iter()
            .filter(|binding| wanted.contains(&binding.action))
            .collect();
        shown.dedup_by_key(|binding| binding.action);
        shown
    }

    /// The state and its tone: the outcome's, the measuring stage's, or how the server stands.
    pub(super) fn status(&self) -> (&'static str, Style) {
        let palette = &self.palette;
        let run = self.view.run.as_ref();
        match run.map(|run| (run.outcome, &run.stage)) {
            _ if self.view.starting && self.stopping => ("Stopping", palette.muted),
            _ if self.view.starting => ("Checking paths", palette.muted),
            Some((Some(outcome), _)) => (format::outcome_label(outcome), palette.outcome(outcome)),
            Some((None, _)) if self.stopping => ("Stopping", palette.muted),
            Some((None, Some(Running::Measuring { plan, .. }))) => {
                (format::label(plan.stage), palette.stage(plan.stage))
            }
            Some((None, Some(Running::Warmup(_)))) => ("Warmup", palette.muted),
            Some((None, None)) => ("Checking paths", palette.muted),
            None if self.config.url.is_none() => ("Not started", palette.muted),
            None => match &self.overlay {
                Overlay::SignIn(sign_in) if sign_in.opened => ("Checking sign-in", palette.muted),
                Overlay::SignIn(_) => ("Sign in", palette.warn),
                _ if self.view.check == Check::SignIn => ("Sign in", palette.warn),
                _ if self.could_not_start() => ("Test could not start", palette.warn),
                _ => ("Not started", palette.muted),
            },
        }
    }

    /// A sheet titled `title` at the right of `area`, scrolled to keep the `focus` entry shown.
    fn sheet(
        &mut self,
        canvas: &mut Canvas,
        area: Rect,
        title: &str,
        entries: &[Entry],
        focus: Option<usize>,
        footer: Vec<Line>,
    ) {
        let palette = self.palette;
        let width = if area.width < 96 { area.width } else { (area.width * 2 / 5).clamp(56, 80) };
        let sheet = Rect::new(area.right() - width, area.y, width, area.height);
        canvas.clear(sheet);
        canvas.fill(sheet, palette.surface);
        canvas.hit(sheet, Target::Sheet);
        let inner = width.saturating_sub(4);
        canvas.line(sheet.x + 2, sheet.y + 1, inner, &Line::styled(title, palette.heading));
        let close = self.cap("esc", "close", self.hover == Some(Target::Cap(Action::Close)));
        let close_width = close.width() as u16;
        canvas.line(sheet.right() - close_width - 2, sheet.y + 1, close_width, &close);
        canvas.hit(
            Rect::new(sheet.right() - close_width - 2, sheet.y + 1, close_width, 1),
            Target::Cap(Action::Close),
        );
        // Every entry as the lines it takes, and where the focused one starts.
        let mut lines: Vec<SheetLine> = Vec::new();
        let mut focused_line = None;
        for (index, entry) in entries.iter().enumerate() {
            match entry {
                Entry::Heading(heading) => {
                    if !lines.is_empty() {
                        lines.push((Line::default(), None));
                    }
                    lines.push((Line::styled(heading, palette.heading), None));
                }
                Entry::Item { label, value, target } => {
                    if focus == Some(index) {
                        focused_line = Some(lines.len());
                    }
                    let gap = usize::from(inner).saturating_sub(label.width() + value.width()).max(2);
                    let row = label.clone().and(" ".repeat(gap), Style::default()).with(value.clone());
                    lines.push((row.fit(usize::from(inner)), Some((index, *target))));
                }
                Entry::Text(text) => {
                    for part in fill(&text.text(), usize::from(inner)) {
                        lines.push((Line::styled(part, text.0.first().map_or(palette.muted, |span| span.style)), None));
                    }
                }
                Entry::Gap => lines.push((Line::default(), None)),
            }
        }
        let footer_lines: Vec<Line> = footer
            .iter()
            .flat_map(|line| {
                fill(&line.text(), usize::from(inner))
                    .into_iter()
                    .map(|part| Line::styled(part, palette.muted))
            })
            .collect();
        let top = sheet.y + 3;
        let bottom = sheet
            .bottom()
            .saturating_sub(footer_lines.len() as u16 + u16::from(!footer_lines.is_empty()) + 1);
        let shown = usize::from(bottom.saturating_sub(top));
        self.rows = shown;
        if let Some(line) = focused_line {
            self.scroll = self.scroll.max((line + 1).saturating_sub(shown)).min(line);
        }
        self.scroll = self.scroll.min(lines.len().saturating_sub(shown));
        for (offset, (line, item)) in lines.iter().skip(self.scroll).take(shown).enumerate() {
            let y = top + offset as u16;
            let row = Rect::new(sheet.x + 1, y, width - 2, 1);
            if let Some((index, target)) = item {
                let lit = focus == Some(*index);
                let hovered = target.is_some() && self.hover == *target;
                if lit || hovered {
                    canvas.fill(row, if lit { palette.selected } else { palette.hover });
                }
                if lit {
                    canvas.line(sheet.x + 1, y, 1, &Line::styled("▌", palette.accent));
                }
                if let Some(target) = target {
                    canvas.hit(row, *target);
                }
            }
            canvas.line(sheet.x + 2, y, inner, line);
        }
        let more = lines.len() > self.scroll + shown;
        if more || self.scroll > 0 {
            let hint = match (self.scroll > 0, more) {
                (true, true) => "↑↓ more",
                (true, false) => "↑ more",
                _ => "↓ more",
            };
            canvas.right(sheet.x + 2, bottom, inner, &Line::styled(hint, palette.muted));
        }
        for (offset, line) in footer_lines.iter().enumerate() {
            canvas.line(sheet.x + 2, bottom + 1 + offset as u16, inner, line);
        }
    }

    /// A card centred over the console, `height` rows tall at most, its ground filled; its inner area.
    fn card_over(&self, canvas: &mut Canvas, area: Rect, height: u16) -> Rect {
        let (width, height) = (area.width.saturating_sub(4).min(76), height.min(area.height));
        let card = Rect::new(area.x + (area.width - width) / 2, area.y + (area.height - height) / 3, width, height);
        canvas.clear(card);
        canvas.fill(card, self.palette.surface);
        canvas.hit(card, Target::Sheet);
        Rect::new(card.x + 3, card.y + 1, width.saturating_sub(6), height.saturating_sub(2))
    }

    /// The server's address asked for: the field, and how its check stands.
    fn connect_card(&self, canvas: &mut Canvas, area: Rect) {
        let palette = &self.palette;
        let inner = self.card_over(canvas, area, 13);
        canvas.line(inner.x, inner.y, inner.width, &Line::styled("Connect to a server", palette.heading));
        let lead = "Enter the address of your Graphite Meter server, or paste it.";
        canvas.line(inner.x, inner.y + 2, inner.width, &Line::styled(lead, palette.muted));
        let field = Rect::new(inner.x, inner.y + 4, inner.width, 1);
        canvas.fill(field, palette.raised);
        if let Some(editor) = self.editor() {
            canvas.line(field.x + 1, field.y, field.width - 2, &editor.line(usize::from(field.width - 2), palette));
        }
        let host = self.config.url.as_ref().map(format::authority).unwrap_or_default();
        let failure = match &self.view.check {
            Check::Failed(failure) => Some(failure.text.clone()),
            _ if self.could_not_start() => {
                let failure = self.view.servers.iter().find_map(|server| server.path.as_ref().err());
                Some(failure.map_or("No test server answered".into(), |failure| failure.text.clone()))
            }
            _ => None,
        };
        let warn = |text: String| Line::styled(text, palette.warn);
        let [said, next] = match (self.editor().and_then(|editor| editor.error.clone()), failure) {
            (Some(error), _) => [Line::styled(error, Style { bold: false, ..palette.err }), Line::default()],
            _ if self.checking() && !host.is_empty() => [
                Line::styled(self.spinner(), palette.accent).and(format!(" Checking {host}…"), palette.muted),
                Line::default(),
            ],
            (None, Some(text)) => {
                let said = match text == crate::net::UNREACHABLE {
                    true => warn(format!("Could not reach {host}.")),
                    false => warn(format!("{host}: {text}")),
                };
                [
                    said,
                    Line::styled("Check the address and that the server is running; Enter tries again.", palette.muted),
                ]
            }
            (None, None) => [Line::default(), Line::default()],
        };
        canvas.line(inner.x, inner.y + 6, inner.width, &said.fit(usize::from(inner.width)));
        canvas.line(inner.x, inner.y + 7, inner.width, &next.fit(usize::from(inner.width)));
        // Remembering is the user's choice: the switch, lit under the pointer, and its key.
        let switch = Rect::new(inner.x, inner.y + 9, inner.width, 1);
        if self.hover == Some(Target::Remember) {
            canvas.fill(Rect::new(switch.x - 1, switch.y, switch.width + 2, 1), palette.hover);
        }
        let remember = Line::styled("Remember this server  ", palette.text).with(self.switch_mark(self.remembering));
        canvas.line(switch.x, switch.y, switch.width, &remember);
        canvas.right(switch.x, switch.y, switch.width, &Line::styled("tab", palette.muted));
        canvas.hit(switch, Target::Remember);
        let keys = Line::styled("enter", palette.text)
            .and(" connects    ", palette.muted)
            .and("esc", palette.text)
            .and(" closes", palette.muted);
        canvas.line(inner.x, inner.y + 10, inner.width, &keys);
    }

    /// The sign-in card: the code to match, the time waited and left, and the page's address as a link.
    fn sign_in_card(&self, canvas: &mut Canvas, area: Rect) -> Vec<Link> {
        let (Overlay::SignIn(sign_in), Some(prompt)) = (&self.overlay, &self.view.sign_in) else {
            return Vec::new();
        };
        let palette = &self.palette;
        let width = usize::from(area.width.saturating_sub(10).min(70));
        let url: Vec<char> = prompt.url.chars().collect();
        let wrapped: Vec<String> = url.chunks(width.max(1)).map(|chunk| chunk.iter().collect()).collect();
        let inner = self.card_over(canvas, area, 12 + wrapped.len() as u16);
        canvas.line(
            inner.x,
            inner.y,
            inner.width,
            &Line::styled(format!("Sign in to {}", prompt.issuer), palette.heading),
        );
        let status = match sign_in.opened {
            false => Line::styled("Open the sign-in page and check that it shows this code.", palette.muted),
            true => Line::styled(self.spinner(), palette.accent).and(" Waiting for approval…", palette.muted),
        };
        canvas.line(inner.x, inner.y + 2, inner.width, &status);
        let code = Rect::new(inner.x, inner.y + 4, prompt.code.chars().count() as u16 + 6, 3);
        canvas.fill(code, palette.raised);
        canvas.line(code.x + 3, code.y + 1, code.width - 6, &Line::styled(&prompt.code, palette.value));
        let waited = self.now.saturating_duration_since(sign_in.since);
        let left = prompt.deadline.into_std().saturating_duration_since(self.now);
        let left = std::time::Duration::from_secs(left.as_secs_f64().round() as u64);
        let times = format!("Waited {} · expires in {}", format::duration(waited), format::stage_time(left));
        canvas.line(inner.x, inner.y + 8, inner.width, &Line::styled(times, palette.muted));
        let mut links = Vec::new();
        for (index, part) in wrapped.into_iter().enumerate() {
            let (y, text) = (inner.y + 10 + index as u16, Line::styled(part, palette.accent));
            if y >= inner.bottom() {
                break;
            }
            canvas.line(inner.x, y, inner.width, &text);
            canvas.hit(Rect::new(inner.x, y, text.width() as u16, 1), Target::Link);
            links.push(Link { row: y, column: inner.x, url: prompt.url.clone(), text });
        }
        links
    }

    /// Every key, grouped by where it works.
    fn help_entries(&self) -> Vec<Entry> {
        let groups: [(&str, &[keys::Binding]); 5] = [
            ("Console", keys::CONSOLE),
            ("Settings", keys::SETTINGS_SHEET),
            ("Details and keys", keys::DETAILS_SHEET),
            ("Test servers", keys::CHOOSER),
            ("Typing", keys::EDIT),
        ];
        let mut entries = Vec::new();
        for (title, table) in groups {
            entries.push(Entry::Heading(title.into()));
            for binding in table.iter().filter(|binding| binding.help.is_some()) {
                let (key, does) = binding.help.unwrap_or_default();
                let label = Line::styled(text::capitalized(does), self.palette.text);
                entries.push(Entry::Item {
                    label,
                    value: Line::styled(key, self.palette.value),
                    target: None,
                });
            }
        }
        entries.push(Entry::Gap);
        entries.push(Entry::Text(Line::styled(
            "Click the run key, a stage's chip or a key below to press it. Hold Shift to select text with the mouse.",
            self.palette.muted,
        )));
        entries
    }

    /// The servers, paths, results and build, as label and value under their headings.
    fn details_entries(&self) -> Vec<Entry> {
        let palette = &self.palette;
        let item = |label: &str, value: String| Entry::Item {
            label: Line::styled(label, palette.muted),
            value: Line::styled(value, palette.text),
            target: None,
        };
        let mut entries = Vec::new();
        let address = self.config.url.as_ref().map_or(MISSING.into(), ToString::to_string);
        for server in self.view.servers.iter() {
            entries.push(Entry::Heading(match self.several() {
                true => server.name.clone(),
                false => "Tested server".into(),
            }));
            entries.push(item("Name", server.name.clone()));
            entries.push(item("Address", server.origin.to_string()));
            if !server.location.is_empty() {
                entries.push(item("Location", server.location.clone()));
            }
            entries.push(item("Status", self.readiness(server).label().into()));
            match &server.path {
                Ok(paths) => {
                    entries.push(item("Throughput path", format::throughput_path(&paths.throughput)));
                    entries.push(item("Streams", format::streams(&self.config, &paths.throughput)));
                    let latency = paths.latency.as_ref().map_or(MISSING.into(), format::latency_path);
                    entries.push(item("Latency path", latency));
                    if let Some(rtt) = paths.idle_rtt {
                        entries.push(item("Idle latency", round_trip(rtt)));
                    }
                    entries.push(item("IP family", format!("IPv{}", paths.ip_family.version())));
                }
                Err(failure) => entries.push(Entry::Text(Line::styled(&failure.text, palette.warn))),
            }
            if let Some(run) = &self.view.run {
                let results = run
                    .results
                    .iter()
                    .filter(|result| !result.stage.directions().is_empty());
                for result in results {
                    let own = result.servers.iter().find(|own| own.server == server.id);
                    let rate: f64 = own.map_or(0.0, |own| {
                        own.throughput
                            .down
                            .iter()
                            .chain(own.throughput.up.iter())
                            .filter_map(|t| t.rate)
                            .map(|r| r.mean)
                            .sum()
                    });
                    if rate > 0.0 {
                        let label = format!("{} {}", icon(result.stage), format::label(result.stage));
                        entries.push(item(&label, format::rate(rate)));
                    }
                }
            }
        }
        if self.view.servers.is_empty() {
            entries.push(Entry::Heading("Server".into()));
            entries.push(item("Server address", address));
            entries.push(Entry::Text(Line::styled("Its test servers show here once it answers.", palette.muted)));
        }
        if let Some(run) = self.view.run.as_ref().filter(|run| !run.issues.is_empty()) {
            entries.push(Entry::Heading("Issues".into()));
            for (stage, issue) in &run.issues {
                let at = issue.at.saturating_duration_since(run.at.unwrap_or(issue.at));
                let name = self.name(Some(&issue.server));
                let label = format!("{} at {}", format::label(*stage), format::duration(at));
                entries.push(item(&label, format!("{name}: {}", issue.failure.reason.label())));
            }
        }
        let latency = Stage::ALL
            .iter()
            .filter_map(|&stage| Some((stage, self.population(stage)?)));
        let latency: Vec<_> = latency.filter(|(_, summary)| summary.replies > 0).collect();
        if !latency.is_empty() {
            entries.push(Entry::Heading(format!("Latency to {}", self.name(self.latency_server()))));
            for (stage, summary) in latency {
                let median = summary.p50.map_or(MISSING.into(), round_trip);
                let p95 = summary.p95.map_or(MISSING.into(), round_trip);
                let value = format!("Median {median} · P95 {p95} · {} replies", format::count(summary.replies));
                entries.push(item(format::compact_population(stage), value));
            }
        }
        entries.push(Entry::Heading("Build".into()));
        entries.push(item("Client", VERSION.into()));
        entries
    }

    /// Whether the chips' stage `stage` is on for the next run.
    pub(super) fn stage_on(&self, stage: Stage) -> bool {
        match self.running() {
            true => self.plan().iter().any(|(own, _)| *own == stage),
            false => self.config.stages.contains(&stage),
        }
    }
}
