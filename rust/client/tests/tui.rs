//! The interface without a terminal: keys, events, signals and ticks in; frames, chrome, effects and exits out.
use crossterm::event::{Event as Input, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use graphite_meter_client::{
    INTERRUPTED, TERMINATED,
    config::{self, Config},
    controller::Command,
    events::{Event, SignInEnd, SignInPrompt},
    measure::aggregate::Rate,
    model::{Dir, Outcome, Stage, StageResult, Throughput},
    net::{ThroughputPath, approval::Unapproved},
    report::report,
    run::{
        engine::StagePlan,
        prepare::{Paths, ServerPath},
    },
    status,
    text::Profile,
    tui::{App, Effect, chrome::Chrome},
};
use graphite_meter_proto::{
    catalog::{ServerEntry, ServerId},
    discovery::{Protocol, ThroughputTransport},
    origin::{BaseUrl, Origin},
};
use ratatui_core::{buffer::Buffer, layout::Rect};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const FULL: Rect = Rect::new(0, 0, 120, 40);
const SECOND: Duration = Duration::from_secs(1);
const PAGE: &str = "https://meter.example/auth/cli?challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn id(name: &str) -> ServerId {
    ServerId::parse(name).unwrap()
}

/// The default settings for a server at meter.example.
fn config() -> Config {
    Config {
        url: Origin::parse("https://meter.example").ok(),
        ..Config::default()
    }
}

/// Setup for the default settings, its first check already asked for.
fn app() -> (App, Instant) {
    let start = Instant::now();
    let mut app = App::new(config(), Profile::Plain, start);
    assert_eq!(checked(app.tick(start)), config());
    (app, start)
}

fn command(effects: &[Effect]) -> &Command {
    match effects {
        [Effect::Command(command)] => command,
        effects => panic!("{effects:?}"),
    }
}

/// The settings of the one check `effects` ask for.
fn checked(effects: Vec<Effect>) -> Config {
    match command(&effects) {
        Command::Check(config) => config.clone(),
        command => panic!("{command:?}"),
    }
}

fn draw(app: &mut App, now: Instant, area: Rect) -> (Buffer, Chrome) {
    let mut buffer = Buffer::empty(area);
    let chrome = app.draw(&mut buffer, now);
    (buffer, chrome)
}

/// The frame's rows as drawn.
fn rows(app: &mut App, now: Instant) -> Vec<String> {
    let (buffer, _) = draw(app, now, FULL);
    let row = |cells: &[ratatui_core::buffer::Cell]| cells.iter().map(|cell| cell.symbol()).collect::<String>();
    buffer.content().chunks(usize::from(FULL.width)).map(row).collect()
}

/// The frame's rows, each run of spaces as one.
fn frame(app: &mut App, now: Instant) -> Vec<String> {
    let squeezed = |row: String| row.split_whitespace().collect::<Vec<_>>().join(" ");
    rows(app, now).into_iter().map(squeezed).collect()
}

fn shows(app: &mut App, now: Instant, text: &str) -> bool {
    frame(app, now).iter().any(|row| row.contains(text))
}

/// The focused row of the frame's body.
fn focused(app: &mut App, now: Instant) -> String {
    let mut focused = frame(app, now).into_iter().filter(|row| row.contains('›'));
    let row = focused.next().expect("a focused row");
    assert_eq!(focused.next(), None);
    row
}

fn press(app: &mut App, code: KeyCode, now: Instant) -> Vec<Effect> {
    app.key(KeyEvent::new(code, KeyModifiers::NONE), now)
}

fn ctrl_c() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
}

fn paths(origin: &Origin) -> Paths {
    let throughput = ThroughputPath {
        origin: origin.clone(),
        transport: ThroughputTransport::FetchStream,
        protocol: Protocol::Http2,
    };
    Paths {
        throughput,
        control: Protocol::Http2,
        latency: None,
        stage_limit: SECOND * 300,
        idle_rtt: Duration::ZERO,
    }
}

/// A prepared server named after its ID.
fn prepared_server(name: &str, origin: &Origin) -> ServerPath {
    ServerPath {
        id: id(name),
        name: name.to_uppercase(),
        location: String::new(),
        origin: origin.clone(),
        offered: None,
        path: Ok(paths(origin)),
    }
}

