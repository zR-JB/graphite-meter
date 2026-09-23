//! Terminal rendering, separate from editing and command dispatch.
use super::*;

impl Ui {
    pub(super) fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < 45 || area.height < 12 {
            frame.render_widget(
                Paragraph::new(
                    "Graphite Meter\nEnlarge terminal to at least 45 × 12.\nq quit · Esc cancel",
                )
                .wrap(Wrap { trim: true }),
                area,
            );
            return;
        }
        let area = area.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        let regions = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(3),
        ])
        .split(area);
        let status = if self.snapshot.status.is_empty() {
            format!("{:?}", self.snapshot.phase)
        } else {
            self.snapshot.status.clone()
        };
        let status = safe_text(&status, usize::from(regions[0].width / 2).saturating_sub(4));
        let title = " Graphite Meter ";
        let status_pill = format!(" {status} ");
        let spacer =
            usize::from(regions[0].width).saturating_sub(title.len() + status_pill.chars().count());
        let status_background = match self.snapshot.phase {
            Phase::Complete => self.theme.success,
            Phase::Cancelled => self.theme.warning,
            Phase::Failed => self.theme.error,
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
                Line::from(vec![
                    Span::styled("native rust client  ", Style::new().fg(self.theme.muted)),
                    Span::styled(
                        safe_text(&self.config.url, 200),
                        Style::new().fg(self.theme.brand_strong),
                    ),
                ]),
            ]),
            regions[0],
        );
        if self.live {
            self.draw_live(frame, regions[1]);
        } else {
            self.draw_setup(frame, regions[1]);
        }
        let (notice, is_error) = self.notice();
        let notice_color = if is_error {
            self.theme.error
        } else {
            self.theme.warning
        };
        let shortcuts = match regions[2].width {
            0..=74 => "r run · Tab section · ? help · q quit",
            75..=109 => {
                "r run · v verify · s servers · Tab sections · ←/→ pages · Esc cancel · ? help · q quit"
            }
            _ => {
                "r run · v verify · s servers · l latency · Tab sections · ←/→ setup pages · Esc cancel · ? help · q quit"
            }
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    safe_text(notice, usize::from(regions[2].width) * 2),
                    Style::new().fg(notice_color),
                ),
                Line::styled(shortcuts, Style::new().fg(self.theme.muted)),
            ])
            .wrap(Wrap { trim: true }),
            regions[2],
        );
        if self.details {
            self.draw_details(frame);
        }
        if let Some(auth) = &self.snapshot.auth {
            let area = popup(frame.area(), 100, 10);
            frame.render_widget(Clear, area);
            let text = format!(
                "Origin: {}\nConfirmation code: {}\n\n{}\n\no opens browser · approve the matching code · Esc cancels",
                safe_text(&auth.origin, 300),
                safe_text(&auth.code, 80),
                safe_text(&auth.browser_url, MAX_TEXT)
            );
            frame.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: false })
                    .block(panel("Client approval required", self.theme)),
                area,
            );
        }
        if self.chooser {
            self.draw_servers(frame);
        }
        if self.help {
            self.draw_help(frame);
        }
        if let Some(edit) = &self.edit {
            let area = popup(frame.area(), 80, 7);
            frame.render_widget(Clear, area);
            let width = usize::from(area.width.saturating_sub(6)).max(1);
            let start = edit.cursor.saturating_sub(width / 2);
            let before: String = edit
                .chars
                .iter()
                .skip(start)
                .take(edit.cursor - start)
                .collect();
            let cursor = edit.chars.get(edit.cursor).copied().unwrap_or(' ');
            let after: String = edit
                .chars
                .iter()
                .skip(edit.cursor + 1)
                .take(width / 2)
                .collect();
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
    }
    fn draw_setup(&mut self, frame: &mut Frame, area: Rect) {
        let setup = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
        frame.render_widget(
            Tabs::new(PAGES.map(|page| page.label))
                .select(self.page)
                .style(Style::new().fg(self.theme.muted))
                .highlight_style(
                    Style::new()
                        .fg(self.theme.inverse)
                        .bg(self.theme.brand)
                        .add_modifier(Modifier::BOLD),
                )
                .divider(" "),
            setup[0],
        );
        let content = setup[1];
        let regions = if content.width >= 95 {
            Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                .split(content)
        } else {
            Layout::horizontal([Constraint::Percentage(100), Constraint::Length(0)]).split(content)
        };
        let items = PAGES[self.page]
            .fields()
            .iter()
            .map(|field| {
                let value = field.value(&self.config);
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{:<25} ", field.label())),
                    Span::styled(
                        if value.is_empty() {
                            "automatic / default".into()
                        } else {
                            safe_text(&value, 200)
                        },
                        Style::new().fg(self.theme.brand_strong),
                    ),
                ]))
            })
            .collect::<Vec<_>>();
        frame.render_stateful_widget(
            List::new(items)
                .block(panel("Enter edit/toggle", self.theme))
                .highlight_style(
                    Style::new()
                        .fg(self.theme.text)
                        .bg(self.theme.surface)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("› "),
            regions[0],
            &mut self.rows,
        );
        if regions[1].width > 0 {
            let stages = self
                .config
                .stages
                .iter()
                .map(|stage| {
                    format!(
                        "{}  {} s",
                        stage.name(),
                        seconds(self.config.duration(*stage))
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let selected = if self.config.servers.is_empty() {
                "Catalogue default selection".into()
            } else {
                safe_text(&self.config.servers.join(", "), 300)
            };
            let text = format!(
                "{selected}\n\n{stages}\n\nWarmup: {} s per transfer stage\n\nLoaded latency: {}\n\nTLS verification: {}\n\nv discovers and checks available servers.\ns chooses up to four servers.\na restores automatic transport paths.",
                seconds(self.config.warmup),
                on_off(self.config.loaded_latency),
                if self.config.insecure {
                    "DISABLED"
                } else {
                    "enabled"
                }
            );
            frame.render_widget(
                Paragraph::new(text)
                    .block(panel("Run plan", self.theme))
                    .wrap(Wrap { trim: true }),
                regions[1],
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

    fn draw_live(&self, frame: &mut Frame, area: Rect) {
        let regions = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(7),
        ])
        .split(area);
        let focus = self.focused_latency();
        let focus_name = focus
            .map(|host| {
                self.snapshot
                    .servers
                    .iter()
                    .find(|server| server.id == host.id)
                    .map_or(host.id.as_str(), |server| server.name.as_str())
            })
            .unwrap_or("unavailable");
        let metrics = format!(
            "↓ {}   ↑ {}   RTT {} [{}]   elapsed {:.1}s",
            rate(self.snapshot.latest.down_bps),
            rate(self.snapshot.latest.up_bps),
            milliseconds(
                focus
                    .filter(|host| host.error.is_none())
                    .and_then(|host| host.latest_ms)
            ),
            safe_text(focus_name, 50),
            self.snapshot.latest.elapsed.as_secs_f64()
        );
        let stage = self.snapshot.stage.map_or("Waiting", Stage::name);
        frame.render_widget(
            Paragraph::new(metrics)
                .block(panel(stage, self.theme))
                .style(Style::new().fg(self.theme.brand_strong)),
            regions[0],
        );
        let points = self
            .snapshot
            .history
            .iter()
            .rev()
            .take(MAX_POINTS)
            .collect::<Vec<_>>();
        let latency = self.snapshot.stage == Some(Stage::Latency);
        let series = |download: bool| {
            points
                .iter()
                .rev()
                .filter_map(|point| {
                    let value = if latency {
                        if download { point.latency_ms } else { None }
                    } else if download {
                        point.down_bps
                    } else {
                        point.up_bps
                    }?;
                    let scale = if latency { 1.0 } else { 1_000_000.0 };
                    (value.is_finite() && value >= 0.0)
                        .then_some((point.elapsed.as_secs_f64(), value / scale))
                })
                .collect::<Vec<_>>()
        };
        let down = if latency {
            focus
                .map(|host| {
                    host.history
                        .iter()
                        .filter_map(|(elapsed, value)| {
                            value
                                .filter(|value| value.is_finite() && *value >= 0.0)
                                .map(|value| (elapsed.as_secs_f64(), value))
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            series(true)
        };
        let up = series(false);
        let maximum = down
            .iter()
            .chain(&up)
            .map(|point| point.1)
            .fold(1.0_f64, f64::max)
            * 1.1;
        let start = points
            .last()
            .map_or(0.0, |point| point.elapsed.as_secs_f64());
        let end = points
            .first()
            .map_or(start + 1.0, |point| point.elapsed.as_secs_f64())
            .max(start + 1.0);
        let datasets = vec![
            Dataset::default()
                .name(if latency { "RTT ms" } else { "↓ Mbps" })
                .marker(Marker::Braille)
                .graph_type(GraphType::Scatter)
                .style(Style::new().fg(self.theme.brand))
                .data(&down),
            Dataset::default()
                .name(if latency { "" } else { "↑ Mbps" })
                .marker(Marker::Braille)
                .graph_type(GraphType::Scatter)
                .style(Style::new().fg(self.theme.brand_strong))
                .data(&up),
        ];
        // Scatter plots leave missing observations empty; no loss is interpolated.
        frame.render_widget(
            Chart::new(datasets)
                .block(panel(
                    if latency {
                        "Recent observed latency"
                    } else {
                        "Recent observed throughput"
                    },
                    self.theme,
                ))
                .x_axis(
                    Axis::default()
                        .bounds([start, end])
                        .labels([format!("{start:.0}s"), format!("{end:.0}s")]),
                )
                .y_axis(
                    Axis::default()
                        .bounds([0.0, maximum])
                        .labels(["0".to_owned(), format!("{maximum:.0}")]),
                ),
            regions[1],
        );
        let rows = self.snapshot.results.iter().take(16).map(|result| {
            let summary = focus
                .and_then(|focus| {
                    result
                        .server_latencies
                        .iter()
                        .find(|host| host.id == focus.id)
                })
                .map(|host| &host.summary);
            Row::new(vec![
                format!(
                    "{}{}",
                    result.stage.name(),
                    if result.complete { "" } else { " (partial)" }
                ),
                rate(result.down_bps),
                rate(result.up_bps),
                milliseconds(
                    summary
                        .and_then(|summary| summary.distribution)
                        .map(|d| d.p50 as f64 / 1_000_000.0),
                ),
                summary.map_or_else(|| "—".into(), |summary| summary.timeouts.to_string()),
                summary.map_or_else(|| "—".into(), |summary| summary.unresolved.to_string()),
            ])
        });
        frame.render_widget(
            Table::new(
                rows,
                [
                    Constraint::Percentage(24),
                    Constraint::Percentage(19),
                    Constraint::Percentage(19),
                    Constraint::Percentage(16),
                    Constraint::Percentage(11),
                    Constraint::Percentage(11),
                ],
            )
            .header(
                Row::new(["Stage", "Down", "Up", "RTT p50", "Timeout", "Pending"])
                    .style(Style::new().fg(self.theme.brand_strong)),
            )
            .block(panel(
                "Stage results · d per-server · receiver upload",
                self.theme,
            )),
            regions[2],
        );
    }
    fn draw_details(&mut self, frame: &mut Frame) {
        let area = popup(frame.area(), 108, frame.area().height.saturating_sub(2));
        let mut lines = Vec::new();
        for result in &self.snapshot.results {
            lines.push(Line::styled(
                format!(
                    "{}{}  ↓ {}  ↑ {}",
                    result.stage.name(),
                    if result.complete { "" } else { " (partial)" },
                    rate(result.down_bps),
                    rate(result.up_bps),
                ),
                Style::new()
                    .fg(self.theme.brand_strong)
                    .add_modifier(Modifier::BOLD),
            ));
            for server in &result.server_results {
                let name = self
                    .snapshot
                    .servers
                    .iter()
                    .find(|summary| summary.id == server.id)
                    .map_or(server.id.as_str(), |summary| summary.name.as_str());
                lines.push(Line::from(format!(
                    "  {}  ↓ {}  ↑ {}",
                    safe_text(name, 32),
                    rate(server.down_bps),
                    rate(server.up_bps),
                )));
                lines.push(Line::styled(
                    format!(
                        "    received ↓ {} B  ↑ {} B",
                        server.down_bytes, server.up_bytes
                    ),
                    Style::new().fg(self.theme.muted),
                ));
                if let Some(host) = result
                    .server_latencies
                    .iter()
                    .find(|host| host.id == server.id)
                {
                    let p50 = host
                        .summary
                        .distribution
                        .map(|distribution| distribution.p50 as f64 / 1_000_000.0);
                    lines.push(Line::styled(
                        format!(
                            "    RTT {}  replies {}  timeouts {}  pending {}",
                            milliseconds(p50),
                            host.summary.count,
                            host.summary.timeouts,
                            host.summary.unresolved,
                        ),
                        Style::new().fg(self.theme.muted),
                    ));
                    if let Some(error) = &host.error {
                        lines.push(Line::styled(
                            format!("    Latency: {}", safe_text(error, 110)),
                            Style::new().fg(self.theme.error),
                        ));
                    }
                }
                if let Some(error) = &server.error {
                    lines.push(Line::styled(
                        format!("    {}", safe_text(error, 120)),
                        Style::new().fg(self.theme.error),
                    ));
                }
            }
            if result.stage == Stage::Latency {
                for host in &result.server_latencies {
                    let p50 = host
                        .summary
                        .distribution
                        .map(|distribution| distribution.p50 as f64 / 1_000_000.0);
                    lines.push(Line::from(format!(
                        "  {}  RTT {}  replies {}  timeouts {}  pending {}",
                        safe_text(&host.id, 32),
                        milliseconds(p50),
                        host.summary.count,
                        host.summary.timeouts,
                        host.summary.unresolved,
                    )));
                    if let Some(error) = &host.error {
                        lines.push(Line::styled(
                            format!("    {}", safe_text(error, 120)),
                            Style::new().fg(self.theme.error),
                        ));
                    }
                }
            }
            lines.push(Line::from(""));
        }
        let visible = usize::from(area.height.saturating_sub(2));
        let maximum_scroll = lines.len().saturating_sub(visible).min(u16::MAX as usize) as u16;
        self.details_scroll = self.details_scroll.min(maximum_scroll);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(lines)
                .scroll((self.details_scroll, 0))
                .block(panel(
                    "Per-server results · ↑/↓ scroll · d/Esc close",
                    self.theme,
                )),
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
                let detail = server.error.as_deref().unwrap_or(&server.transport);
                ListItem::new(vec![
                    Line::from(format!(
                        "{} {} · {}",
                        if selected { "[✓]" } else { "[ ]" },
                        safe_text(&server.name, 120),
                        safe_text(&server.id, 120)
                    )),
                    Line::styled(
                        format!(
                            "    {}  {}",
                            safe_text(&server.origin, 160),
                            safe_text(detail, 160)
                        ),
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
    fn draw_help(&self, frame: &mut Frame) {
        let area = popup(frame.area(), 78, 18);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(concat!(
                "SETUP\n↑/↓ or j/k  select setting     ←/→  change setup page\n",
                "Enter/Space  edit or toggle   s  server chooser\n",
                "v  verify configuration    a  automatic transport paths\n",
                "r  start measurement\n\n",
                "MEASUREMENT\nEsc  cancel active work        r  rerun after completion\n",
                "l  next latency server        d  per-server results\n",
                "Tab/Shift-Tab  section\n\n",
                "EDITING\n←/→ Home/End  move cursor      Enter  apply     Esc  discard\n",
                "Paste is bounded and terminal controls are removed.\n\n",
                "q or Ctrl-C  quit              ? or Esc  close help\n",
                "Missing samples remain missing; partial results stay labelled.",
            ))
            .block(panel("Keyboard help", self.theme))
            .wrap(Wrap { trim: true }),
            area,
        );
    }
}
