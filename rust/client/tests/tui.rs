//! The interface without a terminal: keys, pointer, events, signals and ticks in; frames, chrome, effects and exits out.
use crossterm::event::{Event as Input, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use graphite_meter_client::{
    INTERRUPTED, TERMINATED,
    config::{self, Config},
    controller::Command,
    events::{Event, SignInEnd, SignInPrompt},
    measure::aggregate::Rate,
    model::{Dir, Failure, Outcome, Stage, StageResult, Throughput},
    net::{ThroughputPath, approval::Unapproved},
    report::report,
    run::{
        engine::StagePlan,
        prepare::{Paths, ServerPath},
    },
    status,
    text::Profile,
    tui::{App, Effect},
};
use graphite_meter_proto::{
    catalog::{ServerEntry, ServerId},
    discovery::{IpFamily, Protocol, ThroughputTransport},
    origin::{BaseUrl, Origin},
    reason::FailureReason,
};
use ratatui_core::{buffer::Buffer, layout::Rect};
use std::{
    assert_matches,
    sync::Arc,
    time::{Duration, Instant},
};

const FULL: Rect = Rect::new(0, 0, 160, 44);
const SECOND: Duration = Duration::from_secs(1);
const REST: Duration = Duration::from_millis(350);
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

/// The console for the default settings, its first check already asked for.
fn app() -> (App, Instant) {
    let start = Instant::now();
    let mut app = App::new(config(), None, Profile::Plain, start);
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

/// The frame's rows as drawn.
fn rows(app: &mut App, now: Instant) -> Vec<String> {
    let mut buffer = Buffer::empty(FULL);
    app.draw(&mut buffer, now);
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

/// Where `text` first stands in the frame, as a column and row.
fn find(app: &mut App, now: Instant, text: &str) -> (u16, u16) {
    let rows = rows(app, now);
    let found = rows.iter().enumerate().find_map(|(row, line)| {
        let at = line.find(text)?;
        Some((line[..at].chars().count() as u16, row as u16))
    });
    found.unwrap_or_else(|| panic!("{text:?} is not on screen: {rows:#?}"))
}

fn press(app: &mut App, code: KeyCode, now: Instant) -> Vec<Effect> {
    app.key(KeyEvent::new(code, KeyModifiers::NONE), now)
}

fn pointer(kind: MouseEventKind, (column, row): (u16, u16)) -> Input {
    Input::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE })
}

fn click(app: &mut App, now: Instant, at: (u16, u16)) -> Vec<Effect> {
    app.input(pointer(MouseEventKind::Down(MouseButton::Left), at), now)
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
        idle_rtt: None,
        ip_family: IpFamily::V4,
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

/// A run of `plan` over `names`, started with enter and prepared at `now`.
fn running(names: &[&str], plan: &[(Stage, Duration)], now: Instant) -> App {
    let stages = plan.iter().map(|(stage, _)| *stage).collect();
    let mut app = App::new(Config { stages, ..config() }, None, Profile::Plain, now);
    assert_matches!(command(&press(&mut app, KeyCode::Enter, now)), Command::Run(_));
    let origin = Origin::parse("https://meter.example").unwrap();
    let servers = names.iter().map(|name| prepared_server(name, &origin)).collect();
    app.event(&Event::Prepared { servers, catalogue: Arc::new([]) }, now);
    app.event(&Event::RunStarted { plan: plan.to_vec(), focus: id(names[0]), at: now }, now);
    app
}

/// `stage` started and measuring at `now`.
fn measuring(app: &mut App, stage: Stage, duration: Duration, now: Instant) {
    let plan = StagePlan { stage, members: Vec::new(), duration, latency: None };
    app.event(&Event::StageStarted(plan), now);
    app.event(&Event::Measuring(stage), now);
}

fn ended(outcome: Outcome) -> Event {
    Event::RunFinished { outcome, error: None, elapsed: SECOND * 12 }
}

#[test]
fn the_chooser_keeps_up_to_four_servers_and_none_takes_the_default_ones() {
    let (mut app, now) = app();
    app.event(&prepared(), now);
    for code in [KeyCode::Char('s'), KeyCode::Down, KeyCode::Down, KeyCode::Enter] {
        press(&mut app, code, now);
    }
    assert!(shows(&mut app, now, "Test servers · 1 selected"));
    press(&mut app, KeyCode::Char(' '), now);
    assert!(shows(&mut app, now, "Test servers · 0 selected"));
    press(&mut app, KeyCode::Enter, now);
    assert!(shows(&mut app, now, "Settings"), "applying returns to the settings");
    assert!(checked(app.tick(now + REST)).servers.is_empty());

    let (mut app, now) = self::app();
    app.event(&prepared(), now);
    for code in [KeyCode::Char('s'), KeyCode::Down, KeyCode::Down, KeyCode::Enter] {
        press(&mut app, code, now);
    }
    for _ in 0..4 {
        press(&mut app, KeyCode::Down, now);
        press(&mut app, KeyCode::Char(' '), now);
    }
    assert!(shows(&mut app, now, "Test servers · 4 selected"));
    assert!(shows(&mut app, now, "At most four servers share one test."));
    press(&mut app, KeyCode::Enter, now);
    let config = checked(app.tick(now + REST));
    let ids: Vec<_> = config.servers.iter().map(ServerId::as_str).collect();
    assert_eq!(ids, ["a", "b", "c", "d"]);
}

#[test]
fn a_server_that_cannot_be_reached_opens_its_address_and_enter_tries_again() {
    let (mut app, now) = app();
    let failure = Failure::new(FailureReason::PreparationFailed, graphite_meter_client::net::UNREACHABLE);
    app.event(&Event::Checking { run: false }, now);
    app.event(&Event::CheckFailed(failure), now);
    assert!(frame(&mut app, now)[0].ends_with("● Unreachable"), "{:?}", frame(&mut app, now)[0]);
    assert!(shows(&mut app, now, "Couldn't reach meter.example."));
    assert!(shows(
        &mut app,
        now,
        "Check the address and that the server is running; enter tries again."
    ));
    press(&mut app, KeyCode::Enter, now);
    assert_eq!(checked(app.tick(now)), config(), "enter checks again at once");
    app.event(&Event::Checking { run: false }, now);
    assert!(shows(&mut app, now, "Checking meter.example…"));
    app.event(&prepared(), now);
    assert!(!shows(&mut app, now, "Connect to a server"), "a server that prepared closes the address");
}

#[test]
fn a_cancelled_or_expired_sign_in_asks_again_on_the_next_start() {
    for (end, notice) in [(SignInEnd::Cancelled, "Sign-in canceled"), (SignInEnd::Expired, "Sign-in expired")] {
        let (mut app, now) = app();
        app.event(&Event::Checking { run: false }, now);
        app.event(&Event::SignIn(prompt(now)), now);
        assert!(shows(&mut app, now, "Sign in to Meter") && shows(&mut app, now, "ABCD-EFGH"));
        assert_eq!(press(&mut app, KeyCode::Enter, now), [Effect::OpenBrowser(PAGE.into())]);
        assert!(shows(&mut app, now, "Waiting for approval…"));
        if end == SignInEnd::Cancelled {
            assert_eq!(command(&press(&mut app, KeyCode::Esc, now)), &Command::Stop);
        }
        app.event(&Event::SignInEnded(end), now);
        app.event(&Event::CheckFailed(Unapproved::Expired.failure()), now);
        assert!(shows(&mut app, now, notice) && !shows(&mut app, now, "ABCD-EFGH"), "{end:?}");
        assert!(!shows(&mut app, now, "Connect to a server"), "a lapsed sign-in leaves the address alone");
        assert!(frame(&mut app, now)[0].ends_with("● Sign in"), "{end:?}");
        assert!(
            matches!(command(&press(&mut app, KeyCode::Char('r'), now)), Command::Run(_)),
            "a start signs in again"
        );
        let (mut app, now) = self::app();
        app.event(&Event::Checking { run: false }, now);
        app.event(&Event::SignIn(prompt(now)), now);
        app.event(&Event::SignInEnded(end), now);
        press(&mut app, KeyCode::Char('v'), now);
        assert_eq!(checked(app.tick(now + REST)), config(), "v checks again");
    }
}

#[test]
fn quitting_during_a_run_stops_it_first_and_ends_with_the_signal_s_status() {
    let (now, download) = (Instant::now(), [(Stage::Download, SECOND * 10)]);
    let mut app = running(&["a"], &download, now);
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
fn esc_stops_a_run_at_once_and_quitting_after_one_reports_it() {
    let now = Instant::now();
    let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
    assert_eq!(command(&press(&mut app, KeyCode::Esc, now)), &Command::Stop);
    assert_eq!(press(&mut app, KeyCode::Esc, now), [], "one stop is asked for once");
    assert!(frame(&mut app, now).last().unwrap().starts_with("Stopping"));
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
}

#[test]
fn a_first_start_asks_for_the_server_and_remembers_it_only_when_asked() {
    let now = Instant::now();
    let mut app = App::new(Config::default(), None, Profile::Plain, now);
    assert_eq!(app.tick(now), [], "nothing is checked without a server");
    assert!(shows(&mut app, now, "Connect to a server"));
    assert!(shows(&mut app, now, "Enter the address of your Graphite Meter server, or paste it."));
    assert!(
        shows(&mut app, now, "Remember this server ○ off tab"),
        "remembering waits to be asked for"
    );
    press(&mut app, KeyCode::Esc, now);
    assert!(frame(&mut app, now)[0].ends_with("○ No server"));
    assert!(shows(&mut app, now, "Start test needs a server"));
    assert!(shows(&mut app, now, "Add a server to measure your connection"));
    assert_eq!(press(&mut app, KeyCode::Char('r'), now), [], "a start without a server asks for one");
    assert!(shows(&mut app, now, "Connect to a server"));
    assert_eq!(press(&mut app, KeyCode::Tab, now), [], "nothing is kept before a server prepared");
    assert!(shows(&mut app, now, "Remember this server ● on"));
    app.input(Input::Paste("meter.example/history\n".into()), now);
    press(&mut app, KeyCode::Enter, now);
    assert_eq!(checked(app.tick(now)), config(), "the address is checked at once");
    app.event(&Event::Checking { run: false }, now);
    assert!(shows(&mut app, now, "Checking meter.example…"));
    let kept = Origin::parse("https://meter.example").unwrap();
    assert_eq!(
        app.event(&prepared(), now),
        [Effect::Remember(kept.clone())],
        "an asked-for server is kept once"
    );
    assert!(!shows(&mut app, now, "Connect to a server") && shows(&mut app, now, "Ready A"));
    assert_eq!(app.event(&prepared(), now), []);
    press(&mut app, KeyCode::Char('s'), now);
    press(&mut app, KeyCode::Down, now);
    assert!(shows(&mut app, now, "Remember server ● on"));
    assert_eq!(
        press(&mut app, KeyCode::Char(' '), now),
        [Effect::Forget],
        "turning it off deletes the file"
    );
    assert_eq!(app.event(&prepared(), now), [], "a forgotten server is not kept again");
    assert_eq!(
        press(&mut app, KeyCode::Right, now),
        [Effect::Remember(kept.clone())],
        "a ready server is kept at once"
    );
    let mut later = App::new(config(), Some(kept.clone()), Profile::Plain, now);
    later.tick(now);
    press(&mut later, KeyCode::Char('s'), now);
    assert!(shows(&mut later, now, "Remember server ● on"), "a kept server starts remembered");

    let scratch = graphite_meter_testkit::Scratch::new().unwrap();
    let path = scratch.path().join("graphite-meter").join("server");
    assert_eq!(config::recall(&path), None);
    config::remember(&path, &kept);
    assert_eq!(config::recall(&path), Some(kept));
    config::forget(&path);
    assert!(!path.exists(), "forgetting deletes the kept server");
    std::fs::write(&path, "not a server\n").unwrap();
    assert_eq!(config::recall(&path), None, "a kept address that no longer parses is ignored");

    // An address pasted over the console is the one to test.
    let (mut app, now) = self::app();
    app.input(Input::Paste("https://other.example".into()), now);
    press(&mut app, KeyCode::Enter, now);
    assert_eq!(checked(app.tick(now)).url, Origin::parse("https://other.example").ok());
}

#[test]
fn idle_latency_fills_its_lane_live_from_the_replies() {
    let now = Instant::now();
    let mut app = running(&["a"], &[(Stage::Latency, SECOND * 4)], now);
    measuring(&mut app, Stage::Latency, SECOND * 4, now);
    for ms in 10..=29 {
        let (at, rtt) = (Duration::from_millis(100 * (ms - 9)), Some(Duration::from_millis(ms)));
        app.event(&Event::Probe { server: id("a"), at, rtt }, now + at);
    }
    app.event(&Event::Probe { server: id("a"), at: SECOND * 3, rtt: None }, now + SECOND * 3);
    let later = now + SECOND * 3;
    let lane = frame(&mut app, later)
        .into_iter()
        .find(|row| row.contains("≈ Idle"))
        .unwrap();
    assert!(lane.contains("19.5 ms") && lane.contains("4.8%") && lane.contains("Baseline"), "{lane:?}");
    assert!(shows(&mut app, later, "Range 10.0–29.0 ms") && shows(&mut app, later, "Replies 20"));
    assert!(shows(&mut app, later, "1 probe unanswered"), "the dial notes the streak");
}

#[test]
fn the_console_follows_a_download_and_takes_presses() {
    let now = Instant::now();
    let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
    measuring(&mut app, Stage::Download, SECOND * 10, now);
    for tick in 1..=20 {
        let (at, rates) = (Duration::from_millis(100 * tick), Dir { down: Some(12.5e6), up: None });
        app.event(&Event::Sample { at, rates }, now + at);
    }
    let later = now + SECOND * 3;
    assert!(shows(&mut app, later, "↓ Download Running"), "{:#?}", frame(&mut app, later));
    assert!(shows(&mut app, later, "100.0 Mbit/s") && shows(&mut app, later, "Download 3.0"));
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
        "Run again ~11 s enter",
        "Complete",
    ] {
        assert!(shows(&mut app, later, fact), "{fact}: {:#?}", frame(&mut app, later));
    }

    // The key, a chip, a bar's cap and a sheet's backdrop each take a press.
    let key = find(&mut app, later, "Run again");
    assert_matches!(command(&click(&mut app, later, key)), Command::Run(_));
    let (mut app, now) = self::app();
    app.event(&prepared(), now);
    let chip = find(&mut app, now, "↑ Upload");
    assert_eq!(click(&mut app, now, chip), [], "a chip switches its stage");
    assert!(shows(&mut app, now, "↑ Upload Skipped"));
    assert_eq!(checked(app.tick(now + REST)).stages, [Stage::Latency, Stage::Download]);
    let cap = find(&mut app, now, "settings");
    click(&mut app, now, (cap.0 - 3, cap.1));
    assert!(shows(&mut app, now, "Settings esc close"));
    click(&mut app, now, (2, 20));
    assert!(!shows(&mut app, now, "Settings esc close"), "a press beside a sheet closes it");
    let moved = pointer(MouseEventKind::Moved, find(&mut app, now, "Start test"));
    app.input(moved, now);
    assert!(app.hovered().is_some(), "the key lights under the pointer");
}

/// A finished download of 125 MB over `a`.
fn downloaded(now: Instant) -> App {
    let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
    measuring(&mut app, Stage::Download, SECOND * 10, now);
    let throughput = Dir {
        down: Some(Throughput {
            rate: Some(Rate { mean: 12.5e6, peak: 13e6 }),
            bytes: 125_000_000,
        }),
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
    app.event(&Event::StageFinished(result), now);
    app.event(&ended(Outcome::Complete), now);
    app
}

#[test]
fn results_stay_until_the_next_run_starts_and_a_start_that_ends_first_keeps_them() {
    let now = Instant::now();
    let mut app = downloaded(now);
    assert_matches!(command(&press(&mut app, KeyCode::Enter, now)), Command::Run(_));
    app.event(&Event::Checking { run: true }, now);
    assert!(shows(&mut app, now, "Transferred 125.0 MB"), "the results stay while the next run checks");
    assert!(frame(&mut app, now).last().unwrap().starts_with("Checking paths"));
    assert_eq!(command(&press(&mut app, KeyCode::Esc, now)), &Command::Stop);
    app.event(&ended(Outcome::Stopped), now);
    assert!(shows(&mut app, now, "Transferred 125.0 MB") && shows(&mut app, now, "Test stopped before it started."));
    let exit = app.exit();
    assert_eq!((exit.report, status(&exit.view, None)), (true, 1), "the report is the shown run's");

    let mut app = downloaded(now);
    press(&mut app, KeyCode::Enter, now);
    app.event(&Event::Checking { run: true }, now);
    app.event(
        &Event::RunStarted {
            plan: vec![(Stage::Upload, SECOND * 10)],
            focus: id("a"),
            at: now,
        },
        now,
    );
    assert!(!shows(&mut app, now, "Transferred 125.0 MB"), "a started run replaces them");
}

#[test]
fn the_bidirectional_chip_follows_its_setting_after_a_run() {
    let now = Instant::now();
    let mut app = downloaded(now);
    assert!(!shows(&mut app, now, "↕ Bidire"));
    press(&mut app, KeyCode::Char('4'), now);
    assert!(shows(&mut app, now, "↕ Bidire"), "{:#?}", frame(&mut app, now));
    press(&mut app, KeyCode::Char('4'), now);
    assert!(!shows(&mut app, now, "↕ Bidire"));
}

#[test]
fn a_settings_change_during_a_run_is_checked_once_the_run_ends() {
    let now = Instant::now();
    let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
    press(&mut app, KeyCode::Char('s'), now);
    for _ in 0..10 {
        press(&mut app, KeyCode::Down, now);
    }
    assert!(shows(&mut app, now, "Round trips during transfers"), "{:#?}", frame(&mut app, now));
    press(&mut app, KeyCode::Char(' '), now);
    assert_eq!(app.tick(now + REST), [], "a check would replace the run");
    app.event(&ended(Outcome::Complete), now + REST);
    assert!(!checked(app.tick(now + REST)).loaded_latency);
}

#[test]
fn the_sign_in_card_stays_for_a_press_beside_it_and_ctrl_c_while_typing_stops_a_run_first() {
    let (mut app, now) = app();
    app.event(&Event::Checking { run: false }, now);
    app.event(&Event::SignIn(prompt(now)), now);
    click(&mut app, now, (2, 20));
    assert!(shows(&mut app, now, "Sign in to Meter"), "the sign-in still waits");

    let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
    press(&mut app, KeyCode::Char('s'), now);
    press(&mut app, KeyCode::Enter, now);
    assert_eq!(command(&app.key(ctrl_c(), now)), &Command::Stop);
}