/// A catalogue of five servers of which the check prepared the first.
fn prepared() -> Event {
    let origin = |name: &str| Origin::parse(&format!("https://{name}.example")).unwrap();
    let entry = |name: &str| ServerEntry {
        id: id(name),
        url: BaseUrl::Origin(origin(name)),
        name: name.to_uppercase(),
        location: String::new(),
        additional_origins: Vec::new(),
    };
    let catalogue = ["a", "b", "c", "d", "e"].map(entry);
    let servers = Arc::new([prepared_server("a", &origin("a"))]);
    Event::Prepared { servers, catalogue: Arc::new(catalogue) }
}

fn prompt(now: Instant) -> SignInPrompt {
    SignInPrompt {
        issuer: "Meter".into(),
        url: PAGE.into(),
        code: "ABCD-EFGH".into(),
        deadline: (now + Duration::from_secs(120)).into(),
    }
}

/// A run of `plan` over `names`, started from a row below Start test and prepared at `now`.
fn running(names: &[&str], plan: &[(Stage, Duration)], now: Instant) -> App {
    let stages = plan.iter().map(|(stage, _)| *stage).collect();
    let mut app = App::new(Config { stages, ..config() }, Profile::Plain, now);
    press(&mut app, KeyCode::Down, now);
    assert!(matches!(command(&press(&mut app, KeyCode::Char('r'), now)), Command::Run(_)));
    let origin = Origin::parse("https://meter.example").unwrap();
    let servers = names.iter().map(|name| prepared_server(name, &origin)).collect();
    app.event(&Event::Prepared { servers, catalogue: Arc::new([]) }, now);
    app.event(&Event::RunStarted { plan: plan.to_vec(), focus: id(names[0]), at: now }, now);
    app
}

fn ended(outcome: Outcome) -> Event {
    Event::RunFinished { outcome, error: None, elapsed: SECOND * 12 }
}

#[test]
fn the_chooser_keeps_up_to_four_servers_and_none_takes_the_default_ones() {
    let (mut app, now) = app();
    app.event(&prepared(), now);
    press(&mut app, KeyCode::Char('s'), now);
    assert!(shows(&mut app, now, "Test servers · 1 selected"));
    press(&mut app, KeyCode::Char(' '), now);
    assert!(shows(&mut app, now, "Test servers · 0 selected"));
    press(&mut app, KeyCode::Enter, now);
    assert!(checked(app.tick(now + Duration::from_millis(350))).servers.is_empty());

    let (mut app, now) = self::app();
    app.event(&prepared(), now);
    press(&mut app, KeyCode::Char('s'), now);
    for _ in 0..4 {
        press(&mut app, KeyCode::Down, now);
        press(&mut app, KeyCode::Char(' '), now);
    }
    assert!(shows(&mut app, now, "Test servers · 4 selected"));
    assert!(shows(&mut app, now, "At most four servers share one test."));
    press(&mut app, KeyCode::Enter, now);
    assert!(shows(&mut app, now, "Checking the selected servers…"));
    let config = checked(app.tick(now + Duration::from_millis(350)));
    let ids: Vec<_> = config.servers.iter().map(ServerId::as_str).collect();
    assert_eq!(ids, ["a", "b", "c", "d"]);
}

#[test]
fn checked_paths_turn_stale_on_screen_30_s_later_without_input() {
    let (mut app, now) = app();
    app.event(&prepared(), now);
    assert!(shows(&mut app, now, "A Ready"));
    let fresh = now + Duration::from_secs(30);
    assert_eq!(app.tick(fresh), []);
    assert!(!app.animating(), "nothing changes while the paths are fresh");
    let stale = fresh + Duration::from_millis(33);
    assert_eq!(app.tick(stale), []);
    assert!(app.animating(), "the frame that shows them stale is drawn");
    assert!(shows(&mut app, stale, "A Recheck needed"));
    assert!(!app.animating());
}

