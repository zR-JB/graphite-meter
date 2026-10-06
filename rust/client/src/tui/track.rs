//! The Test panel: the run's servers, paths and settings over its stage track.
use super::{App, frame::unique};
use crate::{
    events::Run,
    model::{Stage, StageResult, StageStatus},
    report::vocabulary as words,
    run::prepare::Paths,
    text::{Line, Style, wrap},
};

const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

impl App {
    /// The stage track, under the run's settings when `fields`.
    pub(super) fn test_view(&self, run: &Run, width: usize, fields: bool) -> Vec<Line> {
        match fields {
            true => [self.test_fields(run, width), vec![Line::default()], self.track(run, width)].concat(),
            false => self.track(run, width),
        }
    }

    /// The run's servers, paths, streams and timing.
    pub(super) fn test_fields(&self, run: &Run, width: usize) -> Vec<Line> {
        let (palette, config) = (&self.palette, &self.config);
        let label = |name: &str| Line::styled(format!("{name:<11}"), palette.text);
        if run.at.is_none() {
            let value = match run.outcome {
                None => self.checking_line(),
                Some(_) => Line::styled(words::MISSING, palette.muted),
            };
            return vec![label("Servers").with(value)];
        }
        let servers = self.view.servers.iter();
        let paths: Vec<_> = servers
            .filter_map(|server| Some((&server.id, server.path.as_ref().ok()?)))
            .collect();
        let throughput = unique(paths.iter().map(|(_, paths)| words::throughput_path(&paths.throughput)));
        let shown = paths.iter().find(|(id, _)| Some(*id) == self.latency_server());
        let latency = shown.and_then(|(_, paths)| paths.latency.as_ref());
        let streams = |(_, paths): &(_, &Paths)| words::streams(config, &paths.throughput);
        let streams = paths.last().map_or(words::MISSING.into(), streams);
        let names: Vec<_> = self.view.servers.iter().map(|server| server.name.as_str()).collect();
        let (names, streams) = match self.several() {
            true => (format!("{} (all servers)", names.join(", ")), format!("per server · {streams}")),
            false => (names.join(", "), streams),
        };
        let (warmup, idle, loaded) = (
            words::setting(config.warmup),
            words::cadence(config.ping),
            words::cadence(config.loaded_ping),
        );
        let fields = [
            ("Servers", names),
            ("Throughput", throughput.join(" / ")),
            ("Latency", latency.map_or(words::MISSING.into(), words::latency_path)),
            ("Streams", streams),
            ("Timing", format!("warmup {warmup} · latency cadence {idle} · loaded cadence {loaded}")),
        ];
        let mut lines = Vec::new();
        for (name, value) in fields {
            let parts: Vec<String> = value.split(" · ").map(str::to_owned).collect();
            for (index, part) in wrap(&parts, width.saturating_sub(11).max(12)).into_iter().enumerate() {
                lines.push(label(if index == 0 { name } else { "" }).and(part, palette.value));
            }
        }
        lines
    }

    /// Each planned stage: waiting, its warmup, its window's progress, or how it ended.
    fn track(&self, run: &Run, width: usize) -> Vec<Line> {
        let (palette, live) = (&self.palette, run.outcome.is_none());
        let current = run.stage.as_ref().filter(|_| live);
        let current = current.map(|(plan, window)| (plan.stage, window.is_some()));
        let mut lines = Vec::new();
        for &(stage, duration) in &run.plan {
            let (hue, muted) = (palette.stage(stage), palette.muted);
            let result = run.results.iter().rfind(|result| result.stage == stage);
            let state = match result.map(|result| (result, result.status(run.focus.as_ref()))) {
                Some((result, StageStatus::Complete)) => {
                    let headline = Some(self.headline(result)).filter(|headline| headline.width() > 0);
                    let headline = headline.unwrap_or_else(|| Line::styled(words::setting(duration), muted));
                    Line::styled("✓ ", palette.ok).with(headline)
                }
                Some((result, StageStatus::Partial)) => {
                    let headline = self.headline(result);
                    let gap = if headline.width() > 0 { " " } else { "" };
                    Line::styled("! ", palette.warn)
                        .with(headline)
                        .and(format!("{gap}Partial"), muted)
                }
                Some((_, StageStatus::Failed)) => Line::styled("✗ ", palette.err).and("Failed", muted),
                Some((_, StageStatus::Stopped)) => Line::styled("○ Stopped", muted),
                None => match current.filter(|(current, _)| *current == stage) {
                    Some((_, true)) => {
                        let elapsed = self.elapsed();
                        bar(
                            hue,
                            elapsed.as_secs_f64() / duration.as_secs_f64(),
                            width.saturating_sub(34).clamp(6, 30),
                            muted,
                        )
                        .and("  ", Style::default())
                        .and(words::clock(elapsed.min(duration)), palette.value)
                        .and(format!(" / {}", words::setting(duration)), muted)
                    }
                    Some((_, false)) => Line::styled(self.spinner(), palette.accent)
                        .and(" warmup ", muted)
                        .and(words::clock(self.elapsed()), palette.value),
                    None if live => Line::styled(format!("○ {}", words::setting(duration)), muted),
                    None => Line::styled(format!("{} Skipped", words::MISSING), muted),
                },
            };
            lines.push(Line::styled(format!("{:<14}", words::label(stage)), hue).with(state));
        }
        lines
    }

    /// A finished stage's mean rates, and for the latency stage the shown server's median.
    fn headline(&self, result: &StageResult) -> Line {
        let mut line = Line::styled(words::mean_rates(result), self.palette.value);
        let own = result
            .servers
            .iter()
            .find(|own| Some(&own.server) == self.latency_server());
        let median = own.and_then(|own| own.latency?.median());
        let median = median.filter(|_| result.stage == Stage::Latency);
        if let Some(median) = median {
            let gap = if line.width() > 0 { "  " } else { "" };
            line = line
                .and(format!("{gap}{}", words::ms(median)), self.palette.value)
                .and(" median", self.palette.muted);
        }
        line
    }
}

/// `share` of `width` cells filled to an eighth, over the rest in shade.
fn bar(fill: Style, share: f64, width: usize, rest: Style) -> Line {
    let cells = (share * width as f64).clamp(0.0, width as f64);
    let (full, part) = (cells as usize, EIGHTHS[((cells.fract()) * 8.0) as usize]);
    let shade = width - full - usize::from(!part.is_empty());
    Line::styled(format!("{}{part}", "█".repeat(full)), fill).and("░".repeat(shade), rest)
}
