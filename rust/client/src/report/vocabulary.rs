//! The words and numbers views print: stage and population labels, durations, counts and a run's cells.
use crate::{
    measure::{
        format,
        latency::{Population, added},
    },
    model::{Direction, Outcome, Stage, Throughput},
};
use std::time::Duration;

/// What a cell shows without a value.
pub const MISSING: &str = "—";

/// Median, Added, P95, jitter and probe timeouts.
pub fn latency_cells(population: &Population, idle: Option<Duration>) -> Vec<String> {
    let (summary, median) = (population.summary, population.median());
    let mut cells = vec![MISSING.to_owned(); 5];
    if let Some(median) = median {
        cells[0] = ms(median);
        cells[1] = added(Some(median), idle).map_or(MISSING.into(), |added| format!("{} ms", format::added(added)));
    }
    if summary.replies > 0 {
        cells[2] = summary.p95.map_or(MISSING.into(), ms);
    }
    if summary.jitter_pairs > 0 {
        cells[3] = summary.jitter.map_or(MISSING.into(), ms);
    }
    if let Some(ratio) = summary.timeout_ratio() {
        cells[4] = format!("{} / {}", count(summary.timeouts), count(summary.replies + summary.timeouts));
        if ratio >= 0.01 {
            cells[4].push_str(&format!(" ({:.1}%)", ratio * 100.0));
        } else if ratio > 0.0 {
            cells[4].push_str(&format!(" ({:.2}%)", ratio * 100.0));
        }
    }
    cells
}

/// The peak without the mean's unit, bytes, the window and, for uploads, that the receiver timed it.
pub fn throughput_facts(throughput: Throughput, measured: Duration, direction: Direction) -> Vec<String> {
    let mut facts = Vec::new();
    if let Some(rate) = throughput.rate.filter(|rate| rate.peak > 0.0) {
        let (peak, mean) = (format::rate(rate.peak), format::rate(rate.mean));
        let unit = mean
            .rsplit_once(' ')
            .map(|(_, unit)| format!(" {unit}"))
            .unwrap_or_default();
        facts.push(format!("peak {}", peak.strip_suffix(&unit).unwrap_or(&peak)));
    }
    facts.push(format::bytes(throughput.bytes));
    facts.extend((!measured.is_zero()).then(|| clock(measured)));
    facts.extend((direction == Direction::Up).then(|| "receiver-timed".to_owned()));
    facts
}

pub fn label(stage: Stage) -> &'static str {
    ["Latency", "Download", "Upload", "Bidirectional"][stage as usize]
}

pub fn compact_stage(stage: Stage) -> &'static str {
    if stage == Stage::Bidirectional { "Bi-dir" } else { label(stage) }
}

pub fn compact_population(stage: Stage) -> &'static str {
    ["Idle", "Loaded down", "Loaded up", "Loaded bi-dir"][stage as usize]
}

pub fn population_label(stage: Stage) -> String {
    match stage {
        Stage::Latency => "Idle latency".into(),
        stage => format!("Loaded latency · {}", label(stage)),
    }
}

pub fn direction_label(stage: Stage, direction: Direction) -> String {
    match stage {
        Stage::Bidirectional => format!("Bi-dir {}", arrow(direction)),
        stage => label(stage).into(),
    }
}

pub fn arrow(direction: Direction) -> &'static str {
    if direction == Direction::Down { "↓" } else { "↑" }
}

pub fn outcome_label(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Complete => "Complete",
        Outcome::Partial => "Partial",
        Outcome::Incomplete => "Incomplete",
        Outcome::Stopped => "Stopped",
        Outcome::Failed => "Failed",
    }
}

pub fn clock(duration: Duration) -> String {
    format!("{:.1} s", duration.as_secs_f64())
}

pub fn ms(duration: Duration) -> String {
    format!("{} ms", format::latency(duration.as_secs_f64() * 1e3))
}

/// Thousands separated by commas.
pub fn count(value: usize) -> String {
    let digits = value.to_string();
    let groups: Vec<_> = digits
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(String::from_utf8_lossy)
        .collect();
    groups.join(",")
}
