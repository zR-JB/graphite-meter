//! Each server's share of a run: its mean rates and latency medians, the run's issues and its aggregation intervals.
use super::{ADDED_NOTE, Report, vocabulary::*};
use crate::{
    events::View,
    measure::format,
    model::{Direction, Scope, Stage, Throughput},
    text::Line,
    tui::theme::Palette,
};
use std::time::Duration;

/// Per-server mean rates and latency medians, ✗ once left, and issues; `full` adds result facts and final intervals.
pub fn details(view: &View, width: usize, palette: &Palette, full: bool) -> Vec<Line> {
    let Some(run) = &view.run else { return Vec::new() };
    Report { view, run, focus: run.focus.as_ref(), width, palette }.details(full)
}

impl Report<'_> {
    pub(super) fn details(&self, full: bool) -> Vec<Line> {
        let (palette, run) = (self.palette, self.run);
        let planned = run.plan.iter();
        let columns = planned.flat_map(|&(stage, _)| stage.directions().iter().map(move |&at| (stage, at)));
        let columns: Vec<(Stage, Direction)> = columns.collect();
        let rate = |throughput: Option<Throughput>| {
            let rate = throughput.and_then(|throughput| throughput.rate);
            Line::plain(rate.map_or(MISSING.into(), |rate| format::rate(rate.mean)))
        };
        let combined =
            |&(stage, direction): &_| rate(self.result(stage).and_then(|result| result.throughput[direction]));
        let all = columns.iter().map(combined);
        let mut rates = vec![[vec![Line::plain("All servers")], all.collect()].concat()];
        let mut medians = Vec::new();
        for server in self.view.servers.iter() {
            let own = |stage: Stage| {
                let servers = &self.result(stage)?.servers;
                servers.iter().find(|own| own.server == server.id).cloned()
            };
            let mark = if self.view.remains(&server.id) { "" } else { " ✗" };
            let share = |&(stage, direction): &_| rate(own(stage).and_then(|own| own.throughput[direction]));
            let cells = columns.iter().map(share);
            rates.push([vec![Line::plain(format!("{}{mark}", server.name))], cells.collect()].concat());
            let median = |&(stage, _): &(Stage, Duration)| {
                let median = own(stage).and_then(|own| own.latency?.median());
                Line::plain(median.map_or(MISSING.into(), ms))
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
        let populations = run.plan.iter().map(|&(stage, _)| compact_population(stage));
        let populations: Vec<&str> = std::iter::once("Server").chain(populations).collect();
        let mut lines = vec![Line::styled(self.notice(), palette.heading)];
        let facts = if full { self.facts() } else { Vec::new() };
        if !facts.is_empty() {
            lines.extend(facts);
            lines.push(Line::default());
        }
        lines.extend(self.grid(&headers, rates));
        lines.extend([Line::default(), Line::styled("Latency median by server", palette.heading)]);
        lines.extend(self.grid(&populations, medians));
        if !run.issues.is_empty() {
            lines.extend([Line::default(), Line::styled("Issues", palette.heading)]);
        }
        for (stage, issue) in &run.issues {
            let scope = if issue.scope == Scope::Latency { "latency" } else { "throughput" };
            let at = run
                .at
                .map_or(Duration::ZERO, |origin| issue.at.saturating_duration_since(origin));
            let (name, stage, at) = (self.name(&issue.server), compact_stage(*stage), clock(at));
            lines.push(Line::plain(format!(
                "{name} · {stage} {scope} · at {at} · {}",
                issue.failure.reason.label()
            )));
        }
        if full && run.outcome.is_some() && run.results.iter().any(|result| !result.intervals.is_empty()) {
            lines.extend(self.intervals());
        }
        lines.into_iter().map(|line| line.fit(self.width)).collect()
    }

    /// Each measured direction's and latency population's facts, the server's timing, and what Added means.
    fn facts(&self) -> Vec<Line> {
        let Some((_, _, added)) = self.results("Latency") else { return Vec::new() };
        let mut notes = Vec::new();
        for &(stage, _) in &self.run.plan {
            let result = self.result(stage);
            let measured = result.map_or(Duration::ZERO, |result| result.measured);
            for &direction in stage.directions() {
                let throughput = result.and_then(|result| result.throughput[direction]);
                let Some(throughput) = throughput.filter(|it| it.rate.is_some() || it.bytes > 0) else {
                    continue;
                };
                let facts = throughput_facts(throughput, measured, direction, false);
                notes.extend(self.note(&direction_label(stage, direction), &facts));
            }
            let Some(summary) = self.population(stage).map(|population| population.summary) else {
                continue;
            };
            notes.extend(self.note(&population_label(stage), &latency_facts(summary, measured)));
            if let Some(timing) = summary.timing {
                let label = format!("Server timing ({} paired replies, means)", count(timing.pairs));
                let facts = [format!("raw {}", ms(timing.rtt)), format!("handling {}", ms(timing.handling))];
                notes.extend(self.note(&label, &facts));
            }
        }
        notes.extend(added.then(|| Line::styled(ADDED_NOTE, self.palette.muted)));
        notes
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
                let measured = interval.complete && interval.window.is_some();
                let state = if measured { "measured window" } else { "incomplete evidence" };
                let (start, end) = (offset(interval.start), offset(interval.end));
                let span = format!("{} {start:.1}–{end:.1} s", compact_stage(result.stage));
                let parts = [span, names.join(", "), state.into()].into_iter();
                let parts: Vec<_> = parts.filter(|part| !part.is_empty()).collect();
                lines.push(Line::styled(parts.join(" · "), muted));
            }
            let omitted = format!("{} older intervals omitted; byte totals retain the full run", result.omitted);
            lines.extend((result.omitted > 0).then(|| Line::styled(omitted, muted)));
        }
        lines
    }

    /// The run's outcome with how many of its servers remain.
    fn notice(&self) -> String {
        let selected = self.view.servers.len();
        let servers = self.view.servers.iter();
        let remaining = servers.filter(|server| self.view.remains(&server.id)).count();
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