#[test]
fn a_cancelled_or_expired_sign_in_asks_to_sign_in_before_a_test() {
    for (end, notice) in [
        (SignInEnd::Cancelled, "Sign-in canceled. Press v to request a new code."),
        (SignInEnd::Expired, "Sign-in expired. Press v to request a new code."),
    ] {
        let (mut app, now) = app();
        app.event(&Event::Checking { run: false }, now);
        app.event(&Event::SignIn(prompt(now)), now);
        assert!(frame(&mut app, now)[0].ends_with(" Sign in"));
        press(&mut app, KeyCode::Enter, now);
        assert!(frame(&mut app, now)[0].ends_with(" Checking sign-in"));
        if end == SignInEnd::Cancelled {
            assert_eq!(command(&press(&mut app, KeyCode::Esc, now)), &Command::Stop);
        }
        app.event(&Event::SignInEnded(end), now);
        app.event(&Event::CheckFailed(Unapproved::Expired.failure()), now);
        assert!(shows(&mut app, now, notice) && !shows(&mut app, now, "Match this code"), "{end:?}");
        assert!(frame(&mut app, now)[0].ends_with(" Sign in"), "{end:?}");
        assert!(focused(&mut app, now).contains("› ▶ Start test enter sign in first; v requests"));
        assert_eq!(press(&mut app, KeyCode::Char('r'), now), []);
        assert!(shows(&mut app, now, "Test cannot start: sign in first. Press v to request a new code."));
        press(&mut app, KeyCode::Char('v'), now);
        assert_eq!(checked(app.tick(now + Duration::from_millis(350))), config());
    }
}

#[test]
fn quitting_during_a_run_stops_it_first_and_ends_with_the_signal_s_status() {
    let (now, download) = (Instant::now(), [(Stage::Download, SECOND * 10)]);
    let mut app = running(&["a"], &download, now);
    press(&mut app, KeyCode::Esc, now);
    assert_eq!(command(&press(&mut app, KeyCode::Char('q'), now)), &Command::Stop);
    assert!(shows(&mut app, now, "Stopping the test before quitting… ctrl+c quits at once."));
    assert_eq!(press(&mut app, KeyCode::Char('q'), now), []);
    assert_eq!(app.event(&ended(Outcome::Stopped), now), [Effect::Quit]);
    let exit = app.exit();
    assert_eq!((exit.report, status(&exit.view, exit.signal)), (true, 1));
    let lines = report(&exit.view, 100, &exit.palette);
    assert!(lines[0].text().starts_with("Graphite Meter  Stopped"), "{:?}", lines[0]);
    // A second interrupt quits at once without a report; an ended run's report follows, its status the signal's.
    for (key, code) in [(true, INTERRUPTED), (false, TERMINATED)] {
        for outcome in [None, Some(Outcome::Partial), Some(Outcome::Complete)] {
            let mut app = running(&["a"], &download, now);
            let interrupt = |app: &mut App| if key { app.key(ctrl_c(), now) } else { app.signal(code) };
            assert_eq!(command(&interrupt(&mut app)), &Command::Stop);
            let quit = match outcome {
                None => interrupt(&mut app),
                Some(outcome) => app.event(&ended(outcome), now),
            };
            assert_eq!(quit, [Effect::Quit]);
            let exit = app.exit();
            assert_eq!((exit.report, status(&exit.view, exit.signal)), (outcome.is_some(), code), "{outcome:?}");
        }
    }
}

#[test]
fn quitting_after_a_run_reports_it_with_a_signal_s_status_only_when_it_stopped() {
    let now = Instant::now();
    for (signal, outcome, expected) in [
        (Some(TERMINATED), Outcome::Stopped, TERMINATED),
        (Some(INTERRUPTED), Outcome::Stopped, INTERRUPTED),
        (None, Outcome::Stopped, 1),
        (Some(TERMINATED), Outcome::Complete, 0),
        (None, Outcome::Complete, 0),
    ] {
        let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
        app.event(&ended(outcome), now);
        let effects = match signal {
            Some(code) => app.signal(code),
            None => app.key(ctrl_c(), now),
        };
        assert_eq!(effects, [Effect::Quit]);
        let exit = app.exit();
        let quit = (exit.report, status(&exit.view, exit.signal));
        assert_eq!(quit, (true, expected), "{signal:?} after {outcome:?}");
    }
    let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
    app.event(&ended(Outcome::Complete), now);
    press(&mut app, KeyCode::Esc, now);
    assert_eq!(app.key(ctrl_c(), now), [Effect::Quit]);
    assert!(!app.exit().report, "setup shows no run to report");
}

