//! The interface during and after a run: its panels, stop prompt, Details, latency server, charts, chrome, theme and
//! quitting.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use graphite_meter_client::{
    INTERRUPTED, TERMINATED,
    config::Config,
    controller::Command,
    events::Event,
    measure::{
        aggregate::Rate,
        latency::{Population, Summary},
    },
    model::{Dir, Outcome, ServerResult, Stage, StageResult, Throughput},
    net::ThroughputPath,
    report::report,
    run::{
        engine::StagePlan,
        prepare::{Paths, ServerPath},
    },
    status,
    text::{Line, Profile},
    tui::{
        App, Effect,
        chrome::{Chrome, Link, Progress},
        theme::{self, Palette},
    },
};
use graphite_meter_proto::{
    catalog::ServerId,
    discovery::{Protocol, ThroughputTransport},
    origin::Origin,
};
use ratatui_core::{buffer::Buffer, layout::Rect, style::Color};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

const WIDTH: u16 = 120;
const SECOND: Duration = Duration::from_secs(1);

fn id(name: &str) -> ServerId {
    ServerId::parse(name).unwrap()
}

fn draw(app: &mut App, now: Instant) -> (Buffer, Chrome) {
    let mut buffer = Buffer::empty(Rect::new(0, 0, WIDTH, 40));
    let chrome = app.draw(&mut buffer, now);
    (buffer, chrome)
}

/// The frame's rows as drawn.
fn rows(app: &mut App, now: Instant) -> Vec<String> {
    let (buffer, _) = draw(app, now);
    let row = |cells: &[ratatui_core::buffer::Cell]| cells.iter().map(|cell| cell.symbol()).collect::<String>();
    buffer.content().chunks(usize::from(WIDTH)).map(row).collect()
}

fn shows(app: &mut App, now: Instant, text: &str) -> bool {
    let squeezed = |row: &String| row.split_whitespace().collect::<Vec<_>>().join(" ");
    rows(app, now).iter().map(squeezed).any(|row| row.contains(text))
}

fn press(app: &mut App, code: KeyCode, now: Instant) -> Vec<Effect> {
    app.key(KeyEvent::new(code, KeyModifiers::NONE), now)
}

fn command(effects: &[Effect]) -> &Command {
    match effects {
        [Effect::Command(command)] => command,
        effects => panic!("{effects:?}"),
    }
}

/// A run of `plan` over `names`, started and prepared at `now`.
fn running(names: &[&str], plan: &[(Stage, Duration)], now: Instant) -> App {
    let stages = plan.iter().map(|(stage, _)| *stage).collect();
    let mut app = App::new(Config { stages, ..Config::default() }, Profile::Plain, now);
    assert!(matches!(command(&press(&mut app, KeyCode::Char('r'), now)), Command::Run(_)));
    let origin = Origin::parse("https://meter.example").unwrap();
    let paths = Paths {
        throughput: ThroughputPath {
            origin: origin.clone(),
            transport: ThroughputTransport::FetchStream,
            protocol: Protocol::Http2,
        },
        latency: None,
        stage_limit: SECOND * 300,
        idle_rtt: Duration::ZERO,
    };
    let server = |name: &&str| ServerPath {
        id: id(name),
        name: name.to_uppercase(),
        location: String::new(),
        origin: origin.clone(),
        offered: None,
        path: Ok(paths.clone()),
    };
    let servers = names.iter().map(server).collect();
    app.event(&Event::Prepared { servers, catalogue: Arc::new([]) }, now);
    let started = Event::RunStarted { plan: plan.to_vec(), focus: id(names[0]), at: now };
    app.event(&started, now);
    app
}

/// Opens `stage`'s window at `now`.
fn measuring(app: &mut App, stage: Stage, duration: Duration, now: Instant) {
    let plan = StagePlan { stage, members: Vec::new(), duration, latency: None };
    app.event(&Event::StageStarted(plan), now);
    app.event(&Event::Measuring(stage), now);
}

fn sample(app: &mut App, at: Duration, down: Option<f64>, up: Option<f64>, now: Instant) {
    let rates = Dir { down, up };
    app.event(&Event::Sample { at, rates, recovering: false }, now);
}

fn population(median: Duration) -> Option<Population> {
    let summary = Summary { replies: 20, p50: Some(median), ..Summary::default() };
    Some(Population { summary, complete: true })
}

fn finished(stage: Stage, servers: Vec<ServerResult>, down: Option<f64>) -> Event {
    let throughput = Dir {
        down: down.map(|mean| Throughput { rate: Some(Rate { mean, peak: mean }), bytes: 1 }),
        up: None,
    };
    Event::StageFinished(StageResult {
        stage,
        measured: SECOND * 10,
        stopped: false,
        throughput,
        servers,
        failures: Vec::new(),
        intervals: Vec::new(),
        omitted: 0,
    })
}

