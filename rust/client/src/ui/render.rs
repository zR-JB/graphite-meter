//! Terminal rendering, separate from editing and command dispatch.
use super::setup::{on_off, seconds};
use super::*;
use crate::model::Stage;

impl Ui {
    pub(super) fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < 40 || area.height < 12 {
            frame.render_widget(
                Paragraph::new("Enlarge the terminal to at least 40×12.").wrap(Wrap { trim: true }),
                area,
            );
            return;
        }
        let area = area.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        let help: &[&str] = if self.help {
            &[
                "Tab/Shift-Tab focus · arrows change",
                "Enter edit/run · Space stage",
                "Esc close/stop · r run · v recheck",
                "s servers · u available · a auto",
                "d Details · l latency · o sign-in",
                "q quit · Ctrl-C stop · ? keys",
            ]
        } else {
            &[]
        };
        let regions = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(if self.help { 1 } else { 4 }),
            Constraint::Length(if self.help { 1 + help.len() as u16 } else { 2 }),
        ])
        .split(area);
        let status = crate::report::status(&self.snapshot);
        let status = safe_text_width(status, usize::from(regions[0].width / 2).saturating_sub(4));
        let title = " Graphite Meter ";
        let status_pill = format!(" {status} ");
        let spacer = usize::from(regions[0].width).saturating_sub(title.width() + status_pill.width());
        let status_background = match (self.snapshot.phase, self.snapshot.stage) {
            (Phase::Complete, _) => self.theme.ok,
            (Phase::Partial | Phase::Incomplete | Phase::Cancelled, _) => self.theme.warn,
            (Phase::Failed, _) => self.theme.err,
            (Phase::Measuring, Some(stage)) => self.theme.stage(stage),
            _ => self.theme.muted,
        };
        let badge = Style::new().fg(self.theme.inverse).add_modifier(Modifier::BOLD);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(title, badge.bg(self.theme.ink)),
                    Span::raw(" ".repeat(spacer)),
                    Span::styled(status_pill, badge.bg(status_background)),
                ]),
                Line::styled(
                    safe_text_width(
                        &if self.live {
                            self.snapshot
                                .servers
                                .iter()
                                .filter(|server| server.has_check_result())
                                .map(|server| server.name.as_str())
                                .collect::<Vec<_>>()
                                .join(" · ")
                        } else {
                            self.config.url.clone()
                        },
                        usize::from(regions[0].width),
                    ),
                    Style::new().fg(self.theme.ink),
                ),
            ]),
            regions[0],
        );
        if self.live {
            self.draw_live(frame, regions[1]);
        } else {
            self.draw_setup(frame, regions[1]);
        }
        let (mut notice, is_error) = self.notice();
        if !self.live && !is_error && self.notice.is_empty() && self.snapshot.phase == Phase::Setup {
            notice = self.fields()[self.rows.selected().unwrap_or(0)].explanation(&self.config);
        }
        let notice_color = if is_error { self.theme.err } else { self.theme.muted };
        let hints: Vec<&str> = if self.cancel == CancelState::Confirming {
            vec!["Esc confirm stop", "any key continue", "q quit"]
        } else if self.live {
            if self.active() {
                vec!["Esc stop", "d Details", "l Latency server", "? keys", "q quit"]
            } else {
                vec![
                    "Enter Run again",
                    "Esc setup",
                    "d Details",
                    "l Latency server",
                    "? keys",
                    "q quit",
                ]
            }
        } else if self.rows.selected() == Some(0) {
            vec![
                "Enter Start test",
                "↓ settings",
                "s servers",
                "v Recheck paths",
                "? keys",
                "q quit",
            ]
        } else {
            let field = self.fields()[self.rows.selected().unwrap_or(0)];
            use super::setup::Field;
            let action = if field.stage().is_some() || matches!(field, Field::LoadedLatency | Field::Insecure) {
                "Space toggle"
            } else {
                match field {
                    Field::Servers => "Enter choose servers",
                    Field::Advanced => "Enter show/hide",
                    Field::Url
                    | Field::ThroughputOrigin
                    | Field::LatencyOrigin
                    | Field::Streams
                    | Field::AutoStreams => "Enter edit",
                    Field::Warmup => "←/→ 0.1 s",
                    _ => "←/→ choose",
                }
            };
            vec![
                action,
                if field.stage().is_some() {
                    "←/→ 1 s"
                } else {
                    "Tab focus"
                },
                "r Start test",
                "? keys",
                "q quit",
            ]
        };
        let width = usize::from(regions[2].width);
        let mut lines = if self.help {
            Vec::new()
        } else {
            vec![Line::styled(
                safe_text_width(notice, width),
                Style::new().fg(notice_color),
            )]
        };
        lines.push(self.hints(&hints, width));
        lines.extend(
            help.iter()
                .map(|line| self.hints(&line.split(" · ").collect::<Vec<_>>(), width)),
        );
        frame.render_widget(Paragraph::new(lines), regions[2]);
        if self.popup == Popup::Details {
            self.draw_details(frame);
        }
        if self.popup == Popup::Servers {
            self.draw_servers(frame);
        }
        if let Some(edit) = &self.edit {
            let area = popup(frame.area(), 80, 7);
            frame.render_widget(Clear, area);
            let width = usize::from(area.width.saturating_sub(6)).max(1);
            let (before, cursor, after) = edit.viewport(width);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::raw(before),
                        Span::styled(cursor.to_string(), Style::new().add_modifier(Modifier::REVERSED)),
                        Span::raw(after),
                    ]),
                    Line::from("Enter apply · Esc discard · ←/→ Home/End move"),
                    Line::styled(safe_text(&self.notice, 200), Style::new().fg(self.theme.warn)),
                ])
                .block(panel(edit.field.label(), self.theme)),
                area,
            );
        }
        if self.snapshot.auth.is_some() {
            self.draw_auth(frame);
        }
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
        let visible = usize::from(regions[4].height);
        self.auth_scroll = self
            .auth_scroll
            .min(lines.len().saturating_sub(visible).min(u16::MAX as usize) as u16);
        frame.render_widget(
            Paragraph::new(lines)
                .scroll((self.auth_scroll, 0))
                .style(Style::new().fg(self.theme.text)),
            regions[4],
        );
        frame.render_widget(
            Paragraph::new(self.hints(&["Enter/Space/o open", "Esc cancel", "q quit"], width)),
            regions[5],
        );
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
        let content = area;
        let (fields_area, plan_area) = if frame.area().width >= 100 {
            let regions = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(content);
            (regions[0], regions[1])
        } else if content.height >= 12 {
            let regions = Layout::vertical([Constraint::Length(5), Constraint::Min(1)]).split(content);
            (regions[0], regions[1])
        } else {
            (content, Rect::default())
        };
        let items = self
            .fields()
            .iter()
            .map(|field| {
                use super::setup::Field;
                if *field == Field::Start {
                    return ListItem::new(Line::from(Span::styled(
                        " Start test ",
                        Style::new()
                            .fg(self.theme.inverse)
                            .bg(self.theme.ink)
                            .add_modifier(Modifier::BOLD),
                    )));
                }
                if *field == Field::Advanced {
                    return ListItem::new(Line::from(format!(
                        "{} Advanced",
                        if self.advanced { "⌄" } else { "›" }
                    )));
                }
                let value = field.value(&self.config);
                let heading = Style::new().fg(self.theme.ink).add_modifier(Modifier::BOLD);
                let mut lines = Vec::new();
                if *field == Field::Servers {
                    lines.push(Line::styled("Connections", heading));
                }
                if *field == Field::LatencyStage {
                    lines.push(Line::styled("Stages", heading));
                }
                lines.push(Line::from(vec![
                    Span::raw(format!("{:<22} ", field.label())),
                    Span::styled(
                        if value.is_empty() {
                            "Automatic".into()
                        } else {
                            safe_text(&value, 200)
                        },
                        Style::new().fg(self.theme.text).add_modifier(Modifier::BOLD),
                    ),
                ]));
                ListItem::new(lines)
            })
            .collect::<Vec<_>>();
        frame.render_stateful_widget(
            List::new(items)
                .block(panel("Test setup", self.theme))
                .highlight_style(
                    Style::new()
                        .fg(self.theme.text)
                        .bg(self.theme.surface)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("› "),
            fields_area,
            &mut self.rows,
        );
        if plan_area.width > 0 {
            let selected = if self.config.servers.is_empty() {
                "Catalogue default selection".into()
            } else {
                safe_text(&self.config.servers.join(", "), 300)
            };
            let mut lines = vec![selected, String::new(), "Connection paths".into()];
            if self.awaiting {
                lines.push("Checking selected servers".into());
            } else if self.config != self.requested {
                lines.push("Settings changed · verify again".into());
            } else {
                let checked = self
                    .snapshot
                    .servers
                    .iter()
                    .filter(|server| server.has_check_result())
                    .take(MAX_SELECTED_SERVERS)
                    .collect::<Vec<_>>();
                if checked.is_empty() {
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
                    lines.push(safe_text(&server.name, 120));
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
            let width = usize::from(plan_area.width.saturating_sub(2));
            let text = lines
                .iter()
                .map(|line| safe_text_width(line, width))
                .collect::<Vec<_>>()
                .join("\n");
            frame.render_widget(Paragraph::new(text).block(panel("Servers", self.theme)), plan_area);
        }
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
    fn run_servers(&self) -> Vec<&crate::model::ServerSummary> {
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

    fn draw_live(&self, frame: &mut Frame, area: Rect) {
        if area.height < 14 {
            self.draw_live_compact(frame, area);
            return;
        }
        let (results, failures) = crate::report::results(
            &self.snapshot,
            self.latency_server(),
            usize::from(area.width.saturating_sub(2)),
        );
        let results_height = match results.iter().map(Vec::len).sum::<usize>() + failures.len() {
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
        let show_chart = chart_height >= 9;
        let wide = frame.area().width >= 100;
        let (track_area, timeline_area, results_area) = if wide {
            let regions = Layout::vertical([Constraint::Min(9), Constraint::Length(results_height)]).split(area);
            let columns =
                Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).split(regions[0]);
            (columns[0], columns[1], regions[1])
        } else {
            let regions = Layout::vertical([
                Constraint::Length(track_height),
                Constraint::Length(if show_chart { chart_height } else { 0 }),
                Constraint::Min(1),
            ])
            .split(area);
            (regions[0], regions[1], regions[2])
        };
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
                line.push(Span::styled(
                    format!(
                        "{} {}",
                        crate::vocabulary::MISSING,
                        crate::model::StageStatus::Skipped.label()
                    ),
                    muted,
                ));
            } else {
                line.push(Span::styled(
                    format!("○ {} s", self.requested.duration(*stage).as_secs()),
                    muted,
                ));
            }
            track.push(Line::from(line));
        }
        if track_area.height == 2 {
            let index = self
                .snapshot
                .stage
                .and_then(|stage| self.requested.stages.iter().position(|planned| *planned == stage))
                .unwrap_or(0);
            frame.render_widget(
                Paragraph::new(track.into_iter().skip(index).take(2).collect::<Vec<_>>()),
                track_area,
            );
        } else {
            // Go's wide Test panel puts the run's fields above the stage track; a short panel keeps the track.
            let mut lines = Vec::new();
            if wide {
                lines = self.test_fields(usize::from(track_area.width.saturating_sub(2)));
                lines.push(Line::default());
                if lines.len() + track.len() > usize::from(track_area.height.saturating_sub(2)) {
                    lines.clear();
                }
            }
            lines.extend(track);
            frame.render_widget(Paragraph::new(lines).block(panel("Test", self.theme)), track_area);
        }
        if timeline_area.height >= 9 {
            self.draw_timeline(frame, timeline_area);
        }
        if results_height > 0 {
            let mut lines = Vec::new();
            for grid in &results {
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
            lines.extend(
                failures
                    .iter()
                    .map(|failure| Line::styled(safe_text(failure, MAX_TEXT), Style::new().fg(self.theme.err))),
            );
            let mut title = "Results".to_owned();
            if let Some(id) = self.latency_server().filter(|_| self.run_servers().len() > 1) {
                title.push_str(" · latency to ");
                title.push_str(&safe_text(self.server_name(id), 120));
            }
            frame.render_widget(Paragraph::new(lines).block(panel(&title, self.theme)), results_area);
        }
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
        Style::new().fg(self.theme.text).add_modifier(Modifier::BOLD)
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
        let regions = Layout::vertical([
            Constraint::Length(if self.active() {
                if area.height < 12 { 1 } else { 2 }
            } else {
                0
            }),
            Constraint::Length(if area.height >= 12 { 1 } else { 0 }),
            Constraint::Percentage(50),
            Constraint::Min(1),
        ])
        .split(area);
        if self.active() {
            let focus = self.focused_latency();
            frame.render_widget(
                Paragraph::new(format!(
                    "Download {} · Upload {} · Latency {}\nLatency to {} · l switches server",
                    rate(self.shown_down),
                    rate(self.shown_up),
                    milliseconds(focus.and_then(|host| host.latest_ms)),
                    self.latency_name(focus)
                )),
                regions[0],
            );
        }
        let total = self
            .requested
            .stages
            .iter()
            .map(|stage| self.requested.duration(*stage).as_secs_f64())
            .sum::<f64>()
            .max(1.0);
        let mut marks = vec![Span::raw(" ".repeat(14))];
        let mut marked = 0;
        let plot_width = usize::from(area.width.saturating_sub(16));
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
        frame.render_widget(Paragraph::new(Line::from(marks)), regions[1]);
        let mut starts = Vec::new();
        let mut offset = Duration::ZERO;
        for result in &self.snapshot.results {
            starts.push((offset, result.stage));
            offset += result.elapsed;
        }
        starts.extend(self.snapshot.stage.map(|stage| (offset, stage)));
        let hue = |elapsed: Duration| {
            starts
                .iter()
                .rfind(|(start, _)| *start <= elapsed)
                .map_or(self.theme.text, |(_, stage)| self.theme.stage(*stage))
        };
        for (latency, region) in [(false, regions[2]), (true, regions[3])] {
            let mut series = Vec::<(Vec<(f64, f64)>, ratatui::style::Color)>::new();
            for upload in [false, true] {
                if latency && upload {
                    continue;
                }
                let mut segment = Vec::new();
                let mut color = self.theme.text;
                for point in if latency {
                    self.focused_latency()
                        .map(|host| &host.history.points)
                        .unwrap_or(&self.snapshot.history.points)
                } else {
                    &self.snapshot.history.points
                } {
                    let value = if latency {
                        point.latency_ms
                    } else if upload {
                        point.up_bps
                    } else {
                        point.down_bps
                    };
                    let value = value.filter(|value| value.is_finite() && *value >= 0.0);
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
            }
            let max = series
                .iter()
                .flat_map(|(points, _)| points)
                .map(|(_, value)| *value)
                .fold(0.0_f64, f64::max);
            let scale = if latency {
                1.0
            } else if max >= 1.2e9 {
                1e9
            } else if max >= 1.2e6 {
                1e6
            } else if max >= 1.2e3 {
                1e3
            } else {
                1.0
            };
            let units = if latency {
                "ms"
            } else if scale == 1e9 {
                "Gbit/s"
            } else if scale == 1e6 {
                "Mbit/s"
            } else if scale == 1e3 {
                "kbit/s"
            } else {
                "bit/s"
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
                        Span::styled(
                            format!(
                                "{:>12}",
                                format!("{} {units}", graphite_meter_core::format::speed(ceiling))
                            ),
                            muted,
                        ),
                    ])),
                region,
            );
        }
    }

    fn draw_details(&mut self, frame: &mut Frame) {
        let area = popup(frame.area(), 84, frame.area().height.saturating_sub(2));
        let width = usize::from(area.width.saturating_sub(2));
        let details = crate::report::details(&self.snapshot, self.latency_server(), width);
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
        let visible = usize::from(area.height.saturating_sub(2));
        self.details_scroll = self
            .details_scroll
            .min(lines.len().saturating_sub(visible).min(u16::MAX as usize) as u16);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(lines)
                .scroll((self.details_scroll, 0))
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
                    .highlight_style(
                        Style::new()
                            .fg(self.theme.text)
                            .bg(self.theme.surface)
                            .add_modifier(Modifier::BOLD),
                    ),
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
    match crate::vocabulary::CADENCES
        .iter()
        .find(|(.., preset)| *preset == interval)
    {
        Some((_, label, _)) => (*label).into(),
        None => format!("Custom ({})", setting(interval)),
    }
}

/// Go's fmtSetting.
fn setting(duration: Duration) -> String {
    if duration < Duration::from_secs(1) {
        format!("{} ms", duration.as_millis())
    } else {
        format!("{} s", seconds(duration))
    }
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
    use super::*;
    use crate::model::ServerSummary;
    use graphite_meter_core::discovery::{
        LatencyTarget, LatencyTransport, Protocol, ThroughputTarget, ThroughputTransport,
    };
    use ratatui::{Terminal, backend::TestBackend};

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

    fn rows(ui: &mut Ui, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| ui.draw(frame)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
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
        use crate::model::{ServerLatencyResult, StageResult};
        use graphite_meter_core::{
            latency::{LatencyAccumulator, ProbeOutcome},
            measurement::{Direction, MeasurementResult},
        };
        let mut replies = LatencyAccumulator::default();
        for _ in 0..4 {
            replies.record(ProbeOutcome::Reply {
                rtt_nanos: 2_000_000,
                handling_nanos: 0,
            });
        }
        let loaded = |id: &str| ServerLatencyResult {
            elapsed: Some(Duration::from_secs(1)),
            id: id.into(),
            summary: replies.snapshot(),
            ending: None,
        };
        Snapshot {
            phase: Phase::Complete,
            stage: None,
            results: vec![StageResult {
                stage: Stage::Download,
                elapsed: Duration::from_secs(1),
                down: Some(MeasurementResult {
                    direction: Direction::Down,
                    total_bytes: 1_500_000,
                    mean_bytes_per_sec: Some(1_500_000.0),
                    peak_bytes_per_sec: Some(1_500_000.0),
                    samples: 4,
                    elapsed_nanos: Some(1_000_000_000),
                }),
                server_latencies: vec![loaded("a"), loaded("b")],
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
}
