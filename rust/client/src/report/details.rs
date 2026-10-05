//! Each server's share of a run: mean rates and latency medians per server, the run's issues and its aggregation
//! intervals.
use super::{Report, vocabulary::*};
use crate::{
    events::View,
    measure::format,
    model::{Direction, Scope, Stage, Throughput},
    text::Line,
    tui::theme::Palette,
};
use graphite_meter_proto::catalog::ServerId;
use std::time::Duration;

/// Each server's mean rates and latency medians, ✗ once it left, and the run's issues; with `full` its aggregation
/// intervals once it finished.
pub fn details(view: &View, width: usize, palette: &Palette, full: bool) -> Vec<Line> {
    let Some(run) = &view.run else { return Vec::new() };
    Report { view, run, width, palette }.details(full)
}

impl Report<'_> {
    pub(super) fn details(&self, full: bool) -> Vec<Line> {
        let (palette, run) = (self.palette, self.run);
        let columns = run
            .plan
            .iter()
            .flat_map(|&(stage, _)| stage.directions().iter().map(move |&at| (stage, at)));
        let columns: Vec<(Stage, Direction)> = columns.collect();
        let rate = |throughput: Option<Throughput>| {
            Line::plain(
                throughput
                    .and_then(|throughput| throughput.rate)
                    .map_or(MISSING.into(), |rate| format::rate(rate.mean)),
            )
        };
        let all = columns
            .iter()
            .map(|&(stage, direction)| rate(self.result(stage).and_then(|result| result.throughput[direction])));
        let mut rates = vec![[vec![Line::plain("All servers")], all.collect()].concat()];
        let mut medians = Vec::new();
        for server in self.view.servers.iter() {
            let own = |stage: Stage| {
                self.result(stage)?
                    .servers
                    .iter()
                    .find(|own| own.server == server.id)
                    .cloned()
            };
            let mark = if self.remains(&server.id) { "" } else { " ✗" };
            let cells = columns
                .iter()
                .map(|&(stage, direction)| rate(own(stage).and_then(|own| own.throughput[direction])));
            rates.push([vec![Line::plain(format!("{}{mark}", server.name))], cells.collect()].concat());
            let median = |&(stage, _): &(Stage, Duration)| {
                Line::plain(
                    own(stage)
                        .and_then(|own| own.latency?.median())
                        .map_or(MISSING.into(), ms),
                )
            };
            medians.push([vec![Line::plain(&server.name)], run.plan.iter().map(median).collect()].concat());
        }
        let mut headers = vec!["Server".to_owned()];
        headers.extend(
            columns
                .iter()
                .map(|&(stage, direction)| direction_label(stage, direction)),
        );
        let headers: Vec<&str> = headers.iter().map(String::as_str).collect();
        let populations: Vec<&str> = ["Server"]
            .into_iter()
            .chain(run.plan.iter().map(|&(stage, _)| compact_population(stage)))
            .collect();
        let mut lines = vec![Line::styled(self.notice(), palette.heading)];
        lines.extend(self.grid(&headers, rates));
        lines.extend([Line::default(), Line::styled("Latency median by server", palette.heading)]);
        lines.extend(self.grid(&populations, medians));
        if !run.issues.is_empty() {
            lines.extend([Line::default(), Line::styled("Issues", palette.heading)]);
        }
        for issue in &run.issues {
            let scope = if issue.scope == Scope::Latency { "latency" } else { "throughput" };
            let (name, stage, at) = (self.name(&issue.server), compact_stage(issue.stage), clock(issue.at));
            lines.push(Line::plain(format!(
                "{name} · {stage} {scope} · at {at} · {}",
                issue.failure.reason.label()
            )));
        }
        if full && run.outcome.is_some() {
            lines.extend(self.intervals());
        }
        lines.into_iter().map(|line| line.fit(self.width)).collect()
    }

    /// The aggregation intervals of every stage, and how many older ones each dropped.
    fn intervals(&self) -> Vec<Line> {
        let (Some(origin), muted) = (self.run.at, self.palette.muted) else {
            return Vec::new();
        };
        let mut lines = vec![Line::default(), Line::styled("Aggregation intervals", self.palette.heading)];
        let offset = |at: std::time::Instant| at.saturating_duration_since(origin).as_secs_f64();
        for result in &self.run.results {
            for interval in &result.intervals {
                let names: Vec<_> = interval.participants.iter().map(|id| self.name(id)).collect();
                let state = if interval.complete && interval.window.is_some() {
                    "measured window"
                } else {
                    "incomplete evidence"
                };
                let span = format!(
                    "{} {:.1}–{:.1} s",
                    compact_stage(result.stage),
                    offset(interval.start),
                    offset(interval.end)
                );
                let parts: Vec<_> = [span, names.join(", "), state.into()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect();
                lines.push(Line::styled(parts.join(" · "), muted));
            }
            let omitted = format!("{} older intervals omitted; byte totals retain the full run", result.omitted);
            lines.extend((result.omitted > 0).then(|| Line::styled(omitted, muted)));
        }
        lines
    }

    /// Whether `server` is still in the run: in its latest stage without leaving, or prepared before any.
    fn remains(&self, server: &ServerId) -> bool {
        let latest = self
            .run
            .results
            .iter()
            .rev()
            .find_map(|result| result.servers.iter().find(|own| own.server == *server));
        match latest {
            Some(own) => !own.left,
            None => self
                .view
                .servers
                .iter()
                .any(|prepared| prepared.id == *server && prepared.path.is_ok()),
        }
    }

    /// The run's outcome with how many of its servers remain.
    fn notice(&self) -> String {
        let selected = self.view.servers.len();
        let remaining = self
            .view
            .servers
            .iter()
            .filter(|server| self.remains(&server.id))
            .count();
        let outcome = self.run.outcome.map_or("Running", outcome_label);
        match () {
            _ if self.run.outcome.is_none() && remaining < selected => {
                format!("{remaining} of {selected} servers remaining")
            }
            _ if self.run.outcome.is_none() => format!("All {selected} servers"),
            _ if remaining < selected => format!("{outcome} · {remaining} of {selected} servers"),
            _ => format!("{outcome} · all {selected} servers"),
        }
    }
}