fn ended(outcome: Outcome) -> Event {
    Event::RunFinished { outcome, error: None, elapsed: SECOND * 12 }
}

fn done(app: &mut App, now: Instant) {
    app.event(&ended(Outcome::Complete), now);
}

#[test]
fn a_run_shows_its_stages_live_readings_and_then_its_results() {
    let start = Instant::now();
    let plan = [(Stage::Download, SECOND * 10), (Stage::Upload, SECOND * 10)];
    let mut app = running(&["a"], &plan, start);
    assert!(shows(&mut app, start, "Test started. Press esc to stop."));
    measuring(&mut app, Stage::Download, SECOND * 10, start);
    sample(&mut app, SECOND / 4, Some(1.25e7), None, start);
    app.tick(start + SECOND);
    let later = start + SECOND * 2;
    let frame = rows(&mut app, later).join("\n");
    assert!(frame.lines().next().unwrap().trim_end().ends_with("Download"), "{frame}");
    for text in [
        "Timeline · Download",
        "↓ 100.0 Mbit/s",
        "2.0 s / 10 s",
        "○ 10 s",
        "Fetch streams · HTTP/2 · TLS",
    ] {
        assert!(shows(&mut app, later, text), "{text}\n{frame}");
    }

    let a = ServerResult {
        server: id("a"),
        left: false,
        throughput: Dir::default(),
        latency: None,
    };
    app.event(&finished(Stage::Download, vec![a.clone()], Some(1.25e7)), later);
    app.event(&finished(Stage::Upload, vec![a], None), later);
    done(&mut app, later);
    for text in ["Complete", "Results", "Throughput", "Download ↓ 100.0 Mbit/s", "Timeline"] {
        assert!(shows(&mut app, later, text), "{text}\n{:#?}", rows(&mut app, later));
    }
    assert!(shows(&mut app, later, "enter run again • esc setup • d details"));
}

#[test]
fn esc_asks_to_stop_and_a_second_esc_stops() {
    let now = Instant::now();
    let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
    assert_eq!(press(&mut app, KeyCode::Esc, now), []);
    assert!(shows(&mut app, now, "Stop the test? esc confirms, any other key continues."));
    assert!(shows(&mut app, now, "esc confirm stop • q quit"));
    assert_eq!(press(&mut app, KeyCode::Char('x'), now), []);
    assert!(shows(&mut app, now, "Test continues."));
    assert_eq!(press(&mut app, KeyCode::Esc, now), []);
    assert_eq!(command(&press(&mut app, KeyCode::Esc, now)), &Command::Stop);
    assert!(shows(&mut app, now, "Stopping the test…"));
}

#[test]
fn d_opens_details_and_esc_closes_them() {
    let now = Instant::now();
    let mut app = running(&["a", "b"], &[(Stage::Latency, SECOND * 4)], now);
    press(&mut app, KeyCode::Char('d'), now);
    assert!(shows(&mut app, now, "Details"));
    assert!(shows(&mut app, now, "Latency median by server"));
    assert!(shows(&mut app, now, "↑/↓ scroll • esc close • q quit"));
    press(&mut app, KeyCode::Esc, now);
    assert!(!shows(&mut app, now, "Latency median by server"));
    assert!(shows(&mut app, now, "esc stop test • d details • l latency server"));
}

#[test]
fn l_changes_the_shown_latency_server_and_not_the_reports() {
    let now = Instant::now();
    let mut app = running(&["a", "b"], &[(Stage::Latency, SECOND * 4)], now);
    measuring(&mut app, Stage::Latency, SECOND * 4, now);
    for (server, ms) in [("a", 10), ("b", 30)] {
        let rtt = Some(Duration::from_millis(ms));
        app.event(&Event::Probe { server: id(server), at: SECOND, rtt }, now);
    }
    assert!(shows(&mut app, now, "Latency to A · l switches server"));
    assert!(shows(&mut app, now, "Idle latency 10.0 ms"));
    press(&mut app, KeyCode::Char('l'), now);
    assert!(shows(&mut app, now, "Latency to B · l switches server"));
    assert!(shows(&mut app, now, "Idle latency 30.0 ms"));

    let own = |name: &str, ms| ServerResult {
        server: id(name),
        left: false,
        throughput: Dir::default(),
        latency: population(Duration::from_millis(ms)),
    };
    app.event(&finished(Stage::Latency, vec![own("a", 10), own("b", 30)], None), now);
    done(&mut app, now);
    assert!(shows(&mut app, now, "Results · latency to B"));
    assert!(shows(&mut app, now, "Idle 30.0 ms"));
    let lines = report(&app.exit().view, 100, &Palette::new(true));
    let texts: Vec<_> = lines.iter().map(Line::text).collect();
    assert!(texts.iter().any(|text| text.starts_with("Latency to A")), "{texts:#?}");
}

