//! Terminal rendering, separate from editing and command dispatch.
use super::setup::{Field, on_off, seconds};
use super::*;
use crate::model::{Point, ServerLatency, Stage};
use ratatui::style::Color;
use std::collections::VecDeque;

/// Go's terminal progress bar (OSC 9;4): indeterminate while paths are checked, then the share of stage time done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Progress {
    Checking,
    Done(u8),
}

/// The window title and progress bar Go's TUI sets, written when they change and cleared on exit.
#[derive(Default)]
pub(super) struct Chrome {
    title: String,
    progress: Option<Progress>,
}

impl Chrome {
    pub(super) fn show(&mut self, title: String, progress: Option<Progress>) -> io::Result<()> {
        use std::io::Write;
        let sequences = self.update(title, progress);
        if sequences.is_empty() {
            return Ok(());
        }
        let mut stdout = io::stdout().lock();
        stdout.write_all(sequences.as_bytes())?;
        stdout.flush()
    }

    fn update(&mut self, title: String, progress: Option<Progress>) -> String {
        let mut sequences = String::new();
        if title != self.title {
            sequences.push_str(&format!("\x1b]2;{title}\x07"));
            self.title = title;
        }
        if progress != self.progress {
            sequences.push_str(&match progress {
                None => "\x1b]9;4;0\x07".to_owned(),
                Some(Progress::Checking) => "\x1b]9;4;3\x07".to_owned(),
                Some(Progress::Done(percent)) => format!("\x1b]9;4;1;{percent}\x07"),
            });
            self.progress = progress;
        }
        sequences
    }
}

impl Drop for Chrome {
    fn drop(&mut self) {
        let _ = self.show(String::new(), None);
    }
}

impl Ui {
    /// Go's window title.
    pub(super) fn title(&self) -> String {
        format!("Graphite Meter · {}", self.status().0)
    }

    /// Go counts planned stage time; every ended stage counts, where Go skips partial and failed ones.
    pub(super) fn progress(&self) -> Option<Progress> {
        if !self.live || !self.active() {
            return None;
        }
        if !self.snapshot.phase.live() || !self.snapshot.started() {
            return Some(Progress::Checking);
        }
        let duration = |stage: Stage| self.requested.duration(stage).as_secs_f64();
        let total: f64 = self.requested.stages.iter().map(|stage| duration(*stage)).sum();
        let mut done: f64 = self.snapshot.results.iter().map(|result| duration(result.stage)).sum();
        if let (Phase::Measuring, Some(stage)) = (self.snapshot.phase, self.snapshot.stage) {
            done += self.elapsed().as_secs_f64().min(duration(stage));
        }
        Some(Progress::Done((done / total.max(1.0) * 100.0).min(100.0) as u8))
    }

