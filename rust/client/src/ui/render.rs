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
        let help_lines = if self.help {
            [
                "Tab/Shift-Tab focus · arrows change",
                "Enter edit/run · Space stage",
                "Esc close/stop · r run · v recheck",
                "s servers · u available · a auto",
                "d Details · l latency · o sign-in",
                "q quit · Ctrl-C stop · ? keys",
            ]
            .into_iter()
            .map(Line::from)
            .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let regions = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(if self.help { 1 } else { 4 }),
            Constraint::Length(if self.help {
                1 + help_lines.len() as u16
            } else {
                2
            }),
        ])
        .split(area);
        let status = match self.snapshot.phase {
            Phase::Setup => "Not started",
            Phase::Preparing => "Checking paths",
            Phase::Warmup => "Warmup",
            Phase::Measuring => "Running",
            Phase::Complete => "Complete",
            Phase::Partial => "Partial",
            Phase::Incomplete => "Incomplete",
            Phase::Cancelled => "Stopped",
            Phase::Failed => "Failed",
        }
        .to_owned();
        let status = safe_text_width(&status, usize::from(regions[0].width / 2).saturating_sub(4));
        let title = " Graphite Meter ";
        let status_pill = format!(" {status} ");
        let spacer =
            usize::from(regions[0].width).saturating_sub(title.width() + status_pill.width());
        let status_background = match self.snapshot.phase {
            Phase::Complete => self.theme.success,
            Phase::Cancelled | Phase::Partial => self.theme.warning,
            Phase::Failed | Phase::Incomplete => self.theme.error,
            _ => self.theme.brand_strong,
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(
                        title,
                        Style::new()
                            .fg(self.theme.inverse)
                            .bg(self.theme.brand)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" ".repeat(spacer)),
                    Span::styled(
                        status_pill,
                        Style::new()
                            .fg(self.theme.inverse)
                            .bg(status_background)
                            .add_modifier(Modifier::BOLD),
                    ),
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
                    Style::new().fg(self.theme.muted),
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
        if !self.live && !is_error && self.notice.is_empty() && self.snapshot.phase == Phase::Setup
        {
            notice = self.fields()[self.rows.selected().unwrap_or(0)].explanation(&self.config);
        }
        let notice_color = if is_error {
            self.theme.error
        } else {
            self.theme.muted
        };
        let hints: Vec<&str> = if self.cancel == CancelState::Confirming {
            vec!["Esc confirm stop", "any key continue", "q quit"]
        } else if self.live {
            if self.active() {
                vec![
                    "Esc stop",
                    "d Details",
                    "l Latency server",
                    "? keys",
                    "q quit",
                ]
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
            let action = if field.stage().is_some()
                || matches!(field, Field::LoadedLatency | Field::Insecure)
            {
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

        let mut footer = String::new();
        for hint in hints {
            let next = if footer.is_empty() {
                hint.to_owned()
            } else {
                format!("{footer} · {hint}")
            };
            if next.width() > usize::from(regions[2].width) {
                break;
            }
            footer = next;
        }
        let mut lines = if self.help {
            Vec::new()
        } else {
            vec![Line::styled(
                safe_text_width(notice, usize::from(regions[2].width)),
                Style::new().fg(notice_color),
            )]
        };
        lines.push(Line::styled(
            footer,
            Style::new().fg(self.theme.brand_strong),
        ));
        lines.extend(help_lines);
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
                        Span::styled(
                            cursor.to_string(),
                            Style::new().add_modifier(Modifier::REVERSED),
                        ),
                        Span::raw(after),
                    ]),
                    Line::from("Enter apply · Esc discard · ←/→ Home/End move"),
                    Line::styled(
                        safe_text(&self.notice, 200),
                        Style::new().fg(self.theme.warning),
                    ),
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
            Paragraph::new(safe_text_width(
                &format!("Match this code: {}", auth.code),
                width,
            ))
            .style(
                Style::new()
                    .fg(self.theme.brand_strong)
                    .add_modifier(Modifier::BOLD),
            ),
            regions[1],
        );
        frame.render_widget(
            Paragraph::new("Browser URL · ↑/↓ scroll").style(Style::new().fg(self.theme.muted)),
            regions[2],
        );
        let url = safe_text(&auth.browser_url, MAX_TEXT);
        let lines = wrap_columns(&url, width.max(1));
        let remaining = auth
            .deadline
            .saturating_duration_since(tokio::time::Instant::now());
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
            Paragraph::new("Enter/Space/o open · Esc cancel · q quit")
                .style(Style::new().fg(self.theme.brand_strong)),
            regions[5],
        );
    }

    fn draw_setup(&mut self, frame: &mut Frame, area: Rect) {
        let content = area;
        let (fields_area, plan_area) = if frame.area().width >= 100 {
            let regions =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(content);
            (regions[0], regions[1])
        } else if content.height >= 12 {
            let regions =
                Layout::vertical([Constraint::Length(5), Constraint::Min(1)]).split(content);
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
                            .bg(self.theme.brand)
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
                let mut lines = Vec::new();
                if *field == Field::Servers {
                    lines.push(Line::styled(
                        "Connections",
                        Style::new()
                            .fg(self.theme.brand_strong)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                if *field == Field::LatencyStage {
                    lines.push(Line::styled(
                        "Stages",
                        Style::new()
                            .fg(self.theme.brand_strong)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                lines.push(Line::from(vec![
                    Span::raw(format!("{:<22} ", field.label())),
                    Span::styled(
                        if value.is_empty() {
                            "Automatic".into()
                        } else {
                            safe_text(&value, 200)
                        },
                        Style::new().fg(self.theme.brand_strong),
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
                    .take(4)
                    .collect::<Vec<_>>();
                if checked.is_empty() {
                    lines.push(
                        if self.snapshot.phase == Phase::Preparing {
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
            lines.extend(self.config.stages.iter().map(|stage| {
                format!(
                    "  {}  {} s",
                    stage.name(),
                    seconds(self.config.duration(*stage))
                )
            }));
            lines.extend([
                String::new(),
                format!("Warmup: {} s", seconds(self.config.warmup)),
                format!("Loaded latency: {}", on_off(self.config.loaded_latency)),
                format!(
                    "TLS verification: {}",
                    if self.config.insecure {
                        "DISABLED"
                    } else {
                        "enabled"
                    }
                ),
            ]);
            let width = usize::from(plan_area.width.saturating_sub(2));
            let text = lines
                .iter()
                .map(|line| safe_text_width(line, width))
                .collect::<Vec<_>>()
                .join("\n");
            frame.render_widget(
                Paragraph::new(text).block(panel("Servers", self.theme)),
                plan_area,
            );
        }
    }
    fn focused_latency(&self) -> Option<&crate::model::ServerLatency> {
        self.snapshot
            .server_latencies
            .iter()
            .find(|host| Some(&host.id) == self.latency_focus.as_ref())
            .or_else(|| self.snapshot.server_latencies.first())
    }
    fn latency_name<'a>(&'a self, focus: Option<&'a crate::model::ServerLatency>) -> &'a str {
        focus
            .map(|host| {
                self.snapshot
                    .servers
                    .iter()
                    .find(|server| server.id == host.id)
                    .map_or(host.id.as_str(), |server| server.name.as_str())
            })
            .unwrap_or("unavailable")
    }

    fn draw_live(&self, frame: &mut Frame, area: Rect) {
        if area.height < 14 {
            self.draw_live_compact(frame, area);
            return;
        }
        let results_height = (4
            + self
                .snapshot
                .results
                .iter()
                .filter(|result| result.stage != Stage::Latency)
                .count()
            + self
                .snapshot
                .results
                .iter()
                .filter(|result| self.result_latency(result).is_some())
                .count()
            + self.snapshot.failures.len())
        .min(u16::MAX as usize) as u16;
        let track_height = if area.height < 24 {
            2
        } else if self.active() {
            6
        } else {
            4
        };
        let chart_height = area.height.saturating_sub(results_height + track_height);
        let show_chart = chart_height >= 9;
        let (track_area, timeline_area, results_area) = if frame.area().width >= 100 {
            let regions =
                Layout::vertical([Constraint::Min(9), Constraint::Length(results_height)])
                    .split(area);
            let columns =
                Layout::horizontal([Constraint::Percentage(32), Constraint::Percentage(68)])
                    .split(regions[0]);
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
        let mut track = Vec::new();
        for stage in &self.requested.stages {
            let result = self
                .snapshot
                .results
                .iter()
                .find(|result| result.stage == *stage);
            let value = if let Some(result) = result {
                self.outcome(result)
            } else if self.active() && self.snapshot.stage == Some(*stage) {
                match self.snapshot.phase {
                    Phase::Preparing => "checking paths".into(),
                    Phase::Warmup => "warmup".into(),
                    _ => {
                        let elapsed = self.elapsed().as_secs_f64();
                        let duration = self.requested.duration(*stage).as_secs_f64();
                        let filled = (elapsed / duration * 8.0).clamp(0.0, 8.0) as usize;
                        format!(
                            "{}{} {elapsed:.1} s / {duration:.0} s",
                            "█".repeat(filled),
                            "░".repeat(8 - filled)
                        )
                    }
                }
            } else if self.snapshot.phase == Phase::Cancelled {
                "✗ Stopped".into()
            } else {
                format!("○ {} s", self.requested.duration(*stage).as_secs())
            };
            track.push(Line::from(safe_text_width(
                &format!("{:<14} {value}", stage.name()),
                usize::from(area.width.saturating_sub(2)),
            )));
        }
        if track_area.height == 2 {
            let index = self
                .snapshot
                .stage
                .and_then(|stage| {
                    self.requested
                        .stages
                        .iter()
                        .position(|planned| *planned == stage)
                })
                .unwrap_or(0);
            frame.render_widget(
                Paragraph::new(track.into_iter().skip(index).take(2).collect::<Vec<_>>()),
                track_area,
            );
        } else {
            frame.render_widget(
                Paragraph::new(track).block(panel("Test", self.theme)),
                track_area,
            );
        }
        if timeline_area.height >= 9 {
            self.draw_timeline(frame, timeline_area);
        }
        self.draw_results(frame, results_area);
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
        for result in self.snapshot.results.iter().rev().take(4) {
            lines.push(format!("{}: {}", result.stage.name(), self.outcome(result)));
        }
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .map(|line| Line::from(safe_text_width(&line, usize::from(area.width))))
                    .collect::<Vec<_>>(),
            ),
            area,
        );
    }

    fn outcome(&self, result: &crate::model::StageResult) -> String {
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
        match result.status() {
            crate::model::StageStatus::Complete => format!("✓ {headline}"),
            status @ crate::model::StageStatus::Partial => {
                format!("! {headline} {}", status.label())
            }
            status => format!("✗ {}", status.label()),
        }
    }

    fn result_latency<'a>(
        &'a self,
        result: &'a crate::model::StageResult,
    ) -> Option<&'a crate::model::ServerLatencyResult> {
        match &self.latency_focus {
            Some(id) => result.server_latencies.iter().find(|host| &host.id == id),
            None => result.server_latencies.first(),
        }
    }

    fn draw_results(&self, frame: &mut Frame, area: Rect) {
        let mut lines = vec![Line::styled(
            "Throughput              Download           Upload",
            Style::new()
                .fg(self.theme.brand_strong)
                .add_modifier(Modifier::BOLD),
        )];
        for result in &self.snapshot.results {
            if result.stage == Stage::Latency {
                continue;
            }
            lines.push(Line::from(format!(
                "{:<23} {:<18} {}",
                result.stage.name(),
                if result.stage.downloads() {
                    rate(result.down_bps())
                } else {
                    "—".into()
                },
                if result.stage.uploads() {
                    rate(result.up_bps())
                } else {
                    "—".into()
                }
            )));
        }
        if area.width >= 90 {
            lines.push(Line::styled(
                format!(
                    "{:<15}{:<13}{:<13}{:<13}{:<13}{}",
                    crate::vocabulary::LATENCY.label,
                    crate::vocabulary::MEDIAN.label,
                    crate::vocabulary::ADDED.label,
                    crate::vocabulary::P95.label,
                    crate::vocabulary::JITTER.label,
                    crate::vocabulary::PROBE_TIMEOUTS.label
                ),
                Style::new()
                    .fg(self.theme.brand_strong)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            lines.push(Line::styled(
                [
                    crate::vocabulary::LATENCY,
                    crate::vocabulary::MEDIAN,
                    crate::vocabulary::ADDED,
                    crate::vocabulary::P95,
                    crate::vocabulary::JITTER,
                    crate::vocabulary::PROBE_TIMEOUTS,
                ]
                .map(|term| term.label)
                .join(" · "),
                Style::new()
                    .fg(self.theme.brand_strong)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        for result in &self.snapshot.results {
            let Some(host) = self.result_latency(result) else {
                continue;
            };
            let median = host.median();
            let cells = [
                milliseconds(median.map(|median| median as f64 / 1e6)),
                self.snapshot.added_ms(result, &host.id).map_or_else(
                    || "—".into(),
                    |value| format!("{} ms", graphite_meter_core::format::added_ms(value)),
                ),
                milliseconds(
                    median
                        .and(host.summary.distribution)
                        .map(|distribution| distribution.p95 as f64 / 1e6),
                ),
                milliseconds(host.summary.jitter.map(|jitter| jitter as f64 / 1e6)),
                crate::vocabulary::probe_timeouts(host.summary),
            ];
            let name = match result.stage {
                Stage::Latency => "Idle",
                Stage::Download => "Loaded down",
                Stage::Upload => "Loaded up",
                Stage::Bidirectional => "Loaded bi-dir",
            };
            if area.width >= 90 {
                lines.push(Line::from(format!(
                    "{name:<14} {:<12} {:<12} {:<12} {:<12} {}",
                    cells[0], cells[1], cells[2], cells[3], cells[4]
                )));
            } else {
                lines.push(Line::from(format!("{name}: {}", cells.join(" · "))));
            }
        }
        for failure in &self.snapshot.failures {
            lines.push(Line::styled(
                failure.reason.label(),
                Style::new().fg(self.theme.error),
            ));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(panel("Results", self.theme))
                .wrap(Wrap { trim: true }),
            area,
        );
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
        let mut marks = String::new();
        let plot_width = usize::from(area.width.saturating_sub(16));
        let mut at = 0.0;
        for stage in &self.requested.stages {
            let column = (at / total * plot_width as f64) as usize;
            if column >= marks.width() {
                marks.push_str(&" ".repeat(column - marks.width()));
                marks.push_str(if *stage == Stage::Latency {
                    "Idle"
                } else {
                    stage.name()
                });
            }
            at += self.requested.duration(*stage).as_secs_f64();
        }
        frame.render_widget(
            Paragraph::new(safe_text_width(
                &format!("{:>14}{marks}", ""),
                usize::from(area.width),
            ))
            .style(Style::new().fg(self.theme.muted)),
            regions[1],
        );
        for (latency, region) in [(false, regions[2]), (true, regions[3])] {
            let mut series = Vec::<(Vec<(f64, f64)>, ratatui::style::Color)>::new();
            for (upload, color) in [(false, self.theme.brand), (true, self.theme.brand_strong)] {
                if latency && upload {
                    continue;
                }
                let mut segment = Vec::new();
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
                    if let Some(value) = value.filter(|value| value.is_finite() && *value >= 0.0) {
                        segment.push((point.elapsed.as_secs_f64(), value));
                    } else if !segment.is_empty() {
                        series.push((std::mem::take(&mut segment), color));
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
            frame.render_widget(
                Chart::new(datasets)
                    .block(panel(
                        if latency {
                            "Latency · ms"
                        } else {
                            "Throughput"
                        },
                        self.theme,
                    ))
                    .x_axis(
                        Axis::default()
                            .bounds([0.0, total])
                            .labels(["0 s".to_owned(), format!("{total:.0} s")]),
                    )
                    .y_axis(Axis::default().bounds([0.0, ceiling]).labels([
                        format!("{:>12}", "0"),
                        format!(
                            "{:>12}",
                            format!("{} {units}", graphite_meter_core::format::speed(ceiling))
                        ),
                    ])),
                region,
            );
        }
    }

    fn draw_details(&mut self, frame: &mut Frame) {
        let area = popup(frame.area(), 84, frame.area().height.saturating_sub(2));
        let mut lines = vec![Line::styled(
            self.snapshot.status.clone(),
            Style::new()
                .fg(self.theme.brand_strong)
                .add_modifier(Modifier::BOLD),
        )];
        for result in &self.snapshot.results {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                result.stage.name(),
                Style::new()
                    .fg(self.theme.brand_strong)
                    .add_modifier(Modifier::BOLD),
            ));
            for measurement in [&result.down, &result.up].into_iter().flatten() {
                let name =
                    if measurement.direction == graphite_meter_core::measurement::Direction::Down {
                        "Download"
                    } else {
                        "Upload"
                    };
                lines.push(Line::from(format!(
                    "{name}: {}",
                    crate::vocabulary::throughput_facts(measurement)
                )));
            }
            if !result.server_results.is_empty() {
                lines.push(Line::from(format!(
                    "{:<23} {:<20} Upload",
                    "Throughput", "Download"
                )));
                lines.push(Line::from(format!(
                    "{:<23} {:<20} {}",
                    "Combined",
                    rate(result.down_bps()),
                    rate(result.up_bps())
                )));
                for server in &result.server_results {
                    let name = self.server_name(&server.id);
                    lines.push(Line::from(format!(
                        "{:<23} {:<20} {}",
                        safe_text(name, 22),
                        rate(server.down_bps()),
                        rate(server.up_bps())
                    )));
                }
            }
            lines.push(Line::styled(
                "Latency median by server",
                Style::new().fg(self.theme.brand_strong),
            ));
            for host in &result.server_latencies {
                lines.push(Line::from(format!(
                    "{} · Median {} · {} replies",
                    self.server_name(&host.id),
                    milliseconds(host.median().map(|value| value as f64 / 1e6)),
                    host.summary.count
                )));
                lines.push(Line::from(crate::vocabulary::latency_facts(
                    host.summary,
                    host.elapsed,
                )));
                if let Some(timing) = crate::vocabulary::reflector_facts(host.summary) {
                    lines.push(Line::from(timing));
                }
            }
        }
        if !self.snapshot.failures.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "Left the test",
                Style::new()
                    .fg(self.theme.brand_strong)
                    .add_modifier(Modifier::BOLD),
            ));
            for failure in &self.snapshot.failures {
                lines.extend(wrap_columns(
                    &safe_text(
                        &crate::vocabulary::failure_facts(
                            failure,
                            self.server_name(&failure.server_id),
                        ),
                        MAX_TEXT,
                    ),
                    usize::from(area.width.saturating_sub(2)),
                ));
            }
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Aggregation intervals",
            Style::new()
                .fg(self.theme.brand_strong)
                .add_modifier(Modifier::BOLD),
        ));
        for result in &self.snapshot.results {
            if result.omitted_intervals > 0 {
                lines.push(Line::from(format!(
                    "{} earlier intervals omitted",
                    result.omitted_intervals
                )));
            }
            for interval in &result.intervals {
                let reason = match interval.reason {
                    graphite_meter_core::measurement::IntervalReason::StageStart => "stage-start",
                    graphite_meter_core::measurement::IntervalReason::Dropout => "dropout",
                    graphite_meter_core::measurement::IntervalReason::EvidenceResumed => {
                        "evidence-resumed"
                    }
                };
                lines.push(Line::from(format!(
                    "{} · {reason} · {:.1}–{:.1} s{}",
                    result.stage.name(),
                    interval.start_nanos as f64 / 1e9,
                    interval.end_nanos as f64 / 1e9,
                    if interval.complete {
                        ""
                    } else {
                        " · incomplete"
                    }
                )));
                lines.push(Line::from(
                    interval
                        .participants
                        .iter()
                        .map(|id| self.server_name(id))
                        .collect::<Vec<_>>()
                        .join(" · "),
                ));
            }
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Values",
            Style::new().fg(self.theme.brand_strong),
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

    fn server_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.snapshot
            .servers
            .iter()
            .find(|server| server.id == id)
            .map_or(id, |server| server.name.as_str())
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
                            self.theme.error
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
                    .block(panel(
                        "Servers · Space toggle · Enter apply · maximum four",
                        self.theme,
                    ))
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
