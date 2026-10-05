//! The printed report of a reduced view: its lines, plain and in the 16-colour profile, and its width.
use graphite_meter_client::{
    events::{Event, View},
    measure::{
        aggregate::Rate,
        latency::{Population, Summary},
    },
    model::{Dir, Failure, Outcome, Scope, ServerFailure, ServerResult, Stage, StageResult, Throughput},
    report::{WIDTH, progress, report, unstarted},
    run::prepare::ServerPath,
    text::{Line, Profile, write},
    tui::theme::Palette,
};
use graphite_meter_proto::{catalog::ServerId, origin::Origin, reason::FailureReason};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const SECOND: Duration = Duration::from_secs(1);

fn id(text: &str) -> ServerId {
    ServerId::parse(text).unwrap()
}

fn latency(median_ms: u64, replies: usize, timeouts: usize) -> Option<Population> {
    let median = Duration::from_millis(median_ms);
    let summary = Summary {
        replies,
        timeouts,
        p50: Some(median),
        p95: Some(median * 2),
        jitter: Some(Duration::from_micros(400)),
        jitter_pairs: replies - 1,
        ..Summary::default()
    };
    Some(Population { summary, complete: true })
}

fn server(name: &str, left: bool, down: Option<f64>, median_ms: u64) -> ServerResult {
    let throughput = Dir {
        down: down.map(|mean| Throughput { rate: Some(Rate { mean, peak: mean * 1.5 }), bytes: 0 }),
        up: None,
    };
    ServerResult {
        server: id(name),
        left,
        throughput,
        latency: latency(median_ms, 40, 1),
    }
}

/// A view of a latency and download run over `names`; the second leaves the download at 1.5 s.
fn view(names: &[&str]) -> View {
    let (at, mut view) = (Instant::now(), View::default());
    let origin = Origin::parse("https://meter.example").unwrap();
    let unchecked = Failure::new(FailureReason::Timeout, "unchecked");
    let servers = names.iter().map(|name| ServerPath {
        id: id(name),
        name: format!("{name} meter"),
        location: String::new(),
        origin: origin.clone(),
        offered: None,
        path: Err(unchecked.clone()),
    });
    let plan = vec![(Stage::Latency, SECOND * 4), (Stage::Download, SECOND * 10)];
    let shares = |left: bool, down| {
        names
            .iter()
            .enumerate()
            .map(move |(at, name)| server(name, left && at == 1, down, 12 + at as u64))
    };
    let mut download = StageResult {
        stage: Stage::Download,
        measured: SECOND * 10,
        stopped: false,
        throughput: Dir {
            down: Some(Throughput {
                rate: Some(Rate { mean: 1.25e7, peak: 1.5e7 }),
                bytes: 125_000_000,
            }),
            up: None,
        },
        servers: shares(true, Some(1.25e7 / names.len() as f64)).collect(),
        failures: Vec::new(),
        intervals: Vec::new(),
        omitted: 0,
    };
    if let [_, second, ..] = names {
        let failure = Failure::new(FailureReason::Timeout, "download bytes stopped growing for 2s");
        let at = at + SECOND * 15 / 2;
        download
            .failures
            .push(ServerFailure { server: id(second), scope: Scope::Throughput, failure, at });
    }
    let idle = StageResult {
        stage: Stage::Latency,
        measured: SECOND * 4,
        stopped: false,
        throughput: Dir::default(),
        servers: shares(false, None).collect(),
        failures: Vec::new(),
        intervals: Vec::new(),
        omitted: 0,
    };
    let events = [
        Event::Checking { run: true },
        Event::Prepared { servers: servers.collect(), catalogue: Arc::new([]) },
        Event::RunStarted { plan, focus: id(names[0]), at },
        Event::StageFinished(idle),
        Event::StageFinished(download),
        Event::RunFinished {
            outcome: Outcome::Complete,
            error: None,
            elapsed: Duration::from_millis(17_400),
        },
    ];
    events.iter().for_each(|event| view.apply(event));
    view
}