    pub(super) fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < 40 || area.height < 12 {
            frame.render_widget(
                Paragraph::new("Enlarge the terminal to at least 40×12.").wrap(Wrap { trim: true }),
                area,
            );
            return;
        }
        let help: Vec<_> = if self.help {
            keys::full_help().collect()
        } else {
            Vec::new()
        };
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(if self.help { 1 } else { 4 }),
            Constraint::Length(if self.help { 1 + help.len() as u16 } else { 2 }),
        ])
        .areas(area.inner(Margin {
            horizontal: 1,
            vertical: 1,
        }));
        frame.render_widget(self.header(usize::from(header.width)), header);
        self.body_scroll.hidden = 0;
        if self.live {
            self.draw_live(frame, body);
        } else {
            self.draw_setup(frame, body);
        }
        frame.render_widget(self.footer(usize::from(footer.width), &help), footer);
        match self.popup {
            Popup::Details => self.draw_details(frame),
            Popup::Servers => self.draw_servers(frame),
            Popup::None => {}
        }
        if let Some(edit) = &self.edit {
            self.draw_edit(frame, edit);
        }
        if self.snapshot.auth.is_some() {
            self.draw_auth(frame);
        }
    }

    /// The badge and status pill, then the version beside the catalogue or the run's servers.
    fn header(&self, width: usize) -> Paragraph<'static> {
        let (status, background) = self.status();
        let status = format!(" {} ", safe_text_width(status, (width / 2).saturating_sub(4)));
        let title = " Graphite Meter ";
        let spacer = width.saturating_sub(title.width() + status.width());
        let badge = Style::new().fg(self.theme.inverse).add_modifier(Modifier::BOLD);
        let version = format!("native client {}  ", crate::VERSION);
        let context = if self.live && self.snapshot.started() {
            self.run_servers()
                .iter()
                .map(|server| server.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            self.config.url.clone()
        };
        let context = safe_text_width(&context, width.saturating_sub(version.width()));
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(title, badge.bg(self.theme.ink)),
                Span::raw(" ".repeat(spacer)),
                Span::styled(status, badge.bg(background)),
            ]),
            Line::from(vec![
                Span::styled(version, Style::new().fg(self.theme.muted)),
                Span::styled(context, Style::new().fg(self.theme.ink)),
            ]),
        ])
    }

    /// The notice, or the help grid under the hints; setup explains the focused row.
    fn footer(&self, width: usize, help: &[Vec<&'static str>]) -> Paragraph<'static> {
        let (mut notice, is_error) = self.notice();
        if !self.live && !is_error && self.notice.is_empty() && self.snapshot.phase == Phase::Setup {
            notice = self.field().explanation(&self.config);
        }
        let mut hints = self.short_help();
        if self.body_scroll.hidden > 0 && matches!(self.mode(), InputMode::Main | InputMode::Reset) {
            hints.insert(1, MORE.1);
        }
        let color = if is_error { self.theme.err } else { self.theme.muted };
        let mut lines = Vec::new();
        if !self.help {
            lines.push(Line::styled(safe_text_width(notice, width), Style::new().fg(color)));
        }
        lines.push(self.hints(&hints, width));
        lines.extend(help.iter().map(|row| self.hints(row, width)));
        Paragraph::new(lines)
    }

    fn draw_edit(&self, frame: &mut Frame, edit: &Edit) {
        let area = popup(frame.area(), 80, 7);
        frame.render_widget(Clear, area);
        let (before, cursor, after) = edit.viewport(usize::from(area.width.saturating_sub(6)).max(1));
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::raw(before),
                    Span::styled(cursor.to_string(), Style::new().add_modifier(Modifier::REVERSED)),
                    Span::raw(after),
                ]),
                Line::from(keys::hints(&EDIT).join(" · ")),
                Line::styled(safe_text(&self.notice, 200), Style::new().fg(self.theme.warn)),
            ])
            .block(panel(edit.field.term.label, self.theme)),
            area,
        );
    }

    /// Go's statusLabel and pill colour: setup states outside the run view, then the run's own.
    fn status(&self) -> (&'static str, ratatui::style::Color) {
        let snapshot = &self.snapshot;
        if !self.live {
            let label = if self.config.validate().is_err() {
                "Test cannot start"
            } else if snapshot.phase == Phase::Failed
                && !snapshot
                    .servers
                    .iter()
                    .any(|server| server.checked() && server.error.is_none())
            {
                "Test could not start"
            } else {
                "Not started"
            };
            return (label, self.theme.muted);
        }
        let background = match (snapshot.phase, snapshot.stage) {
            (Phase::Failed, _) if !snapshot.started() => return ("Test could not start", self.theme.muted),
            (Phase::Complete, _) => self.theme.ok,
            (Phase::Partial | Phase::Incomplete | Phase::Cancelled, _) => self.theme.warn,
            (Phase::Failed, _) => self.theme.err,
            (Phase::Measuring, Some(stage)) => self.theme.stage(stage),
            _ => self.theme.muted,
        };
        (crate::report::status(snapshot), background)
    }

    fn draw_auth(&mut self, frame: &mut Frame) {
        let auth = self.snapshot.auth.as_ref().expect("active approval");
        let area = popup(frame.area(), 100, 12);
        frame.render_widget(Clear, area);
        frame.render_widget(panel("Sign in", self.theme), area);
        let inner = area.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        let regions = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
        let width = usize::from(inner.width);
        frame.render_widget(
            Paragraph::new(safe_text_width(&format!("Origin: {}", auth.origin), width))
                .style(Style::new().fg(self.theme.muted)),
            regions[0],
        );
        frame.render_widget(
            Paragraph::new(safe_text_width(&format!("Match this code: {}", auth.code), width))
                .style(Style::new().fg(self.theme.text).add_modifier(Modifier::BOLD)),
            regions[1],
        );
        frame.render_widget(
            Paragraph::new("Browser URL · ↑/↓ scroll").style(Style::new().fg(self.theme.muted)),
            regions[2],
        );
        let url = safe_text(&auth.browser_url, MAX_TEXT);
        let lines = wrap_columns(&url, width.max(1));
        let remaining = auth.deadline.saturating_duration_since(tokio::time::Instant::now());
        frame.render_widget(
            Paragraph::new(format!(
                "waited {:.0} s · expires in {:.0} s",
                crate::net::AUTHORIZATION_TIMEOUT
                    .saturating_sub(remaining)
                    .as_secs_f64(),
                remaining.as_secs_f64()
            ))
            .style(Style::new().fg(self.theme.muted)),
            regions[3],
        );
        let offset = self.auth_scroll.clamp(lines.len(), usize::from(regions[4].height));
        frame.render_widget(
            Paragraph::new(lines)
                .scroll(offset)
                .style(Style::new().fg(self.theme.text)),
            regions[4],
        );
        frame.render_widget(Paragraph::new(self.hints(&keys::hints(&SIGN_IN), width)), regions[5]);
    }

    fn hints(&self, hints: &[&str], width: usize) -> Line<'static> {
        let mut spans = Vec::new();
        let mut used = 0;
        for hint in hints {
            let separator = if spans.is_empty() { "" } else { " · " };
            used += separator.width() + hint.width();
            if used > width {
                break;
            }
            let (key, action) = hint.split_at(hint.find(' ').unwrap_or(hint.len()));
            spans.extend([
                Span::styled(separator, Style::new().fg(self.theme.border)),
                Span::styled(key.to_owned(), Style::new().fg(self.theme.text)),
                Span::styled(action.to_owned(), Style::new().fg(self.theme.muted)),
            ]);
        }
        Line::from(spans)
    }

    fn draw_setup(&mut self, frame: &mut Frame, area: Rect) {
        let (fields_area, plan_area) = if frame.area().width >= 100 {
            let [left, right] = Layout::horizontal([Constraint::Percentage(50); 2]).areas(area);
            (left, right)
        } else if area.height >= 12 {
            let [top, bottom] = Layout::vertical([Constraint::Length(5), Constraint::Min(1)]).areas(area);
            (top, bottom)
        } else {
            (area, Rect::default())
        };
        let items: Vec<_> = self.fields().iter().map(|field| self.setup_row(field)).collect();
        frame.render_stateful_widget(
            List::new(items)
                .block(panel("Test setup", self.theme))
                .highlight_style(self.value().bg(self.theme.surface))
                .highlight_symbol("› "),
            fields_area,
            &mut self.rows,
        );
        if plan_area.width > 0 {
            let lines = self.plan();
            let width = usize::from(plan_area.width.saturating_sub(2));
            let text = lines
                .iter()
                .map(|line| safe_text_width(line, width))
                .collect::<Vec<_>>()
                .join("\n");
            let offset = self
                .body_scroll
                .clamp(lines.len(), usize::from(plan_area.height.saturating_sub(2)));
            frame.render_widget(
                Paragraph::new(text).scroll(offset).block(panel("Servers", self.theme)),
                plan_area,
            );
        }
    }

    /// A setup row: its group heading, if it opens one, then its label and value.
    fn setup_row(&self, field: &Field) -> ListItem<'static> {
        let value = match field.kind {
            Kind::Start => {
                let button = bold(self.theme.inverse).bg(self.theme.ink);
                return ListItem::new(Line::from(Span::styled(" Start test ", button)));
            }
            Kind::Advanced => return ListItem::new(format!("{} Advanced", if self.advanced { "⌄" } else { "›" })),
            Kind::Reset => return ListItem::new(field.term.label),
            _ => field.value(&self.config),
        };
        let value = if value.is_empty() {
            "Automatic".into()
        } else {
            safe_text(&value, 200)
        };
        let mut lines = Vec::new();
        if !field.heading.is_empty() {
            lines.push(Line::styled(field.heading, bold(self.theme.ink)));
        }
        lines.push(Line::from(vec![
            Span::raw(format!("{:<22} ", field.term.label)),
            Span::styled(value, bold(self.theme.text)),
        ]));
        ListItem::new(lines)
    }

    /// The Servers panel: the selection, its paths as checked, then the run's plan.
    fn plan(&self) -> Vec<String> {
        let selected = if self.config.servers.is_empty() {
            "Catalogue default selection".into()
        } else {
            safe_text(&self.config.servers.join(", "), 300)
        };
        let mut lines = vec![selected, String::new(), "Connection paths".into()];
        if self.awaiting || self.recheck.is_some() {
            lines.push("Checking selected servers".into());
        } else if self.config.preparation_key() != self.requested.preparation_key() {
            lines.push("Settings changed · verify again".into());
        } else {
            let checked = self
                .snapshot
                .servers
                .iter()
                .filter(|server| server.has_check_result())
                .take(MAX_SELECTED_SERVERS)
                .collect::<Vec<_>>();
            if checked.is_empty() || self.snapshot.phase == Phase::Checking {
                lines.push(
                    if self.snapshot.phase == Phase::Checking {
                        "Checking selected servers"
                    } else {
                        "Not checked"
                    }
                    .into(),
                );
            }
            for server in checked {
                let state = if server.error.is_some() { "Failed" } else { "Ready" };
                lines.push(format!("{} · {state}", safe_text(&server.name, 120)));
                if let Some(label) = server.throughput_label() {
                    lines.push(format!("  Download {label}"));
                }
                if let Some(label) = server.latency_label() {
                    lines.push(format!("  Latency {label}"));
                }
                if let Some(error) = &server.error {
                    lines.push(format!("  Unavailable: {}", safe_text(error, 120)));
                }
            }
        }
        lines.extend([String::new(), "Run order".into()]);
        lines.extend(
            self.config
                .stages
                .iter()
                .map(|stage| format!("  {}  {} s", stage.name(), seconds(self.config.duration(*stage)))),
        );
        lines.extend([
            String::new(),
            format!("Warmup: {} s", seconds(self.config.warmup)),
            format!("Loaded latency: {}", on_off(self.config.loaded_latency)),
            format!(
                "TLS verification: {}",
                if self.config.insecure { "DISABLED" } else { "enabled" }
            ),
        ]);
        lines
    }
    fn focused_latency(&self) -> Option<&crate::model::ServerLatency> {
        self.snapshot
            .server_latencies
            .iter()
            .find(|host| Some(host.id.as_str()) == self.latency_server())
    }
    fn latency_name<'a>(&'a self, focus: Option<&'a crate::model::ServerLatency>) -> &'a str {
        focus.map_or("unavailable", |host| self.server_name(&host.id))
    }
    fn server_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.snapshot
            .servers
            .iter()
            .find(|server| server.id == id)
            .map_or(id, |server| server.name.as_str())
    }

    /// The run's servers as Go's run details list them: every selected server the check reached.
    pub(super) fn run_servers(&self) -> Vec<&crate::model::ServerSummary> {
        self.snapshot
            .servers
            .iter()
            .filter(|server| server.has_check_result())
            .collect()
    }

    /// Go's testFields: the run's servers and paths, then its stream and timing settings.
    fn test_fields(&self, width: usize) -> Vec<Line<'static>> {
        let label = |name: &str| Span::styled(format!("{name:<11}"), Style::new().fg(self.theme.text));
        let missing = || crate::vocabulary::MISSING.to_owned();
        if !self.snapshot.started() {
            let value = if self.active() {
                "Checking paths…".into()
            } else {
                missing()
            };
            return vec![Line::from(vec![
                label("Servers"),
                Span::styled(value, Style::new().fg(self.theme.muted)),
            ])];
        }
        let servers = self.run_servers();
        let mut throughputs = Vec::new();
        for path in servers.iter().filter_map(|server| server.throughput_label()) {
            if !throughputs.contains(&path) {
                throughputs.push(path);
            }
        }
        let latency = servers
            .iter()
            .find(|server| Some(server.id.as_str()) == self.latency_server())
            .and_then(|server| server.latency_label());
        let mut names = servers
            .iter()
            .map(|server| server.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let target = servers.iter().rev().find_map(|server| server.throughput.as_ref());
        let mut streams = streams_label(&self.requested, target);
        if servers.len() > 1 {
            names.push_str(" (all servers)");
            streams = format!("per server · {streams}");
        }
        let timing = format!(
            "warmup {} · latency cadence {} · loaded cadence {}",
            setting(self.requested.warmup),
            cadence_label(self.requested.ping_interval),
            cadence_label(self.requested.loaded_ping_interval)
        );
        let throughput = if throughputs.is_empty() {
            missing()
        } else {
            throughputs.join(" / ")
        };
        let mut lines = Vec::new();
        for (name, value) in [
            ("Servers", names),
            ("Throughput", throughput),
            ("Latency", latency.unwrap_or_else(missing)),
            ("Streams", streams),
            ("Timing", timing),
        ] {
            let parts: Vec<_> = value.split(" · ").map(str::to_owned).collect();
            let wrapped = crate::report::wrap_parts(&parts, width.saturating_sub(11).max(12));
            for (index, line) in wrapped.into_iter().enumerate() {
                lines.push(Line::from(vec![
                    label(if index == 0 { name } else { "" }),
                    Span::styled(safe_text(&line, MAX_TEXT), self.value()),
                ]));
            }
        }
        lines
    }

    fn draw_live(&mut self, frame: &mut Frame, area: Rect) {
        if area.height < 14 {
            return self.draw_live_compact(frame, area);
        }
        let (results, title) = self.results(usize::from(area.width.saturating_sub(2)));
        let results_height = match results.len() {
            0 => 0,
            lines => (lines + 2).min(usize::from(u16::MAX)) as u16,
        };
        let track_height = if area.height < 24 {
            2
        } else if self.active() {
            6
        } else {
            4
        };
        let chart_height = area.height.saturating_sub(results_height + track_height);
        let wide = frame.area().width >= 100;
        let [track_area, timeline_area, results_area] = if wide {
            let [top, bottom] = Layout::vertical([Constraint::Min(9), Constraint::Length(results_height)]).areas(area);
            let [track, timeline] =
                Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(top);
            [track, timeline, bottom]
        } else {
            Layout::vertical([
                Constraint::Length(track_height),
                Constraint::Length(if chart_height >= 9 { chart_height } else { 0 }),
                Constraint::Min(1),
            ])
            .areas(area)
        };
        self.draw_track(frame, track_area, wide);
        if timeline_area.height >= 9 {
            self.draw_timeline(frame, timeline_area);
        }
        if results_height > 0 {
            let offset = self
                .body_scroll
                .clamp(results.len(), usize::from(results_area.height.saturating_sub(2)));
            frame.render_widget(
                Paragraph::new(results).scroll(offset).block(panel(&title, self.theme)),
                results_area,
            );
        }
    }

    /// The results panel's lines, each grid's stage labels in their stage's colour, and its title.
    fn results(&self, width: usize) -> (Vec<Line<'static>>, String) {
        let (grids, failures) = crate::report::results(&self.snapshot, self.latency_server(), width);
        let muted = Style::new().fg(self.theme.muted);
        let mut lines = Vec::new();
        for grid in &grids {
            for (index, line) in grid.iter().enumerate() {
                let line = safe_text(line, MAX_TEXT);
                let (label, cells) = line.split_at(line.find("  ").unwrap_or(line.len()));
                lines.push(if index == 0 || label.is_empty() {
                    Line::styled(line, muted)
                } else if let Some(stage) = crate::report::label_stage(label) {
                    Line::from(vec![
                        Span::styled(label.to_owned(), Style::new().fg(self.theme.stage(stage))),
                        Span::raw(cells.to_owned()),
                    ])
                } else {
                    Line::from(line)
                });
            }
        }
        let err = Style::new().fg(self.theme.err);
        lines.extend(
            failures
                .iter()
                .map(|failure| Line::styled(safe_text(failure, MAX_TEXT), err)),
        );
        let mut title = "Results".to_owned();
        if let Some(id) = self.latency_server().filter(|_| self.run_servers().len() > 1) {
            title.push_str(" · latency to ");
            title.push_str(&safe_text(self.server_name(id), 120));
        }
        (lines, title)
    }

    /// The stage track, two of its lines when the body is short. Go's wide Test panel puts the
    /// run's fields above it; a panel too short for both keeps the track.
    fn draw_track(&self, frame: &mut Frame, area: Rect, wide: bool) {
        let track = self.stage_track();
        if area.height == 2 {
            let index = self
                .snapshot
                .stage
                .and_then(|stage| self.requested.stages.iter().position(|planned| *planned == stage))
                .unwrap_or(0);
            let shown: Vec<_> = track.into_iter().skip(index).take(2).collect();
            return frame.render_widget(Paragraph::new(shown), area);
        }
        let mut lines = Vec::new();
        if wide {
            lines = self.test_fields(usize::from(area.width.saturating_sub(2)));
            lines.push(Line::default());
            if lines.len() + track.len() > usize::from(area.height.saturating_sub(2)) {
                lines.clear();
            }
        }
        lines.extend(track);
        frame.render_widget(Paragraph::new(lines).block(panel("Test", self.theme)), area);
    }

    /// Each planned stage's outcome, progress, or wait.
    fn stage_track(&self) -> Vec<Line<'static>> {
        let muted = Style::new().fg(self.theme.muted);
        let mut track = Vec::new();
        for stage in &self.requested.stages {
            let hue = Style::new().fg(self.theme.stage(*stage));
            let mut line = vec![Span::styled(format!("{:<14} ", stage.name()), hue)];
            if let Some(result) = self.snapshot.results.iter().find(|result| result.stage == *stage) {
                line.extend(self.outcome(result));
            } else if self.active() && self.snapshot.stage == Some(*stage) {
                match self.snapshot.phase {
                    Phase::Preparing => line.push(Span::styled("checking paths", muted)),
                    Phase::Warmup => line.push(Span::styled("warmup", muted)),
                    _ => {
                        let elapsed = self.elapsed().as_secs_f64();
                        let duration = self.requested.duration(*stage).as_secs_f64();
                        let filled = (elapsed / duration * 8.0).clamp(0.0, 8.0) as usize;
                        line.extend([
                            Span::styled("█".repeat(filled), hue),
                            Span::styled("░".repeat(8 - filled), muted),
                            Span::styled(format!(" {elapsed:.1} s"), self.value()),
                            Span::styled(format!(" / {duration:.0} s"), muted),
                        ]);
                    }
                }
            } else if !self.active() {
                let skipped = crate::model::StageStatus::Skipped.label();
                line.push(Span::styled(format!("{} {skipped}", crate::vocabulary::MISSING), muted));
            } else {
                let planned = self.requested.duration(*stage).as_secs();
                line.push(Span::styled(format!("○ {planned} s"), muted));
            }
            track.push(Line::from(line));
        }
        track
    }

    fn draw_live_compact(&self, frame: &mut Frame, area: Rect) {
        let mut lines = Vec::new();
        if self.active() {
            lines.push(format!(
                "{} · {:.1} s",
                self.snapshot.stage.map_or("Checking paths", Stage::name),
                self.elapsed().as_secs_f64()
            ));
            lines.push(format!(
                "Download {} · Upload {}",
                rate(self.shown_down),
                rate(self.shown_up)
            ));
            lines.push(format!(
                "Latency {} · {}",
                milliseconds(self.focused_latency().and_then(|host| host.latest_ms)),
                self.latency_name(self.focused_latency())
            ));
        }
        let mut lines: Vec<_> = lines
            .into_iter()
            .map(|line| Line::from(safe_text_width(&line, usize::from(area.width))))
            .collect();
        for result in self.snapshot.results.iter().rev().take(4) {
            let hue = Style::new().fg(self.theme.stage(result.stage));
            let mut line = vec![Span::styled(result.stage.name(), hue), Span::raw(": ")];
            line.extend(self.outcome(result));
            lines.push(Line::from(line));
        }
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn value(&self) -> Style {
        bold(self.theme.text)
    }

    fn outcome(&self, result: &crate::model::StageResult) -> Vec<Span<'static>> {
        let headline = if result.stage == Stage::Latency {
            milliseconds(
                self.result_latency(result)
                    .and_then(|host| host.median())
                    .map(|median| median as f64 / 1e6),
            )
        } else {
            [
                result.stage.downloads().then(|| rate(result.down_bps())),
                result.stage.uploads().then(|| rate(result.up_bps())),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" / ")
        };
        let muted = Style::new().fg(self.theme.muted);
        match self.snapshot.stage_status(result) {
            crate::model::StageStatus::Complete => vec![
                Span::styled("✓ ", Style::new().fg(self.theme.ok)),
                Span::styled(headline, self.value()),
            ],
            status @ crate::model::StageStatus::Partial => vec![
                Span::styled("! ", Style::new().fg(self.theme.warn)),
                Span::styled(headline, self.value()),
                Span::styled(format!(" {}", status.label()), muted),
            ],
            status => vec![
                Span::styled("✗ ", Style::new().fg(self.theme.err).add_modifier(Modifier::BOLD)),
                Span::styled(status.label(), muted),
            ],
        }
    }

    fn result_latency<'a>(
        &'a self,
        result: &'a crate::model::StageResult,
    ) -> Option<&'a crate::model::ServerLatencyResult> {
        let shown = self.latency_server();
        result
            .server_latencies
            .iter()
            .find(|host| Some(host.id.as_str()) == shown)
    }

    fn draw_timeline(&self, frame: &mut Frame, area: Rect) {
        let [readout, marks, throughput_area, latency_area] = Layout::vertical([
            Constraint::Length(if self.active() {
                if area.height < 12 { 1 } else { 2 }
            } else {
                0
            }),
            Constraint::Length(if area.height >= 12 { 1 } else { 0 }),
            Constraint::Percentage(50),
            Constraint::Min(1),
        ])
        .areas(area);
        if self.active() {
            let focus = self.focused_latency();
            frame.render_widget(
                Paragraph::new(format!(
                    "Download {} · Upload {} · Latency {}\nLatency to {}{}",
                    rate(self.shown_down),
                    rate(self.shown_up),
                    milliseconds(focus.and_then(|host| host.latest_ms)),
                    self.latency_name(focus),
                    if self.run_servers().len() > 1 {
                        " · l switches server"
                    } else {
                        ""
                    }
                )),
                readout,
            );
        }
        let total = self
            .requested
            .stages
            .iter()
            .map(|stage| self.requested.duration(*stage).as_secs_f64())
            .sum::<f64>()
            .max(1.0);
        frame.render_widget(Paragraph::new(self.stage_marks(total, area.width)), marks);
        let series = timeline_series(&self.snapshot, self.focused_latency(), self.theme);
        for ((series, area), latency) in series
            .into_iter()
            .zip([throughput_area, latency_area])
            .zip([false, true])
        {
            self.draw_chart(frame, area, series, latency, total);
        }
    }

    /// A chart over the planned stage time, its values scaled to readable units.
    fn draw_chart(&self, frame: &mut Frame, area: Rect, mut series: Vec<Series>, latency: bool, total: f64) {
        let max = series
            .iter()
            .flat_map(|(points, _)| points)
            .map(|(_, value)| *value)
            .fold(0.0_f64, f64::max);
        let (scale, units) = if latency {
            (1.0, "ms")
        } else {
            [(1.2e9, 1e9, "Gbit/s"), (1.2e6, 1e6, "Mbit/s"), (1.2e3, 1e3, "kbit/s")]
                .into_iter()
                .find(|(from, ..)| max >= *from)
                .map_or((1.0, "bit/s"), |(_, scale, units)| (scale, units))
        };
        let ceiling = axis_ceiling(max / scale);
        for (points, _) in &mut series {
            for (_, value) in points {
                *value /= scale;
            }
        }
        let datasets = series
            .iter()
            .map(|(points, color)| {
                Dataset::default()
                    .marker(Marker::Braille)
                    .graph_type(GraphType::Line)
                    .style(Style::new().fg(*color))
                    .data(points)
            })
            .collect::<Vec<_>>();
        let axis = Style::new().fg(self.theme.border);
        let muted = Style::new().fg(self.theme.muted);
        let top = format!("{} {units}", graphite_meter_core::format::speed(ceiling));
        frame.render_widget(
            Chart::new(datasets)
                .block(panel(if latency { "Latency · ms" } else { "Throughput" }, self.theme))
                .x_axis(
                    Axis::default()
                        .style(axis)
                        .bounds([0.0, total])
                        .labels([Span::styled("0 s", muted), Span::styled(format!("{total:.0} s"), muted)]),
                )
                .y_axis(Axis::default().style(axis).bounds([0.0, ceiling]).labels([
                    Span::styled(format!("{:>12}", "0"), muted),
                    Span::styled(format!("{top:>12}"), muted),
                ])),
            area,
        );
    }

    /// Each planned stage's name where it starts on the time axis, while there is room.
    fn stage_marks(&self, total: f64, width: u16) -> Line<'static> {
        let mut marks = vec![Span::raw(" ".repeat(14))];
        let mut marked = 0;
        let plot_width = usize::from(width.saturating_sub(16));
        let mut at = 0.0;
        for stage in &self.requested.stages {
            let column = (at / total * plot_width as f64) as usize;
            if column >= marked {
                let label = if *stage == Stage::Latency { "Idle" } else { stage.name() };
                marks.push(Span::raw(" ".repeat(column - marked)));
                marks.push(Span::styled(label, Style::new().fg(self.theme.stage(*stage))));
                marked = column + label.width();
            }
            at += self.requested.duration(*stage).as_secs_f64();
        }
        Line::from(marks)
    }

    fn draw_details(&mut self, frame: &mut Frame) {
        let area = popup(frame.area(), 84, frame.area().height.saturating_sub(2));
        let width = usize::from(area.width.saturating_sub(2));
        let details = if self.snapshot.started() {
            crate::report::details(&self.snapshot, self.latency_server(), width)
        } else {
            "Waiting for the first server report…".into()
        };
        let mut lines: Vec<_> = details
            .lines()
            .map(|line| Line::from(safe_text(line, MAX_TEXT)))
            .collect();
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Values",
            Style::new().fg(self.theme.ink).add_modifier(Modifier::BOLD),
        ));
        for term in crate::vocabulary::VALUES {
            lines.extend(wrap_columns(
                &format!("{} · {}", term.label, term.explanation),
                usize::from(area.width.saturating_sub(2)),
            ));
        }
        let offset = self
            .details_scroll
            .clamp(lines.len(), usize::from(area.height.saturating_sub(2)));
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(lines)
                .scroll(offset)
                .block(panel("Details · ↑/↓ scroll · d/Esc close", self.theme)),
            area,
        );
    }

    fn draw_servers(&mut self, frame: &mut Frame) {
        let area = popup(frame.area(), 100, 24);
        frame.render_widget(Clear, area);
        let items = self
            .snapshot
            .servers
            .iter()
            .take(MAX_SERVERS)
            .map(|server| {
                let selected = self.config.servers.contains(&server.id);
                let detail = server.error.clone().unwrap_or_else(|| {
                    if server.checked() {
                        server.connection_label()
                    } else {
                        "Not checked".into()
                    }
                });
                ListItem::new(vec![
                    Line::from(format!(
                        "{} {} · {}",
                        if selected { "[✓]" } else { "[ ]" },
                        safe_text(&server.name, 120),
                        safe_text(&server.id, 120)
                    )),
                    Line::styled(
                        format!("    {}", safe_text(&server.origin, 160)),
                        Style::new().fg(self.theme.muted),
                    ),
                    Line::styled(
                        format!("    {}", safe_text(&detail, 160)),
                        Style::new().fg(if server.error.is_some() {
                            self.theme.err
                        } else {
                            self.theme.muted
                        }),
                    ),
                ])
            })
            .collect::<Vec<_>>();
        if items.is_empty() {
            frame.render_widget(
                Paragraph::new("No catalogue yet. Esc returns to setup; v discovers servers.\nServer IDs can also be entered in setup.")
                    .wrap(Wrap { trim: true })
                    .block(panel("Servers", self.theme)),
                area,
            );
        } else {
            frame.render_stateful_widget(
                List::new(items)
                    .block(panel("Servers · Space toggle · Enter apply · maximum four", self.theme))
                    .highlight_style(self.value().bg(self.theme.surface)),
                area,
                &mut self.servers,
            );
        }
    }
}