#[test]
fn charts_keep_the_planned_span_and_dash_bidirectional_upload() {
    let now = Instant::now();
    let mut app = running(&["a"], &[(Stage::Bidirectional, SECOND * 10)], now);
    measuring(&mut app, Stage::Bidirectional, SECOND * 10, now);
    for quarter in 1..=8 {
        sample(&mut app, SECOND * quarter / 4, Some(1.25e7), Some(6.25e6), now);
    }
    assert!(shows(&mut app, now, "↓ solid · ↑ dashed"));
    assert!(shows(&mut app, now, "Bi-dir 10.0 s"), "the ruler ends at the planned span");
    let braille = |c: char| ('\u{2801}'..='\u{28ff}').contains(&c);
    let traces: Vec<String> = rows(&mut app, now)
        .into_iter()
        .filter(|row| row.chars().any(braille))
        .map(|row| {
            let cells: Vec<char> = row.chars().collect();
            let first = cells.iter().position(|&c| braille(c)).unwrap();
            let last = cells.iter().rposition(|&c| braille(c)).unwrap();
            assert!(cells.len() - last > 40, "a trace two seconds into ten fills a fifth: {row}");
            cells[first..=last].iter().collect()
        })
        .collect();
    assert_eq!(traces.len(), 2, "{traces:#?}");
    assert!(!traces[0].contains(' '), "download is solid: {}", traces[0]);
    assert!(traces[1].contains(' '), "upload is dashed: {}", traces[1]);
}

#[test]
fn chrome_titles_the_window_and_shows_progress_until_the_end() {
    let start = Instant::now();
    let mut app = App::new(Config { stages: vec![Stage::Download], ..Config::default() }, Profile::Plain, start);
    let (_, setup) = draw(&mut app, start);
    assert_eq!(setup.progress, Progress::None);
    let bytes = setup.bytes(None, Profile::Plain);
    assert_eq!(bytes, "\x1b]2;Graphite Meter · Checking paths\x07\x1b]9;4;0\x07".as_bytes());

    press(&mut app, KeyCode::Char('r'), start);
    let (_, preparing) = draw(&mut app, start);
    assert_eq!(preparing.bytes(Some(&setup), Profile::Plain), b"\x1b]9;4;3\x07");

    app.event(
        &Event::RunStarted {
            plan: vec![(Stage::Download, SECOND * 10)],
            focus: id("a"),
            at: start,
        },
        start,
    );
    measuring(&mut app, Stage::Download, SECOND * 10, start);
    let (_, halfway) = draw(&mut app, start + SECOND * 5);
    let bytes = halfway.bytes(Some(&preparing), Profile::Plain);
    assert_eq!(bytes, "\x1b]2;Graphite Meter · Download\x07\x1b]9;4;1;50\x07".as_bytes());
    assert_eq!(halfway.bytes(Some(&halfway), Profile::Plain), b"");

    done(&mut app, start + SECOND * 10);
    let (_, finished) = draw(&mut app, start + SECOND * 10);
    let bytes = finished.bytes(Some(&halfway), Profile::Plain);
    assert_eq!(bytes, "\x1b]2;Graphite Meter · Complete\x07\x1b]9;4;0\x07".as_bytes());
    assert_eq!(Chrome::default().bytes(Some(&finished), Profile::Plain), b"\x1b]2;\x07");

    let link = Link {
        row: 3,
        column: 1,
        url: "https://a.example/x\x1b".into(),
        text: Line::plain("open"),
    };
    let linked = Chrome { links: vec![link], ..finished.clone() };
    let bytes = linked.bytes(Some(&finished), Profile::Plain);
    assert_eq!(bytes, b"\x1b[4;2H\x1b]8;;https://a.example/x\x1b\\open\x1b]8;;\x1b\\");
}