#[test]
fn a_first_start_asks_for_the_server_and_keeps_the_one_that_prepared() {
    let now = Instant::now();
    let mut app = App::new(Config::default(), Profile::Plain, now);
    assert_eq!(app.tick(now), [], "nothing is checked without a server");
    assert!(focused(&mut app, now).contains("› Server address type the server's address"));
    assert!(shows(&mut app, now, "Enter your Graphite Meter server's address, then press enter."));
    press(&mut app, KeyCode::Esc, now);
    assert!(frame(&mut app, now)[0].ends_with("● Not started"));
    assert!(shows(&mut app, now, "▶ Start test r needs a server") && !shows(&mut app, now, "Checking paths"));
    assert!(shows(&mut app, now, "Its test servers and the paths to them show"));
    assert_eq!(press(&mut app, KeyCode::Char('r'), now), [], "a start without a server asks for one");
    assert!(shows(&mut app, now, "Enter your Graphite Meter server's address, then press enter."));
    app.input(Input::Paste("meter.example/history\n".into()), now);
    press(&mut app, KeyCode::Enter, now);
    assert!(
        focused(&mut app, now).contains("› ▶ Start test enter"),
        "a first server moves the focus to Start"
    );
    assert_eq!(checked(app.tick(now + Duration::from_millis(350))), config());
    let kept = Origin::parse("https://meter.example").unwrap();
    assert_eq!(
        app.event(&prepared(), now),
        [Effect::Remember(kept.clone())],
        "a prepared server is kept once"
    );
    assert_eq!(app.event(&prepared(), now), []);
    let scratch = graphite_meter_testkit::Scratch::new().unwrap();
    let path = scratch.path().join("graphite-meter").join("server");
    assert_eq!(config::recall(&path), None);
    config::remember(&path, &kept);
    assert_eq!(config::recall(&path), Some(kept));
    std::fs::write(&path, "not a server\n").unwrap();
    assert_eq!(config::recall(&path), None, "a kept address that no longer parses is ignored");
}

#[test]
fn the_console_follows_a_download_and_its_key_takes_clicks() {
    let now = Instant::now();
    let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
    let plan = StagePlan {
        stage: Stage::Download,
        members: Vec::new(),
        duration: SECOND * 10,
        latency: None,
    };
    app.event(&Event::StageStarted(plan), now);
    app.event(&Event::Measuring(Stage::Download), now);
    for tick in 1..=20 {
        let rates = Dir { down: Some(12.5e6), up: None };
        app.event(
            &Event::Sample { at: Duration::from_millis(100 * tick), rates },
            now + Duration::from_millis(100 * tick),
        );
    }
    let later = now + SECOND * 3;
    assert!(shows(&mut app, later, "↓ Download"), "{:#?}", frame(&mut app, later));
    assert!(shows(&mut app, later, "100.0 Mbit/s") && shows(&mut app, later, "measuring"));
    let rate = Rate { mean: 12.5e6, peak: 13e6 };
    let throughput = Dir {
        down: Some(Throughput { rate: Some(rate), bytes: 125_000_000 }),
        up: None,
    };
    let result = StageResult {
        stage: Stage::Download,
        measured: SECOND * 10,
        stopped: false,
        throughput,
        servers: Vec::new(),
        failures: Vec::new(),
        intervals: Vec::new(),
        omitted: 0,
    };
    app.event(&Event::StageFinished(result), later);
    app.event(&ended(Outcome::Complete), later);
    for fact in [
        "Peak 104.0 Mbit/s",
        "Transferred 125.0 MB",
        "Duration 10.0 s",
        "Run again about 11 s enter",
    ] {
        assert!(shows(&mut app, later, fact), "{fact}: {:#?}", frame(&mut app, later));
    }

    // A click on the key's plate presses it, in a profile that paints one.
    let mut app = App::new(config(), Profile::TrueColor, now);
    app.event(&prepared(), now);
    let click = |column, row| {
        let kind = MouseEventKind::Down(MouseButton::Left);
        Input::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE })
    };
    let shown = rows(&mut app, now);
    let row = shown.iter().position(|row| row.contains("▶ Start test")).unwrap();
    assert_eq!(app.input(click(1, row as u16), now), [], "the marker beside the key is not the key");
    assert!(
        matches!(command(&app.input(click(10, row as u16 - 1), now)), Command::Run(_)),
        "its cap is"
    );

    // In 256 colours the selected row and the dimmed key share an index; only the key presses.
    let mut app = App::new(config(), Profile::Ansi256, now);
    app.event(&prepared(), now);
    press(&mut app, KeyCode::Down, now);
    let shown = rows(&mut app, now);
    let selected = shown.iter().position(|row| row.contains("Server address")).unwrap();
    assert_eq!(app.input(click(10, selected as u16), now), [], "the selected row is not the key");
    let key = shown.iter().position(|row| row.contains("▶ Start test")).unwrap();
    assert!(matches!(command(&app.input(click(10, key as u16), now)), Command::Run(_)));
}