fn wrap_columns(value: &str, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut columns = 0;
    for character in value.chars() {
        let next = cell_width(character);
        if !current.is_empty() && columns + next > width {
            lines.push(Line::from(std::mem::take(&mut current)));
            columns = 0;
        }
        current.push(character);
        columns += next;
    }
    lines.push(Line::from(current));
    lines
}

/// Go's streamsLabel for the lanes `Config::lanes` opens on the path.
fn streams_label(config: &Config, target: Option<&graphite_meter_core::discovery::ThroughputTarget>) -> String {
    use graphite_meter_core::discovery::{Protocol, ThroughputTransport};
    if config.streams > 0 {
        return format!("Forced · {} per direction", config.streams);
    }
    match target.map(|target| (target.transport, target.protocol, config.lanes(target))) {
        Some((ThroughputTransport::WebTransport, ..)) => "Automatic · 1 continuous stream per direction".into(),
        Some((_, Protocol::Http2 | Protocol::Http3, (down, up))) => {
            format!("Automatic · {down} download / {up} upload")
        }
        Some((_, Protocol::Http1, (down, _))) => format!("Automatic · up to {down} per direction"),
        _ => "Automatic".into(),
    }
}

/// Go's cadenceLabel.
fn cadence_label(interval: Duration) -> String {
    crate::vocabulary::cadence(interval).map_or_else(|| format!("Custom ({})", setting(interval)), Into::into)
}