#[test]
fn the_background_answer_arriving_as_keys_sets_the_palette() {
    let now = Instant::now();
    let mut app = App::new(Config::default(), Profile::Ansi, now);
    let title = |app: &mut App| draw(app, now).0[(1, 0)].bg;
    assert_eq!(title(&mut app), Color::Indexed(15));
    assert_eq!(app.key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT), now), []);
    for c in "11;rgb:FDFD/f6f6/e3e3".chars() {
        assert_eq!(press(&mut app, KeyCode::Char(c), now), [], "{c}");
    }
    assert_eq!(app.key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::ALT), now), []);
    assert_eq!(title(&mut app), Color::Indexed(0));
    assert!(matches!(command(&press(&mut app, KeyCode::Char('r'), now)), Command::Run(_)));

    let mut split = App::new(Config::default(), Profile::Ansi, now);
    for c in "\x1b]11;rgb:FDFD/f6f6/e3e3".chars() {
        let code = if c == '\x1b' { KeyCode::Esc } else { KeyCode::Char(c) };
        assert_eq!(press(&mut split, code, now), [], "an answer split after its escape starts no test: {c}");
    }
    assert_eq!(split.key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::ALT), now), []);
    assert_eq!(title(&mut split), Color::Indexed(0));
}

#[test]
fn the_colour_profile_follows_the_environment() {
    for (tty, env, expected) in [
        (true, "TERM=xterm", Profile::Ansi),
        (true, "TERM=xterm-256color", Profile::Ansi256),
        (true, "TERM=xterm-256color COLORTERM=truecolor", Profile::TrueColor),
        (true, "TERM=xterm-kitty", Profile::TrueColor),
        (true, "TERM=screen COLORTERM=truecolor", Profile::Ansi256),
        (true, "TERM=xterm TMUX=/tmp/tmux", Profile::Ansi256),
        (true, "TERM=dumb", Profile::Plain),
        (true, "TERM=dumb CLICOLOR=1", Profile::Plain),
        (true, "TERM=xterm-256color NO_COLOR=1", Profile::Ascii),
        (true, "TERM=dumb NO_COLOR=1", Profile::Plain),
        (false, "TERM=xterm-256color", Profile::Plain),
        (false, "TERM=xterm-256color CLICOLOR_FORCE=1", Profile::Ansi256),
    ] {
        let vars: HashMap<_, _> = env.split(' ').filter_map(|pair| pair.split_once('=')).collect();
        let profile = theme::profile(tty, |name| vars.get(name).map(|value| value.to_string()));
        assert_eq!(profile, expected, "{env} on a terminal: {tty}");
    }
}

#[test]
fn q_during_a_run_stops_it_and_then_quits_with_its_report() {
    let now = Instant::now();
    let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
    press(&mut app, KeyCode::Esc, now);
    assert_eq!(command(&press(&mut app, KeyCode::Char('q'), now)), &Command::Stop);
    assert!(shows(&mut app, now, "Stopping the test before quitting… ctrl+c quits at once."));
    assert_eq!(press(&mut app, KeyCode::Char('q'), now), []);
    assert_eq!(app.event(&ended(Outcome::Stopped), now), [Effect::Quit]);
    let exit = app.exit();
    assert!(exit.report);
    assert_eq!(status(&exit.view, exit.signal), 1);
    let lines = report(&exit.view, 100, &exit.palette);
    assert!(lines[0].text().starts_with("Graphite Meter  Stopped"), "{:?}", lines[0]);
}

#[test]
fn ctrl_c_or_a_signal_stops_a_run_and_a_second_quits_at_once() {
    let now = Instant::now();
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    for (key, code) in [(true, INTERRUPTED), (false, TERMINATED)] {
        let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
        let interrupt = |app: &mut App| if key { app.key(ctrl_c, now) } else { app.interrupt(code) };
        assert_eq!(command(&interrupt(&mut app)), &Command::Stop);
        assert_eq!(interrupt(&mut app), [Effect::Quit]);
        let exit = app.exit();
        assert!(!exit.report, "a running test has no report");
        assert_eq!(status(&exit.view, exit.signal), code);
    }
}

#[test]
fn quitting_a_finished_run_reports_it_and_after_setup_does_not() {
    let now = Instant::now();
    for (setup, report) in [(false, true), (true, false)] {
        let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
        done(&mut app, now);
        if setup {
            press(&mut app, KeyCode::Esc, now);
        }
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.key(ctrl_c, now), [Effect::Quit]);
        let exit = app.exit();
        assert_eq!(exit.report, report);
        assert_eq!(status(&exit.view, exit.signal), 0);
    }
}

#[test]
fn esc_after_a_finished_run_returns_to_start_test() {
    let now = Instant::now();
    let mut app = App::new(Config::default(), Profile::Plain, now);
    press(&mut app, KeyCode::Down, now);
    assert!(matches!(command(&press(&mut app, KeyCode::Char('r'), now)), Command::Run(_)));
    app.event(
        &Event::RunStarted {
            plan: vec![(Stage::Latency, SECOND)],
            focus: id("a"),
            at: now,
        },
        now,
    );
    done(&mut app, now);
    press(&mut app, KeyCode::Esc, now);
    assert!(shows(&mut app, now, "› Start test"));
}
