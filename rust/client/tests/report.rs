//! The printed report of a reduced view: its lines, plain and in the 16-colour profile, and its width.
use graphite_meter_client::{
    events::{Event, View},
    measure::{
        aggregate::{Rate, Reading},
        latency::{Population, Summary, Timing},
    },
    model::{Dir, Failure, LaneHealth, Outcome, Scope, ServerFailure, ServerResult, Stage, StageResult, Throughput},
    report::{WIDTH, details, progress, report, unreported},
    run::{
        engine::{Decision, Engine, Input, Member, Sample, StagePlan},
        prepare::ServerPath,
    },
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
fn full_details_open_with_each_result_s_facts_and_without_intervals_leave_out_their_heading() {
    let mut view = view(&["a", "b"]);
    let idle = &mut view.run.as_mut().unwrap().results[0].servers[0];
    let idle = idle.latency.as_mut().unwrap();
    let handling = Duration::from_micros(300);
    idle.summary.timing = Some(Timing { pairs: 38, rtt: Duration::from_millis(12), handling });
    let text = printed(&details(&view, WIDTH, &Palette::new(true), true), Profile::Plain);
    let expected = "\
Complete · 1 of 2 servers
Idle latency: 40 replies · 4.0 s
Server timing (38 paired replies, means): raw 12.0 ms · handling 0.3 ms
Download: peak 120.0 Mbit/s · 125.0 MB · 10.0 s
Loaded latency · Download: 40 replies · 10.0 s
Added: loaded median minus idle median, same server.

Server       Download
All servers  100.0 Mbit/s
a meter      50.00 Mbit/s
b meter ✗    50.00 Mbit/s

Latency median by server
Server   Idle     Loaded down
a meter  12.0 ms  12.0 ms
b meter  13.0 ms  13.0 ms

Issues
b meter · Download throughput · at 7.5 s · Stopped delivering data
";
    assert_eq!(text, expected);
}

/// A download stage of server `a` measured by the engine from `base`, one kilobyte per millisecond.
fn measured_download(base: Instant) -> StageResult {
    let member = Member { server: id("a"), warmup: Duration::ZERO };
    let plan = StagePlan {
        stage: Stage::Download,
        members: vec![member],
        duration: SECOND * 2,
        latency: None,
    };
    let (mut engine, mut at) = (Engine::new(plan, base), base);
    loop {
        let down = Some(at.duration_since(base).as_millis() as u64 * 1000);
        let reading = Reading { server: id("a"), down, up: None, fed: None };
        let lanes = Dir { down: LaneHealth::Ok, up: LaneHealth::Ok };
        let samples = [Sample { reading, ready: true, missed: None, lanes }];
        let input = Input {
            now: at,
            lateness: Duration::ZERO,
            samples: &samples,
            probes: &[],
            departed: &[],
        };
        let tick = engine.tick(input);
        if tick.decisions.contains(&Decision::Finish) {
            return engine.result();
        }
        at = tick.next;
    }
}

#[test]
fn full_details_end_with_the_aggregation_intervals_of_a_finished_run() {
    let mut view = view(&["a", "b"]);
    let run = view.run.as_mut().unwrap();
    let measured = measured_download(run.at.unwrap());
    run.results[1].intervals = measured.intervals;
    let text = printed(&details(&view, WIDTH, &Palette::new(true), true), Profile::Plain);
    let intervals = "\n\nAggregation intervals\nDownload 0.0–2.0 s · a meter · measured window\n";
    assert!(text.ends_with(intervals), "{text}");
}

#[test]
fn the_16_colour_and_ascii_profiles_write_their_codes_and_a_narrow_report_fits_its_width() {
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
    let ascii = printed(&lines, Profile::Ascii);
    assert!(ascii.starts_with("\x1b[1mGraphite Meter\x1b[m"), "{ascii}");
    assert_eq!(ascii.replace("\x1b[1m", "").replace("\x1b[m", ""), printed(&lines, Profile::Plain));
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
    assert_eq!(unreported(&view).as_deref(), Some("Test stopped before it started."));
    let error = Some(Failure::new(FailureReason::ConnectionLost, "Server could not be reached"));
    view.apply(&Event::RunFinished { outcome: Outcome::Failed, error, elapsed: SECOND });
    assert_eq!(unreported(&view).as_deref(), Some("Test could not start: Server could not be reached"));
    assert_eq!(progress(&Event::Measuring(Stage::Bidirectional)).as_deref(), Some("Bidirectional…"));
}

/// A finished download run over the server `a`, which `result` describes; its stage over `failures`.
fn sole(mut result: StageResult, outcome: Outcome, error: Option<Failure>) -> String {
    let (at, mut view) = (Instant::now(), View::default());
    let server = ServerPath {
        id: id("a"),
        name: "a meter".into(),
        location: String::new(),
        origin: Origin::parse("https://meter.example").unwrap(),
        offered: None,
        path: Err(Failure::new(FailureReason::Timeout, "unchecked")),
    };
    for failure in &mut result.failures {
        failure.at = at + SECOND * 5;
    }
    let events = [
        Event::Checking { run: true },
        Event::Prepared { servers: Arc::new([server]), catalogue: Arc::new([]) },
        Event::RunStarted {
            plan: vec![(Stage::Download, SECOND * 10)],
            focus: id("a"),
            at,
        },
        Event::StageFinished(result),
        Event::RunFinished { outcome, error, elapsed: SECOND * 12 },
    ];
    events.iter().for_each(|event| view.apply(event));
    printed(&report(&view, WIDTH, &Palette::new(true)), Profile::Plain)
}

/// The download stage over `a`: measured for `measured` at 100 Mbit/s when `rate`, `a` leaving with `failure`.
fn download(measured: Duration, rate: bool, failure: Option<(Scope, FailureReason)>) -> StageResult {
    let throughput = (!measured.is_zero()).then(|| Throughput {
        rate: rate.then_some(Rate { mean: 1.25e7, peak: 1.5e7 }),
        bytes: 125_000_000,
    });
    let failures = failure.map(|(scope, reason)| ServerFailure {
        server: id("a"),
        scope,
        failure: Failure::new(reason, "lost"),
        at: Instant::now(),
    });
    let left = failures
        .as_ref()
        .is_some_and(|failure| failure.failure.reason != FailureReason::InsufficientEvidence);
    StageResult {
        stage: Stage::Download,
        measured,
        stopped: false,
        throughput: Dir { down: throughput, up: None },
        servers: vec![ServerResult {
            server: id("a"),
            left,
            throughput: Dir { down: throughput, up: None },
            latency: latency(12, 40, 1),
        }],
        failures: failures.into_iter().collect(),
        intervals: Vec::new(),
        omitted: 0,
    }
}

#[test]
fn a_sole_server_that_left_names_why_under_its_partial_rate_and_latency() {
    let result = download(SECOND * 10, true, Some((Scope::Throughput, FailureReason::ConnectionLost)));
    let expected = "\
Graphite Meter  Partial  a meter · 12.0 s · 125.0 MB

↓ Download  100.0 Mbit/s   Partial · peak 120.0 · 125.0 MB · 10.0 s

Latency      Median   Added  P95      Jitter  Probe timeouts
Loaded down  12.0 ms  —      24.0 ms  0.4 ms  1 / 41 (2.4%)

Download: Connection lost
Loaded latency · Download: Connection lost

Loaded latency · Download: 40 replies · 10.0 s
Added: loaded median minus idle median, same server.
";
    assert_eq!(sole(result, Outcome::Partial, None), expected);
}

#[test]
fn too_little_measured_time_shows_only_a_dash() {
    let result = download(SECOND * 10, false, Some((Scope::Throughput, FailureReason::InsufficientEvidence)));
    let text = sole(result, Outcome::Incomplete, None);
    assert!(text.contains("\n↓ Download  —\n\nLatency "), "{text}");
    assert!(!text.contains("Too little measured time"), "{text}");
}

#[test]
fn a_stop_before_the_window_shows_stopped_and_a_zero_window_no_duration() {
    let mut result = download(Duration::ZERO, false, None);
    result.stopped = true;
    let text = sole(result, Outcome::Stopped, None);
    assert!(text.contains("\n↓ Download  Stopped\n"), "{text}");
    assert!(text.contains("\nLoaded latency · Download stopped.\n"), "{text}");
    assert!(!text.contains("\nDownload stopped."), "{text}");
    assert!(text.contains("\nLoaded latency · Download: 40 replies\n"), "{text}");
}

#[test]
fn a_started_run_s_error_ends_its_report_and_an_expired_sign_in_withholds_it() {
    let failure = Some((Scope::Throughput, FailureReason::ConnectionLost));
    let error = Failure::new(FailureReason::ConnectionLost, "all selected servers failed: lost");
    let text = sole(download(SECOND * 10, true, failure), Outcome::Incomplete, Some(error));
    assert!(text.ends_with("\n\nall selected servers failed: lost\n"), "{text}");
    let error = Failure::new(FailureReason::SignInRequired, "all selected servers failed: sign in");
    let failure = Some((Scope::Throughput, FailureReason::SignInRequired));
    assert_eq!(sole(download(SECOND * 10, true, failure), Outcome::Incomplete, Some(error)), "");
}