/// Go's fmtSetting.
fn setting(duration: Duration) -> String {
    if duration < Duration::from_secs(1) {
        format!("{} ms", duration.as_millis())
    } else {
        format!("{} s", seconds(duration))
    }
}

fn bold(color: Color) -> Style {
    Style::new().fg(color).add_modifier(Modifier::BOLD)
}

/// A chart line: its points and their colour.
type Series = (Vec<(f64, f64)>, Color);

/// The run view's chart lines: the throughput chart's download and upload lines, then the shown
/// server's latency line. A line breaks where its value goes missing or its stage colour changes.
fn timeline_series(snapshot: &Snapshot, focus: Option<&ServerLatency>, theme: Theme) -> [Vec<Series>; 2] {
    let mut starts = Vec::new();
    let mut offset = Duration::ZERO;
    for result in &snapshot.results {
        starts.push((offset, result.stage));
        offset += result.elapsed;
    }
    starts.extend(snapshot.stage.map(|stage| (offset, stage)));
    let hue = |elapsed: Duration| {
        starts
            .iter()
            .rfind(|(start, _)| *start <= elapsed)
            .map_or(theme.text, |(_, stage)| theme.stage(*stage))
    };
    let lines = |points: &VecDeque<Point>, value: fn(&Point) -> Option<f64>| {
        let (mut series, mut segment, mut color) = (Vec::new(), Vec::new(), theme.text);
        for point in points {
            let value = value(point).filter(|value| value.is_finite() && *value >= 0.0);
            let point_hue = hue(point.elapsed);
            if !segment.is_empty() && (value.is_none() || point_hue != color) {
                series.push((std::mem::take(&mut segment), color));
            }
            if let Some(value) = value {
                color = point_hue;
                segment.push((point.elapsed.as_secs_f64(), value));
            }
        }
        if !segment.is_empty() {
            series.push((segment, color));
        }
        series
    };
    let history = &snapshot.history.points;
    let latency = focus.map_or(history, |host| &host.history.points);
    [
        [
            lines(history, |point| point.down_bps),
            lines(history, |point| point.up_bps),
        ]
        .concat(),
        lines(latency, |point| point.latency_ms),
    ]
}