fn printed(lines: &[Line], profile: Profile) -> String {
    let mut out = Vec::new();
    write(lines, profile, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn a_report_prints_throughput_latency_and_notes_as_plain_text() {
    let lines = report(&view(&["a"]), WIDTH, &Palette::new(true));
    let expected = "\
Graphite Meter  Complete  a meter · 17.4 s · 125.0 MB

↓ Download  100.0 Mbit/s   peak 120.0 · 125.0 MB · 10.0 s

Latency      Median   Added    P95      Jitter  Probe timeouts
Idle         12.0 ms           24.0 ms  0.4 ms  1 / 41 (2.4%)
Loaded down  12.0 ms  +0.0 ms  24.0 ms  0.4 ms  1 / 41 (2.4%)

Idle latency: 40 replies · 4.0 s
Loaded latency · Download: 40 replies · 10.0 s
Added: loaded median minus idle median, same server.
";
    assert_eq!(printed(&lines, Profile::Plain), expected);
}

#[test]
fn several_servers_add_each_server_s_share_and_the_issues() {
    let lines = report(&view(&["a", "b"]), WIDTH, &Palette::new(true));
    let text = printed(&lines, Profile::Plain);
    assert!(text.starts_with("Graphite Meter  Complete  2 servers · 17.4 s · 125.0 MB\n"), "{text}");
    assert!(text.contains("\nLatency to a meter  Median"), "{text}");
    assert!(text.contains("\nComplete · 1 of 2 servers\n"), "{text}");
    assert!(
        text.contains("\nAll servers  100.0 Mbit/s\na meter      50.00 Mbit/s\nb meter ✗    50.00 Mbit/s\n"),
        "{text}"
    );
    assert!(
        text.ends_with("\nIssues\nb meter · Download throughput · at 7.5 s · Stopped delivering data\n"),
        "{text}"
    );
}

#[test]
fn the_16_colour_profile_writes_gos_sgr_codes_and_a_narrow_report_fits_its_width() {
    let lines = report(&view(&["a", "b"]), 40, &Palette::new(false));
    let text = printed(&lines, Profile::Ansi);
    let codes: Vec<u8> = text
        .split("\x1b[")
        .skip(1)
        .flat_map(|sequence| {
            sequence
                .split_once('m')
                .unwrap()
                .0
                .split(';')
                .filter(|code| !code.is_empty())
        })
        .map(|code| code.parse().unwrap())
        .collect();
    assert!(codes.contains(&1) && codes.contains(&90) && codes.contains(&34), "{codes:?}");
    assert!(codes.iter().all(|code| matches!(code, 1 | 30..=37 | 90..=97)), "{codes:?}");
    let plain = printed(&lines, Profile::Plain);
    assert!(plain.contains("\nIdle\n  Median 12.0 ms · P95 24.0 ms\n  Jitter 0.4 ms\n"), "{plain}");
    let details = plain.split_once("\nComplete · 1 of 2 servers\n").unwrap().1;
    assert!(details.lines().all(|line| line.chars().count() <= 40), "{details}");
    assert!(details.ends_with("\nb meter · Download throughput · at 7.5 …\n"), "{details}");
}

#[test]
fn a_run_that_never_started_reports_why_and_progress_names_each_stage() {
    let mut view = View::default();
    view.apply(&Event::Checking { run: true });
    view.apply(&Event::RunFinished { outcome: Outcome::Stopped, error: None, elapsed: SECOND });
    assert!(report(&view, WIDTH, &Palette::new(true)).is_empty());
    assert_eq!(unstarted(&view).as_deref(), Some("Test stopped before it started."));
    let error = Some(Failure::new(FailureReason::ConnectionLost, "Server could not be reached"));
    view.apply(&Event::RunFinished { outcome: Outcome::Failed, error, elapsed: SECOND });
    assert_eq!(unstarted(&view).as_deref(), Some("Test could not start: Server could not be reached"));
    assert_eq!(progress(&Event::Measuring(Stage::Bidirectional)).as_deref(), Some("Bidirectional…"));
}
