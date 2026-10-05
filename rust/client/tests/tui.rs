//! The interface without a terminal: keys, events, signals and ticks in; frames, chrome, effects and exits out.
use crossterm::event::{Event as Input, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use graphite_meter_client::{
    INTERRUPTED, TERMINATED,
    config::Config,
    controller::Command,
    events::{Event, SignInEnd, SignInPrompt},
    measure::{
        aggregate::Rate,
        latency::{Population, Summary},
    },
    model::{Dir, Outcome, ServerResult, Stage, StageResult, Throughput},
    net::{ThroughputPath, approval::Unapproved},
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
    catalog::{ServerEntry, ServerId},
    discovery::{Protocol, ThroughputTransport},
    origin::{BaseUrl, Origin},
};
use ratatui_core::{buffer::Buffer, layout::Rect, style::Color};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

const FULL: Rect = Rect::new(0, 0, 120, 40);
const SECOND: Duration = Duration::from_secs(1);
const PAGE: &str = "https://meter.example/auth/cli?challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn id(name: &str) -> ServerId {
    ServerId::parse(name).unwrap()
}

/// Setup for the default settings, its first check already asked for.
fn app() -> (App, Instant) {
    let start = Instant::now();
    let mut app = App::new(Config::default(), Profile::Plain, start);
    assert_eq!(checked(app.tick(start)), Config::default());
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

/// The text of `row` from `column` on, as drawn.
fn drawn(buffer: &Buffer, row: u16, column: u16) -> String {
    let text: String = (column..buffer.area.width).map(|x| buffer[(x, row)].symbol()).collect();
    text.trim_end().to_owned()
}

fn press(app: &mut App, code: KeyCode, now: Instant) -> Vec<Effect> {
    app.key(KeyEvent::new(code, KeyModifiers::NONE), now)
}

fn presses(app: &mut App, code: KeyCode, times: usize, now: Instant) {
    for _ in 0..times {
        assert_eq!(press(app, code, now), []);
    }
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

/// The sign-in screen for `PAGE`, which expires two minutes after `now`.
fn signing_in(now: Instant) -> App {
    let mut app = App::new(Config::default(), Profile::Plain, now);
    app.event(&Event::SignIn(prompt(now)), now);
    app
}

/// A run of `plan` over `names`, started from a row below Start test and prepared at `now`.
fn running(names: &[&str], plan: &[(Stage, Duration)], now: Instant) -> App {
    let stages = plan.iter().map(|(stage, _)| *stage).collect();
    let mut app = App::new(Config { stages, ..Config::default() }, Profile::Plain, now);
    press(&mut app, KeyCode::Down, now);
    assert!(matches!(command(&press(&mut app, KeyCode::Char('r'), now)), Command::Run(_)));
    let origin = Origin::parse("https://meter.example").unwrap();
    let servers = names.iter().map(|name| prepared_server(name, &origin)).collect();
    app.event(&Event::Prepared { servers, catalogue: Arc::new([]) }, now);
    app.event(&Event::RunStarted { plan: plan.to_vec(), focus: id(names[0]), at: now }, now);
    app
}

/// Opens `stage`'s window at `now`.
fn measuring(app: &mut App, stage: Stage, duration: Duration, now: Instant) {
    let plan = StagePlan { stage, members: Vec::new(), duration, latency: None };
    app.event(&Event::StageStarted(plan), now);
    app.event(&Event::Measuring(stage), now);
}

fn sample(app: &mut App, at: Duration, down: Option<f64>, up: Option<f64>, now: Instant) {
    app.event(&Event::Sample { at, rates: Dir { down, up }, recovering: false }, now);
}

fn finished(stage: Stage, servers: Vec<ServerResult>, down: Option<f64>) -> Event {
    let down = down.map(|mean| Throughput { rate: Some(Rate { mean, peak: mean }), bytes: 1 });
    Event::StageFinished(StageResult {
        stage,
        measured: SECOND * 10,
        stopped: false,
        throughput: Dir { down, up: None },
        servers,
        failures: Vec::new(),
        intervals: Vec::new(),
        omitted: 0,
    })
}

fn ended(outcome: Outcome) -> Event {
    Event::RunFinished { outcome, error: None, elapsed: SECOND * 12 }
}

/// `name`'s share of a stage, with an idle median of `ms` when given.
fn share(name: &str, ms: Option<u64>) -> ServerResult {
    let latency = ms.map(|ms| {
        let summary = Summary {
            replies: 20,
            p50: Some(Duration::from_millis(ms)),
            ..Summary::default()
        };
        Population { summary, complete: true }
    });
    ServerResult {
        server: id(name),
        left: false,
        throughput: Dir::default(),
        latency,
    }
}

#[test]
fn setup_starts_on_start_test_and_arrows_and_space_change_its_values() {
    let start = Instant::now();
    let mut app = App::new(Config::default(), Profile::Plain, start);
    let rows = frame(&mut app, start);
    assert!(rows[0].contains("Graphite Meter") && rows[0].ends_with("Checking paths"), "{rows:#?}");
    assert!(focused(&mut app, start).contains("› Start test ⠋ checking paths"));
    let footer = "enter start test • ↑/↓ move • ? keys • q quit";
    assert!(rows.iter().any(|row| row.contains(footer)), "{rows:#?}");
    assert_eq!(checked(app.tick(start)), Config::default());
    let now = start;
    press(&mut app, KeyCode::Down, now);
    assert!(focused(&mut app, now).contains("Catalogue URL http://127.0.0.1:7246"));
    presses(&mut app, KeyCode::Down, 2, now);
    assert!(focused(&mut app, now).contains("Throughput path Automatic · each server"));
    press(&mut app, KeyCode::Right, now);
    assert!(focused(&mut app, now).contains("Throughput path Fetch streams · every server"));
    presses(&mut app, KeyCode::Down, 3, now);
    assert!(focused(&mut app, now).contains("Latency ● 4 s"));
    press(&mut app, KeyCode::Right, now);
    presses(&mut app, KeyCode::Left, 2, now);
    assert!(focused(&mut app, now).contains("Latency ● 3 s"));
    assert!(shows(&mut app, now, "Latency 3 s."));
    press(&mut app, KeyCode::Down, now);
    press(&mut app, KeyCode::Char(' '), now);
    assert!(focused(&mut app, now).contains("Download ○ 10 s"));
    assert!(shows(&mut app, now, "Download off."));
}

#[test]
fn the_editor_types_question_marks_and_q_caps_its_text_and_esc_cancels() {
    let (mut app, now) = app();
    press(&mut app, KeyCode::Down, now);
    press(&mut app, KeyCode::Enter, now);
    for c in "?q".chars() {
        assert_eq!(press(&mut app, KeyCode::Char(c), now), []);
    }
    assert!(focused(&mut app, now).contains("http://127.0.0.1:7246?q"));
    press(&mut app, KeyCode::Esc, now);
    assert!(shows(&mut app, now, "Edit canceled."));
    assert!(focused(&mut app, now).contains("Catalogue URL http://127.0.0.1:7246"));
    assert!(!shows(&mut app, now, "7246?q"));

    press(&mut app, KeyCode::Enter, now);
    presses(&mut app, KeyCode::Char('a'), 4096 - "http://127.0.0.1:7246".len() - 1, now);
    press(&mut app, KeyCode::Char('Y'), now);
    press(&mut app, KeyCode::Char('Z'), now);
    let row = focused(&mut app, now);
    assert!(row.contains("aaY") && !row.contains('Z'), "{row}");
    press(&mut app, KeyCode::Esc, now);

    press(&mut app, KeyCode::Enter, now);
    presses(&mut app, KeyCode::Backspace, 21, now);
    for c in "meter.example".chars() {
        press(&mut app, KeyCode::Char(c), now);
    }
    press(&mut app, KeyCode::Enter, now);
    assert!(shows(&mut app, now, "Catalogue https://meter.example."));
    let config = checked(app.tick(now + Duration::from_millis(350)));
    assert_eq!(config.url.to_string(), "https://meter.example");
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
fn question_mark_lists_the_screen_s_keys_and_s_explains_a_one_server_catalogue() {
    let (mut app, now) = app();
    app.event(&prepared(), now);
    assert!(!shows(&mut app, now, "recheck paths"));
    press(&mut app, KeyCode::Char('?'), now);
    for key in [
        "r start test",
        "space on/off",
        "enter start or open",
        "v recheck paths",
        "a automatic paths",
    ] {
        assert!(shows(&mut app, now, key), "{key}");
    }
    assert!(shows(&mut app, now, "s test servers"));
    press(&mut app, KeyCode::Char('s'), now);
    for key in ["space select", "enter apply", "esc cancel"] {
        assert!(shows(&mut app, now, key), "{key}");
    }
    assert!(!shows(&mut app, now, "recheck paths"));

    let (mut app, now) = self::app();
    let Event::Prepared { servers, catalogue } = prepared() else { unreachable!() };
    app.event(&Event::Prepared { servers, catalogue: catalogue[..1].into() }, now);
    assert_eq!(press(&mut app, KeyCode::Char('s'), now), []);
    assert!(shows(&mut app, now, "This catalogue offers one server."));
    press(&mut app, KeyCode::Char('?'), now);
    assert!(!shows(&mut app, now, "s test servers"), "help leaves it out");
}

#[test]
fn a_path_change_checks_once_350_ms_after_the_last_change() {
    let (mut app, start) = app();
    let at = |ms| start + Duration::from_millis(ms);
    presses(&mut app, KeyCode::Down, 6, at(0));
    press(&mut app, KeyCode::Right, at(0));
    assert_eq!(app.tick(at(400)), [], "a stage's duration is no path setting");
    presses(&mut app, KeyCode::Up, 3, at(400));
    press(&mut app, KeyCode::Right, at(400));
    press(&mut app, KeyCode::Right, at(600));
    assert_eq!(app.tick(at(949)), []);
    let config = checked(app.tick(at(950)));
    assert_eq!(config.paths.throughput_transport, Some(ThroughputTransport::WebTransport));
    assert_eq!(app.tick(at(1300)), []);
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
fn sign_in_shows_the_code_and_links_the_page_where_it_is_drawn() {
    let start = Instant::now();
    let mut app = signing_in(start);
    let later = start + Duration::from_millis(3200);
    for text in [
        "Sign in to Meter",
        "Open the sign-in page below",
        "Match this code │ ABCD-EFGH │",
        "waited 3.2 s · expires in 1 min 57 s",
        "Check the code, then press enter to open the sign-in page.",
        "enter/space open page • esc cancel • q quit",
    ] {
        assert!(shows(&mut app, later, text), "{text}\n{:#?}", frame(&mut app, later));
    }
    let (buffer, chrome) = draw(&mut app, later, FULL);
    let [link] = &chrome.links[..] else { panic!("{:?}", chrome.links) };
    assert_eq!((link.url.as_str(), link.text.text()), (PAGE, PAGE.to_owned()));
    assert_eq!(drawn(&buffer, link.row, link.column), PAGE);

    let (mut app, small) = (signing_in(start), Rect::new(0, 0, 40, 12));
    let wheel = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    };
    app.input(Input::Mouse(wheel), start);
    assert_eq!(draw(&mut app, start, small).1.links, [], "the wheel leaves the link below the viewport");
    press(&mut app, KeyCode::PageDown, start);
    let (buffer, chrome) = draw(&mut app, start, small);
    let texts: Vec<_> = chrome.links.iter().map(|link| link.text.text()).collect();
    assert_eq!(texts.concat(), PAGE);
    assert_eq!(chrome.links.iter().map(|link| link.row).collect::<Vec<_>>(), [7, 8, 9]);
    for link in &chrome.links {
        assert_eq!(drawn(&buffer, link.row, link.column), link.text.text());
    }
}

#[test]
fn enter_space_and_o_open_the_sign_in_page() {
    let now = Instant::now();
    for code in [KeyCode::Enter, KeyCode::Char(' '), KeyCode::Char('o')] {
        let mut app = signing_in(now);
        assert_eq!(press(&mut app, code, now), [Effect::OpenBrowser(PAGE.into())], "{code:?}");
        assert!(shows(&mut app, now, "⠋ Waiting for approval…"));
        assert!(shows(&mut app, now, "Sign-in page opened in the browser."));
    }
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
        assert!(focused(&mut app, now).contains("› Start test sign in first; v requests a new code"));
        assert_eq!(press(&mut app, KeyCode::Char('r'), now), []);
        assert!(shows(&mut app, now, "Test cannot start: sign in first. Press v to request a new code."));
        press(&mut app, KeyCode::Char('v'), now);
        assert_eq!(checked(app.tick(now + Duration::from_millis(350))), Config::default());
    }
}

#[test]
fn a_run_shows_its_stages_live_readings_details_and_then_its_results() {
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
    press(&mut app, KeyCode::Char('d'), later);
    assert!(shows(&mut app, later, "Latency median by server"));
    assert!(shows(&mut app, later, "↑/↓ scroll • esc close • q quit"));
    press(&mut app, KeyCode::Esc, later);
    assert!(!shows(&mut app, later, "Latency median by server"));
    assert!(shows(&mut app, later, "esc stop test • d details"));

    app.event(&finished(Stage::Download, vec![share("a", None)], Some(1.25e7)), later);
    app.event(&finished(Stage::Upload, vec![share("a", None)], None), later);
    app.event(&ended(Outcome::Complete), later);
    for text in ["Complete", "Results", "Throughput", "Download ↓ 100.0 Mbit/s", "Timeline"] {
        assert!(shows(&mut app, later, text), "{text}\n{:#?}", rows(&mut app, later));
    }
    assert!(shows(&mut app, later, "enter run again • esc setup • d details"));
    press(&mut app, KeyCode::Esc, later);
    assert!(shows(&mut app, later, "› Start test"), "esc returns to Start test");
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
    app.event(&finished(Stage::Latency, vec![share("a", Some(10)), share("b", Some(30))], None), now);
    app.event(&ended(Outcome::Complete), now);
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
    let (_, setup) = draw(&mut app, start, FULL);
    assert_eq!(setup.progress, Progress::None);
    let bytes = setup.bytes(None, Profile::Plain);
    assert_eq!(bytes, "\x1b]2;Graphite Meter · Checking paths\x07\x1b]9;4;0\x07".as_bytes());

    press(&mut app, KeyCode::Char('r'), start);
    let (_, preparing) = draw(&mut app, start, FULL);
    assert_eq!(preparing.bytes(Some(&setup), Profile::Plain), b"\x1b]9;4;3\x07");

    let plan = vec![(Stage::Download, SECOND * 10)];
    app.event(&Event::RunStarted { plan, focus: id("a"), at: start }, start);
    measuring(&mut app, Stage::Download, SECOND * 10, start);
    let (_, halfway) = draw(&mut app, start + SECOND * 5, FULL);
    let bytes = halfway.bytes(Some(&preparing), Profile::Plain);
    assert_eq!(bytes, "\x1b]2;Graphite Meter · Download\x07\x1b]9;4;1;50\x07".as_bytes());
    assert_eq!(halfway.bytes(Some(&halfway), Profile::Plain), b"");

    app.event(&ended(Outcome::Complete), start + SECOND * 10);
    let (_, finished) = draw(&mut app, start + SECOND * 10, FULL);
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
    let title = |app: &mut App| draw(app, now, FULL).0[(1, 0)].bg;
    let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
    let mut app = App::new(Config::default(), Profile::Ansi, now);
    assert_eq!(title(&mut app), Color::Indexed(15));
    assert_eq!(app.key(alt(']'), now), []);
    for c in "11;rgb:FDFD/f6f6/e3e3".chars() {
        assert_eq!(press(&mut app, KeyCode::Char(c), now), [], "{c}");
    }
    assert_eq!(app.key(alt('\\'), now), []);
    assert_eq!(title(&mut app), Color::Indexed(0));
    assert!(matches!(command(&press(&mut app, KeyCode::Char('r'), now)), Command::Run(_)));

    let mut split = App::new(Config::default(), Profile::Ansi, now);
    for c in "\x1b]11;rgb:FDFD/f6f6/e3e3".chars() {
        let code = if c == '\x1b' { KeyCode::Esc } else { KeyCode::Char(c) };
        assert_eq!(press(&mut split, code, now), [], "an answer split after its escape starts no test: {c}");
    }
    assert_eq!(split.key(alt('\\'), now), []);
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