fn axis_ceiling(value: f64) -> f64 {
    if value <= 0.0 {
        return 0.1;
    }
    let power = 10.0_f64.powf(value.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .find(|step| step * power >= value)
        .unwrap()
        * power
}

#[cfg(test)]
mod tests {
    use super::super::tests::{download_measurement, latency_result, probes, rows};
    use super::*;
    use crate::model::ServerSummary;
    use graphite_meter_core::discovery::{
        LatencyTarget, LatencyTransport, Protocol, ThroughputTarget, ThroughputTransport,
    };

    fn server(id: &str, name: &str, latency: LatencyTransport) -> ServerSummary {
        let origin = format!("https://{id}.example");
        ServerSummary {
            id: id.into(),
            name: name.into(),
            origin: origin.clone(),
            throughput: Some(ThroughputTarget {
                base_url: origin.clone(),
                transport: ThroughputTransport::FetchStream,
                protocol: Protocol::Http2,
            }),
            latency: Some(LatencyTarget {
                base_url: origin,
                transport: latency,
            }),
            error: None,
        }
    }

    fn measuring() -> Snapshot {
        Snapshot {
            phase: Phase::Measuring,
            stage: Some(Stage::Download),
            servers: vec![
                server("a", "Alpha", LatencyTransport::WebSocket),
                server("b", "Beta", LatencyTransport::WebTransport),
            ],
            participants: vec!["a".into(), "b".into()],
            latency_focus: Some("a".into()),
            ..Snapshot::default()
        }
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.spans.iter().map(|span| span.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn run_view_lists_servers_paths_streams_and_timing_like_go() {
        let mut ui = Ui::new(Config::default(), measuring());
        ui.live = true;
        assert_eq!(
            text(&ui.test_fields(200)),
            [
                "Servers    Alpha, Beta (all servers)",
                "Throughput Fetch streams · HTTP/2 · TLS",
                "Latency    WebSocket · HTTP/1.1 · TLS",
                "Streams    per server · Automatic · 1 download / 4 upload",
                "Timing     warmup 800 ms · latency cadence Reply-driven · loaded cadence Medium (250 ms)",
            ]
        );
        // The latency path follows the shown server, and a narrow panel wraps at Go's parts.
        ui.latency_pick = Some("b".into());
        assert_eq!(
            text(&ui.test_fields(40)),
            [
                "Servers    Alpha, Beta (all servers)",
                "Throughput Fetch streams · HTTP/2 · TLS",
                "Latency    WebTransport datagrams",
                "           HTTP/3 · TLS",
                "Streams    per server · Automatic",
                "           1 download / 4 upload",
                "Timing     warmup 800 ms",
                "           latency cadence Reply-driven",
                "           loaded cadence Medium (250 ms)",
            ]
        );
        ui.latency_pick = None;
        ui.snapshot.servers.truncate(1);
        (ui.requested.streams, ui.requested.warmup) = (3, Duration::from_millis(1500));
        ui.requested.ping_interval = Duration::from_secs(1);
        assert_eq!(
            text(&ui.test_fields(200)),
            [
                "Servers    Alpha",
                "Throughput Fetch streams · HTTP/2 · TLS",
                "Latency    WebSocket · HTTP/1.1 · TLS",
                "Streams    Forced · 3 per direction",
                "Timing     warmup 1.5 s · latency cadence Custom (1 s) · loaded cadence Medium (250 ms)",
            ]
        );

        ui.update(Snapshot {
            phase: Phase::Preparing,
            participants: Vec::new(),
            ..measuring()
        });
        assert_eq!(text(&ui.test_fields(200)), ["Servers    Checking paths…"]);
        ui.snapshot.phase = Phase::Failed;
        assert_eq!(text(&ui.test_fields(200)), ["Servers    —"]);
    }

    #[test]
    fn wide_test_panel_puts_the_fields_above_the_stage_track() {
        let mut ui = Ui::new(Config::default(), measuring());
        ui.live = true;
        let wide = rows(&mut ui, 140, 40);
        let servers = wide
            .iter()
            .position(|row| row.contains("│Servers    Alpha, Beta (all servers)"));
        let track = wide.iter().position(|row| row.contains("│Download "));
        assert!(
            servers.is_some_and(|servers| track.is_some_and(|track| servers < track)),
            "{wide:#?}"
        );
        assert!(
            wide.iter().any(|row| row.contains("│Timing     warmup 800 ms")),
            "{wide:#?}"
        );
        let narrow = rows(&mut ui, 90, 40);
        assert!(!narrow.iter().any(|row| row.contains("Servers    ")), "{narrow:#?}");
        assert!(narrow.iter().any(|row| row.contains("Download ")), "{narrow:#?}");
        // A panel too short for both keeps the whole stage track.
        ui.requested.stages.push(Stage::Bidirectional);
        let short = rows(&mut ui, 120, 20);
        assert!(!short.iter().any(|row| row.contains("Servers    ")), "{short:#?}");
        assert!(short.iter().any(|row| row.contains("│Bidirectional ")), "{short:#?}");
    }

    /// Both servers measured download and loaded latency.
    fn finished() -> Snapshot {
        let replies = probes(&[2_000_000; 4], 0);
        Snapshot {
            phase: Phase::Complete,
            stage: None,
            results: vec![crate::model::StageResult {
                stage: Stage::Download,
                elapsed: Duration::from_secs(1),
                down: Some(download_measurement()),
                server_latencies: vec![latency_result("a", replies), latency_result("b", replies)],
                ..Default::default()
            }],
            plan: vec![Stage::Download],
            ..measuring()
        }
    }

    #[test]
    fn results_title_names_the_latency_server_after_multi_server_runs() {
        let config = Config {
            stages: vec![Stage::Download],
            ..Config::default()
        };
        let mut ui = Ui::new(config, finished());
        ui.live = true;
        let title = |ui: &mut Ui| {
            rows(ui, 100, 30)
                .into_iter()
                .find(|row| row.contains("╭Results"))
                .unwrap_or_default()
        };
        assert!(
            title(&mut ui).contains("╭Results · latency to Alpha─"),
            "{}",
            title(&mut ui)
        );
        ui.latency_pick = Some("b".into());
        assert!(
            title(&mut ui).contains("╭Results · latency to Beta─"),
            "{}",
            title(&mut ui)
        );
        ui.latency_pick = None;
        ui.snapshot.servers.truncate(1);
        assert!(title(&mut ui).contains("╭Results──"), "{}", title(&mut ui));
    }

    #[test]
    fn setup_pill_says_when_the_test_cannot_or_could_not_start() {
        let unreachable = ServerSummary {
            throughput: None,
            latency: None,
            error: Some("Server could not be reached".into()),
            ..server("a", "Alpha", LatencyTransport::WebSocket)
        };
        let mut ui = Ui::new(
            Config::default(),
            Snapshot {
                phase: Phase::Failed,
                servers: vec![unreachable.clone()],
                ..Snapshot::default()
            },
        );
        assert_eq!(ui.status(), ("Test could not start", ui.theme.muted));
        assert!(rows(&mut ui, 100, 30)[1].ends_with(" Test could not start  "));
        // A server that is still ready leaves the test startable.
        ui.snapshot
            .servers
            .push(server("b", "Beta", LatencyTransport::WebSocket));
        assert_eq!(ui.status().0, "Not started");
        ui.config.stages.clear();
        assert_eq!(ui.status().0, "Test cannot start");
        // A run that failed before any server started reads the same; one that started failed.
        (ui.config, ui.live) = (Config::default(), true);
        ui.snapshot.servers = vec![unreachable];
        assert_eq!(ui.status(), ("Test could not start", ui.theme.muted));
        ui.snapshot.participants = vec!["a".into()];
        assert_eq!(ui.status(), ("Failed", ui.theme.err));
        // Returning to setup after a run no longer shows its outcome.
        ui.live = false;
        ui.snapshot.phase = Phase::Complete;
        assert_eq!(ui.status(), ("Not started", ui.theme.muted));
    }

    #[test]
    fn window_title_and_progress_bar_follow_the_run_like_go() {
        use crate::model::StageResult;
        let mut ui = Ui::new(Config::default(), Snapshot::default());
        assert_eq!(ui.title(), "Graphite Meter · Not started");
        assert_eq!(ui.progress(), None);
        ui.live = true;
        ui.update(Snapshot {
            phase: Phase::Preparing,
            ..Snapshot::default()
        });
        assert_eq!(ui.title(), "Graphite Meter · Checking paths");
        assert_eq!(ui.progress(), Some(Progress::Checking));
        // 4 s of latency and 5 s of download in a 24 s plan.
        let mut snapshot = measuring();
        snapshot.latest.elapsed = Duration::from_secs(5);
        snapshot.results.push(StageResult {
            stage: Stage::Latency,
            elapsed: Duration::from_secs(4),
            ..Default::default()
        });
        ui.update(snapshot);
        assert_eq!(ui.title(), "Graphite Meter · Download");
        assert_eq!(ui.progress(), Some(Progress::Done(37)));
        ui.snapshot.phase = Phase::Complete;
        assert_eq!(ui.title(), "Graphite Meter · Complete");
        assert_eq!(ui.progress(), None);

        let mut chrome = Chrome::default();
        assert_eq!(
            chrome.update("Graphite Meter · Checking paths".into(), Some(Progress::Checking)),
            "\x1b]2;Graphite Meter · Checking paths\x07\x1b]9;4;3\x07"
        );
        assert_eq!(
            chrome.update("Graphite Meter · Checking paths".into(), Some(Progress::Checking)),
            ""
        );
        assert_eq!(
            chrome.update("Graphite Meter · Download".into(), Some(Progress::Done(37))),
            "\x1b]2;Graphite Meter · Download\x07\x1b]9;4;1;37\x07"
        );
        assert_eq!(
            chrome.update("Graphite Meter · Complete".into(), None),
            "\x1b]2;Graphite Meter · Complete\x07\x1b]9;4;0\x07"
        );
        // Leaving clears the title the TUI set.
        assert_eq!(chrome.update(String::new(), None), "\x1b]2;\x07");
    }

    #[test]
    fn header_shows_the_version_beside_the_catalogue_or_the_run_servers() {
        let mut ui = Ui::new(Config::default(), measuring());
        let version = format!(" native client {}  ", crate::VERSION);
        assert!(rows(&mut ui, 100, 30)[2].starts_with(&format!("{version}http://127.0.0.1:7246 ")));
        ui.live = true;
        assert!(rows(&mut ui, 100, 30)[2].starts_with(&format!("{version}Alpha, Beta ")));
    }
}
