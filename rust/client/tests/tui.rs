//! The interface without a terminal: keys, pointer, events, signals and ticks in; frames, chrome, effects and exits out.
use crossterm::event::{
    Event as Input, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use graphite_meter_client::{
    INTERRUPTED, TERMINATED,
    config::{self, Config},
    controller::{Command, Kind},
    events::{Event, Probed, SignInEnd, SignInPrompt},
    measure::{
        aggregate::Rate,
        latency::{Latency, ProbeOutcome},
    },
    model::{
        Dir, Failure, Outcome, Scope, ServerFailure, ServerResult, Stage, StageResult, Throughput,
    },
    net::{ThroughputPath, approval::Unapproved},
    report::report,
    run::{
        engine::StagePlan,
        prepare::{Paths, ServerPath},
    },
    status,
    style::Palette,
    text::Profile,
    tui::{App, Effect},
};
use graphite_meter_proto::{
    catalog::{ServerEntry, ServerId},
    discovery::{IpFamily, Protocol, ThroughputTransport},
    origin::{BaseUrl, Origin},
    reason::FailureReason,
};
use ratatui_core::{
    buffer::{Buffer, Cell},
    layout::Rect,
};
use std::{
    assert_matches,
    sync::Arc,
    time::{Duration, Instant},
};

const FULL: Rect = Rect::new(0, 0, 160, 44);
const SECOND: Duration = Duration::from_secs(1);
const REST: Duration = Duration::from_millis(350);
const PAGE: &str =
    "https://meter.example/auth/cli?challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn id(name: &str) -> ServerId {
    ServerId::parse(name).unwrap()
}

fn config() -> Config {
    Config {
        url: Origin::parse("https://meter.example").ok(),
        ..Config::default()
    }
}

/// The console for the default settings, its first check already asked for.
fn app() -> (App, Instant) {
    let start = Instant::now();
    let mut app = App::new(config(), None, (Profile::Plain, Palette::new(true)), start);
    assert_eq!(checked(app.tick(start)), config());
    (app, start)
}

fn command(effects: &[Effect]) -> &Command {
    match effects {
        [Effect::Command(command)] => command,
        effects => panic!("{effects:?}"),
    }
}

fn checked(effects: Vec<Effect>) -> Config {
    match command(&effects) {
        Command::Check(config) => config.clone(),
        command => panic!("{command:?}"),
    }
}

fn rows(app: &mut App, now: Instant) -> Vec<String> {
    let mut buffer = Buffer::empty(FULL);
    app.draw(&mut buffer, now);
    let row = |cells: &[Cell]| cells.iter().map(Cell::symbol).collect::<String>();
    buffer
        .content()
        .chunks(usize::from(FULL.width))
        .map(row)
        .collect()
}

/// The frame's rows, each run of spaces as one.
fn frame(app: &mut App, now: Instant) -> Vec<String> {
    let squeezed = |row: String| row.split_whitespace().collect::<Vec<_>>().join(" ");
    rows(app, now).into_iter().map(squeezed).collect()
}

fn shows(app: &mut App, now: Instant, text: &str) -> bool {
    frame(app, now).iter().any(|row| row.contains(text))
}

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
    Input::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
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
        latency: None,
        stage_limit: SECOND * 300,
        idle_rtt: None,
        ip_family: IpFamily::V4,
    }
}

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

/// A catalog of five servers of which the check prepared the first.
fn prepared() -> Event {
    let origin = |name: &str| Origin::parse(&format!("https://{name}.example")).unwrap();
    let entry = |name: &str| ServerEntry {
        id: id(name),
        url: BaseUrl::Origin(origin(name)),
        name: name.to_uppercase(),
        location: String::new(),
        additional_origins: Vec::new(),
    };
    let catalog = ["a", "b", "c", "d", "e"].map(entry);
    let servers = Arc::new([prepared_server("a", &origin("a"))]);
    Event::Prepared {
        servers,
        catalog: Arc::new(catalog),
    }
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
    let mut app = App::new(
        Config { stages, ..config() },
        None,
        (Profile::Plain, Palette::new(true)),
        now,
    );
    assert_matches!(
        command(&press(&mut app, KeyCode::Enter, now)),
        Command::Run(_)
    );
    let origin = Origin::parse("https://meter.example").unwrap();
    let servers = names
        .iter()
        .map(|name| prepared_server(name, &origin))
        .collect();
    app.event(
        &Event::Prepared {
            servers,
            catalog: Arc::new([]),
        },
        now,
    );
    app.event(
        &Event::RunStarted {
            plan: plan.to_vec(),
            focus: id(names[0]),
            at: now,
        },
        now,
    );
    app
}

fn measuring(app: &mut App, stage: Stage, duration: Duration, now: Instant) {
    let plan = StagePlan {
        stage,
        members: Vec::new(),
        duration,
        latency: None,
    };
    app.event(&Event::StageStarted(plan), now);
    app.event(&Event::Measuring(stage), now);
}

fn ended(outcome: Outcome) -> Event {
    Event::RunFinished {
        outcome,
        error: None,
        elapsed: SECOND * 12,
    }
}

#[test]
fn the_chooser_keeps_up_to_four_servers_and_none_takes_the_default_ones() {
    let (mut app, now) = app();
    app.event(&prepared(), now);
    for code in [
        KeyCode::Char('s'),
        KeyCode::Down,
        KeyCode::Down,
        KeyCode::Enter,
    ] {
        press(&mut app, code, now);
    }
    assert!(shows(&mut app, now, "Test servers · 1 selected"));
    press(&mut app, KeyCode::Char(' '), now);
    assert!(shows(&mut app, now, "Test servers · 0 selected"));
    press(&mut app, KeyCode::Enter, now);
    assert!(
        shows(&mut app, now, "Settings"),
        "applying returns to the settings"
    );
    assert!(checked(app.tick(now + REST)).servers.is_empty());

    let (mut app, now) = self::app();
    app.event(&prepared(), now);
    for code in [
        KeyCode::Char('s'),
        KeyCode::Down,
        KeyCode::Down,
        KeyCode::Enter,
    ] {
        press(&mut app, code, now);
    }
    for _ in 0..4 {
        press(&mut app, KeyCode::Down, now);
        press(&mut app, KeyCode::Char(' '), now);
    }
    assert!(shows(&mut app, now, "Test servers · 4 selected"));
    assert!(shows(
        &mut app,
        now,
        "At most 4 test servers share one test."
    ));
    press(&mut app, KeyCode::Enter, now);
    let config = checked(app.tick(now + REST));
    let ids: Vec<_> = config.servers.iter().map(ServerId::as_str).collect();
    assert_eq!(ids, ["a", "b", "c", "d"]);
}

#[test]
fn a_server_that_cannot_be_reached_opens_its_address_and_enter_tries_again() {
    let (mut app, now) = app();
    assert!(
        !frame(&mut app, now)[0].ends_with("Failed"),
        "no server prepared yet is no failure"
    );
    let failure = Failure::new(
        FailureReason::PathFailed,
        graphite_meter_client::net::UNREACHABLE,
    );
    app.event(&Event::Checking(Kind::Check), now);
    app.event(&Event::CheckFailed(failure), now);
    assert!(
        frame(&mut app, now)[0].ends_with("● Failed"),
        "{:?}",
        frame(&mut app, now)[0]
    );
    assert!(
        shows(&mut app, now, "Could not reach meter.example")
            && !shows(&mut app, now, "meter.example.")
    );
    assert!(shows(
        &mut app,
        now,
        "Check the address and that the server is running; Enter tries again."
    ));
    press(&mut app, KeyCode::Enter, now);
    assert_eq!(
        checked(app.tick(now)),
        config(),
        "enter checks again at once"
    );
    app.event(&Event::Checking(Kind::Check), now);
    assert!(shows(&mut app, now, "Checking meter.example…"));
    app.event(&prepared(), now);
    assert!(
        !shows(&mut app, now, "Connect to a server"),
        "a server that prepared closes the address"
    );
}

#[test]
fn a_canceled_or_expired_sign_in_asks_again_on_the_next_start() {
    for (end, notice) in [
        (SignInEnd::Canceled, "Sign-in canceled"),
        (SignInEnd::Expired, "Sign-in expired"),
    ] {
        let (mut app, now) = app();
        app.event(&Event::Checking(Kind::Check), now);
        app.event(&Event::SignIn(prompt(now)), now);
        assert!(shows(&mut app, now, "Sign in to Meter") && shows(&mut app, now, "ABCD-EFGH"));
        assert_eq!(
            press(&mut app, KeyCode::Enter, now),
            [Effect::OpenBrowser(PAGE.into())]
        );
        assert!(shows(&mut app, now, "Waiting for approval…"));
        if end == SignInEnd::Canceled {
            assert_eq!(command(&press(&mut app, KeyCode::Esc, now)), &Command::Stop);
        }
        app.event(&Event::SignInEnded(end), now);
        app.event(&Event::CheckFailed(Unapproved::Expired.failure()), now);
        assert!(
            shows(&mut app, now, notice) && !shows(&mut app, now, "ABCD-EFGH"),
            "{end:?}"
        );
        assert!(
            !shows(&mut app, now, "Connect to a server"),
            "a lapsed sign-in leaves the address alone"
        );
        assert!(frame(&mut app, now)[0].ends_with("● Sign in"), "{end:?}");
        assert!(
            matches!(
                command(&press(&mut app, KeyCode::Char('r'), now)),
                Command::Run(_)
            ),
            "a start signs in again"
        );
        let (mut app, now) = self::app();
        app.event(&Event::Checking(Kind::Check), now);
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
    assert_eq!(
        command(&press(&mut app, KeyCode::Char('q'), now)),
        &Command::Stop
    );
    assert!(shows(
        &mut app,
        now,
        "Stopping the test before quitting… Ctrl+C quits at once."
    ));
    assert_eq!(press(&mut app, KeyCode::Char('q'), now), []);
    assert_eq!(app.event(&ended(Outcome::Stopped), now), [Effect::Quit]);
    let exit = app.exit();
    assert_eq!((exit.report, status(&exit.view, exit.signal)), (true, 1));
    let lines = report(&exit.view, 100, &exit.palette);
    assert!(
        lines[0].text().starts_with("Graphite Meter  Stopped"),
        "{:?}",
        lines[0]
    );
    // A second interrupt quits at once without a report; an ended run's report follows, its status the signal's.
    for (key, code) in [(true, INTERRUPTED), (false, TERMINATED)] {
        for outcome in [None, Some(Outcome::Partial), Some(Outcome::Complete)] {
            let mut app = running(&["a"], &download, now);
            let interrupt = |app: &mut App| {
                if key {
                    app.key(ctrl_c(), now)
                } else {
                    app.signal(code)
                }
            };
            assert_eq!(command(&interrupt(&mut app)), &Command::Stop);
            let quit = match outcome {
                None => interrupt(&mut app),
                Some(outcome) => app.event(&ended(outcome), now),
            };
            assert_eq!(quit, [Effect::Quit]);
            let exit = app.exit();
            assert_eq!(
                (exit.report, status(&exit.view, exit.signal)),
                (outcome.is_some(), code),
                "{outcome:?}"
            );
        }
    }
}

#[test]
fn esc_stops_a_run_at_once_and_quitting_after_one_reports_it() {
    let now = Instant::now();
    let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
    assert_eq!(command(&press(&mut app, KeyCode::Esc, now)), &Command::Stop);
    assert_eq!(
        press(&mut app, KeyCode::Esc, now),
        [],
        "one stop is asked for once"
    );
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
    let mut app = App::new(
        Config::default(),
        None,
        (Profile::Plain, Palette::new(true)),
        now,
    );
    assert_eq!(app.tick(now), [], "nothing is checked without a server");
    assert!(shows(&mut app, now, "Connect to a server"));
    assert!(shows(
        &mut app,
        now,
        "Enter the address of your Graphite Meter server, or paste it."
    ));
    assert!(
        shows(&mut app, now, "Remember this server ○ off tab"),
        "remembering waits to be asked for"
    );
    press(&mut app, KeyCode::Esc, now);
    assert!(frame(&mut app, now)[0].ends_with("○ No server"));
    assert!(shows(&mut app, now, "Start test needs a server"));
    assert!(shows(
        &mut app,
        now,
        "Add a server to measure your connection"
    ));
    assert_eq!(
        press(&mut app, KeyCode::Char('r'), now),
        [],
        "a start without a server asks for one"
    );
    assert!(shows(&mut app, now, "Connect to a server"));
    assert_eq!(
        press(&mut app, KeyCode::Tab, now),
        [],
        "nothing is kept before a server prepared"
    );
    assert!(shows(&mut app, now, "Remember this server ● on"));
    app.input(Input::Paste("meter.example/history\n".into()), now);
    press(&mut app, KeyCode::Enter, now);
    assert_eq!(
        checked(app.tick(now)),
        config(),
        "the address is checked at once"
    );
    app.event(&Event::Checking(Kind::Check), now);
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
    assert!(shows(&mut app, now, "Remember this server ● on"));
    assert_eq!(
        press(&mut app, KeyCode::Char(' '), now),
        [Effect::Forget],
        "turning it off deletes the file"
    );
    assert_eq!(
        app.event(&prepared(), now),
        [],
        "a forgotten server is not kept again"
    );
    assert_eq!(
        press(&mut app, KeyCode::Right, now),
        [Effect::Remember(kept.clone())],
        "a ready server is kept at once"
    );
    let mut later = App::new(
        config(),
        Some(kept.clone()),
        (Profile::Plain, Palette::new(true)),
        now,
    );
    later.tick(now);
    press(&mut later, KeyCode::Char('s'), now);
    assert!(
        shows(&mut later, now, "Remember this server ● on"),
        "a kept server starts remembered"
    );

    let scratch = graphite_meter_testkit::Scratch::new().unwrap();
    let path = scratch.path().join("graphite-meter").join("server");
    assert_eq!(config::recall(&path), None);
    config::remember(&path, &kept);
    assert_eq!(config::recall(&path), Some(kept));
    config::forget(&path);
    assert!(!path.exists(), "forgetting deletes the kept server");
    std::fs::write(&path, "not a server\n").unwrap();
    assert_eq!(
        config::recall(&path),
        None,
        "a kept address that no longer parses is ignored"
    );

    // An address pasted over the console is the one to test.
    let (mut app, now) = self::app();
    app.input(Input::Paste("https://other.example".into()), now);
    press(&mut app, KeyCode::Enter, now);
    assert_eq!(
        checked(app.tick(now)).url,
        Origin::parse("https://other.example").ok()
    );
}

#[test]
fn idle_latency_fills_its_lane_live_from_the_replies() {
    let now = Instant::now();
    let mut app = running(&["a"], &[(Stage::Latency, SECOND * 4)], now);
    measuring(&mut app, Stage::Latency, SECOND * 4, now);
    let mut latency = Latency::default();
    let mut probed = |app: &mut App, at: Duration, rtt: Option<Duration>| {
        latency.record(
            rtt.map_or(ProbeOutcome::Timeout, |rtt| ProbeOutcome::Reply {
                rtt,
                handling: Duration::ZERO,
            }),
        );
        let (server, probes) = (id("a"), Box::new([Probed { at, rtt }]));
        app.event(
            &Event::Probes {
                server,
                probes,
                latency: latency.summary(),
            },
            now + at,
        );
    };
    for ms in 10..=29 {
        probed(
            &mut app,
            Duration::from_millis(100 * (ms - 9)),
            Some(Duration::from_millis(ms)),
        );
    }
    probed(&mut app, SECOND * 3, None);
    let later = now + SECOND * 3;
    let lane = frame(&mut app, later)
        .into_iter()
        .find(|row| row.contains("≈ Idle"))
        .unwrap();
    assert!(
        lane.contains("19.5 ms") && lane.contains("4.8%"),
        "{lane:?}"
    );
    assert!(
        shows(&mut app, later, "Range 10.0 to 29.0 ms") && shows(&mut app, later, "Replies 20")
    );
    assert!(
        shows(&mut app, later, "1 timeout in a row"),
        "the dial notes the streak"
    );
}

#[test]
fn the_console_follows_a_download_and_takes_presses() {
    let now = Instant::now();
    let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
    measuring(&mut app, Stage::Download, SECOND * 10, now);
    for tick in 1..=20 {
        let (at, rates) = (
            Duration::from_millis(100 * tick),
            Dir {
                down: Some(12.5e6),
                up: None,
            },
        );
        app.event(&Event::Sample { at, rates }, now + at);
    }
    let later = now + SECOND * 3;
    assert!(
        shows(&mut app, later, "↓ Download Running"),
        "{:#?}",
        frame(&mut app, later)
    );
    assert!(shows(&mut app, later, "100.0 Mbit/s") && shows(&mut app, later, "Download 3.0"));
    let rate = Rate {
        mean: 12.5e6,
        peak: 13e6,
    };
    let throughput = Dir {
        down: Some(Throughput {
            rate: Some(rate),
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
        assert!(
            shows(&mut app, later, fact),
            "{fact}: {:#?}",
            frame(&mut app, later)
        );
    }

    // The key, a chip, a bar's cap and a sheet's backdrop each take a press.
    let key = find(&mut app, later, "Run again");
    assert_matches!(command(&click(&mut app, later, key)), Command::Run(_));
    let (mut app, now) = self::app();
    app.event(&prepared(), now);
    let chip = find(&mut app, now, "↑ Upload");
    assert_eq!(click(&mut app, now, chip), [], "a chip switches its stage");
    assert!(shows(&mut app, now, "↑ Upload Skipped"));
    assert_eq!(
        checked(app.tick(now + REST)).stages,
        [Stage::Latency, Stage::Download]
    );
    let cap = find(&mut app, now, "settings");
    click(&mut app, now, (cap.0 - 3, cap.1));
    assert!(shows(&mut app, now, "Settings esc close"));
    click(&mut app, now, (2, 20));
    assert!(
        !shows(&mut app, now, "Settings esc close"),
        "a press beside a sheet closes it"
    );
    let moved = pointer(MouseEventKind::Moved, find(&mut app, now, "Start test"));
    app.input(moved, now);
    assert!(app.hovered().is_some(), "the key lights under the pointer");
}

fn result_of(stage: Stage) -> StageResult {
    StageResult {
        stage,
        measured: SECOND * 10,
        stopped: false,
        throughput: Dir {
            down: Some(Throughput {
                rate: Some(Rate {
                    mean: 12.5e6,
                    peak: 13e6,
                }),
                bytes: 125_000_000,
            }),
            up: None,
        },
        servers: Vec::new(),
        failures: Vec::new(),
    }
}

#[test]
fn issues_and_a_blocked_start_read_as_the_browser_says_them() {
    let now = Instant::now();
    let mut app = running(&["a", "b"], &[(Stage::Download, SECOND * 10)], now);
    measuring(&mut app, Stage::Download, SECOND * 10, now);
    let failed = |scope, reason| ServerFailure {
        server: id("b"),
        scope,
        failure: Failure::new(reason, "lost"),
        at: now,
    };
    app.event(
        &Event::ServerFailed(failed(Scope::Latency, FailureReason::Timeout)),
        now,
    );
    assert!(shows(
        &mut app,
        now,
        "B · Loaded download: Stopped delivering data"
    ));
    let lost = failed(Scope::Throughput, FailureReason::ConnectionLost);
    app.event(&Event::ServerFailed(lost.clone()), now);
    assert!(shows(&mut app, now, "B · Download: Connection lost"));
    let result = StageResult {
        failures: vec![lost.clone(), lost],
        ..result_of(Stage::Download)
    };
    app.event(&Event::StageFinished(result), now);
    app.event(&ended(Outcome::Partial), now);
    let rows = frame(&mut app, now);
    let named = rows
        .iter()
        .map(|row| row.matches("B: connection lost").count());
    assert_eq!(
        named.sum::<usize>(),
        1,
        "the card names the server once: {rows:#?}"
    );

    let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
    measuring(&mut app, Stage::Download, SECOND * 10, now);
    let alone = ServerFailure {
        server: id("a"),
        ..failed(Scope::Throughput, FailureReason::ConnectionLost)
    };
    app.event(&Event::ServerFailed(alone.clone()), now);
    assert!(shows(&mut app, now, "Download: Connection lost"));
    let mut buffer = Buffer::empty(Rect::new(0, 0, 110, 36));
    app.draw(&mut buffer, now);
    let bar: String = buffer.content()[110 * 35..]
        .iter()
        .map(Cell::symbol)
        .collect();
    assert!(
        bar.contains("Download: Connection lost")
            && bar.contains("stop test")
            && !bar.contains("keys"),
        "the keys after the first give way to a notice: {bar}"
    );
    let loaded = ServerFailure {
        scope: Scope::Latency,
        ..alone
    };
    let result = StageResult {
        failures: vec![loaded],
        ..result_of(Stage::Download)
    };
    app.event(&Event::StageFinished(result), now);
    app.event(&ended(Outcome::Partial), now);
    let rows = frame(&mut app, now);
    assert!(
        rows.iter().any(|row| row.contains("↓ Download ✓"))
            && !rows.iter().any(|row| row.contains("Download Partial")),
        "loaded latency lost leaves the download whole: {rows:#?}"
    );
    press(&mut app, KeyCode::Char('2'), now);
    assert!(
        frame(&mut app, now)
            .last()
            .unwrap()
            .starts_with("Test cannot start"),
        "settings that cannot run outrank the last outcome"
    );

    let mut app = running(&["a", "b"], &[(Stage::Download, SECOND * 10)], now);
    app.event(
        &Event::ServerFailed(failed(Scope::Server, FailureReason::PathFailed)),
        now,
    );
    assert!(shows(&mut app, now, "B · Start: Path unavailable"));

    let config = Config {
        stages: Vec::new(),
        ..config()
    };
    let mut app = App::new(config, None, (Profile::Plain, Palette::new(true)), now);
    let blocked = frame(&mut app, now);
    let count = |text: &str| blocked.iter().filter(|row| row.contains(text)).count();
    assert_eq!(
        count("Test cannot start"),
        2,
        "dial and status bar: {blocked:#?}"
    );
    assert!(shows(&mut app, now, "Start test turn on a stage"));
}

#[test]
fn a_stage_without_a_selected_server_is_partial_and_says_who_it_lacks() {
    let now = Instant::now();
    let plan = [(Stage::Download, SECOND * 10), (Stage::Upload, SECOND * 10)];
    let mut app = running(&["a", "b"], &plan, now);
    let own = |name: &str, left: bool| ServerResult {
        server: id(name),
        left,
        throughput: Dir::default(),
        latency: None,
    };
    let lost = ServerFailure {
        server: id("b"),
        scope: Scope::Throughput,
        failure: Failure::new(FailureReason::ConnectionLost, "lost"),
        at: now,
    };
    measuring(&mut app, Stage::Download, SECOND * 10, now);
    app.event(&Event::ServerFailed(lost.clone()), now);
    let download = StageResult {
        servers: vec![own("a", false), own("b", true)],
        failures: vec![lost.clone()],
        ..result_of(Stage::Download)
    };
    app.event(&Event::StageFinished(download), now);
    measuring(&mut app, Stage::Upload, SECOND * 10, now);
    let upload = StageResult {
        stage: Stage::Upload,
        throughput: Dir {
            down: None,
            up: result_of(Stage::Download).throughput.down,
        },
        servers: vec![own("a", false)],
        ..result_of(Stage::Upload)
    };
    app.event(&Event::StageFinished(upload), now);
    app.event(&ended(Outcome::Partial), now);
    let rows = frame(&mut app, now);
    for said in [
        "↑ Upload Partial",
        "Without B",
        "B: connection lost in Download",
    ] {
        assert!(
            rows.iter().any(|row| row.contains(said)),
            "{said}: {rows:#?}"
        );
    }

    let mut app = running(&["a", "b"], &plan, now);
    measuring(&mut app, Stage::Download, SECOND * 10, now);
    let gone = |name: &str| ServerFailure {
        server: id(name),
        ..lost.clone()
    };
    let download = StageResult {
        servers: vec![own("a", true), own("b", true)],
        failures: vec![gone("a"), gone("b")],
        ..result_of(Stage::Download)
    };
    app.event(&Event::StageFinished(download), now);
    app.event(&ended(Outcome::Incomplete), now);
    let rows = frame(&mut app, now);
    for said in [
        "↑ Upload Not reached",
        "Upload not reached: connection lost in Download",
    ] {
        assert!(
            rows.iter().any(|row| row.contains(said)),
            "{said}: {rows:#?}"
        );
    }
}

fn downloaded(now: Instant) -> App {
    let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
    measuring(&mut app, Stage::Download, SECOND * 10, now);
    let throughput = Dir {
        down: Some(Throughput {
            rate: Some(Rate {
                mean: 12.5e6,
                peak: 13e6,
            }),
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
    };
    app.event(&Event::StageFinished(result), now);
    app.event(&ended(Outcome::Complete), now);
    app
}

#[test]
fn results_stay_until_the_next_run_starts_and_a_start_that_ends_first_keeps_them() {
    let now = Instant::now();
    let mut app = downloaded(now);
    assert_matches!(
        command(&press(&mut app, KeyCode::Enter, now)),
        Command::Run(_)
    );
    app.event(&Event::Checking(Kind::Run), now);
    assert!(
        shows(&mut app, now, "Transferred 125.0 MB"),
        "the results stay while the next run checks"
    );
    assert!(
        frame(&mut app, now)
            .last()
            .unwrap()
            .starts_with("Checking paths")
    );
    assert_eq!(command(&press(&mut app, KeyCode::Esc, now)), &Command::Stop);
    app.event(&ended(Outcome::Stopped), now);
    assert!(
        shows(&mut app, now, "Transferred 125.0 MB")
            && shows(&mut app, now, "Test stopped before it started.")
    );
    let exit = app.exit();
    assert_eq!(
        (exit.report, status(&exit.view, None)),
        (true, 1),
        "the report is the shown run's"
    );

    let mut app = downloaded(now);
    press(&mut app, KeyCode::Enter, now);
    app.event(&Event::Checking(Kind::Run), now);
    app.event(
        &Event::RunStarted {
            plan: vec![(Stage::Upload, SECOND * 10)],
            focus: id("a"),
            at: now,
        },
        now,
    );
    assert!(
        !shows(&mut app, now, "Transferred 125.0 MB"),
        "a started run replaces them"
    );
}

#[test]
fn the_bidirectional_chip_follows_its_setting_after_a_run() {
    let now = Instant::now();
    let mut app = downloaded(now);
    assert!(!shows(&mut app, now, "↕ Bidire"));
    press(&mut app, KeyCode::Char('4'), now);
    assert!(
        shows(&mut app, now, "↕ Bidire"),
        "{:#?}",
        frame(&mut app, now)
    );
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
    assert!(
        shows(&mut app, now, "Round trips during transfers"),
        "{:#?}",
        frame(&mut app, now)
    );
    press(&mut app, KeyCode::Char(' '), now);
    assert_eq!(app.tick(now + REST), [], "a check would replace the run");
    app.event(&ended(Outcome::Complete), now + REST);
    assert!(!checked(app.tick(now + REST)).loaded_latency);
}

#[test]
fn the_sign_in_card_stays_for_a_press_beside_it_and_ctrl_c_while_typing_stops_a_run_first() {
    let (mut app, now) = app();
    app.event(&Event::Checking(Kind::Check), now);
    app.event(&Event::SignIn(prompt(now)), now);
    click(&mut app, now, (2, 20));
    assert!(
        shows(&mut app, now, "Sign in to Meter"),
        "the sign-in still waits"
    );

    let mut app = running(&["a"], &[(Stage::Download, SECOND * 10)], now);
    press(&mut app, KeyCode::Char('s'), now);
    press(&mut app, KeyCode::Enter, now);
    assert_eq!(command(&app.key(ctrl_c(), now)), &Command::Stop);
}

#[test]
fn the_settings_editor_applies_values_in_bounds_and_says_why_it_refuses_others() {
    let (mut app, now) = app();
    let typed = |app: &mut App, text: &str| {
        app.key(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            now,
        );
        text.chars()
            .for_each(|c| _ = press(app, KeyCode::Char(c), now));
        press(app, KeyCode::Enter, now);
    };
    press(&mut app, KeyCode::Char('s'), now);
    (0..15).for_each(|_| _ = press(&mut app, KeyCode::Down, now));
    press(&mut app, KeyCode::Enter, now);
    typed(&mut app, "15");
    assert!(
        shows(
            &mut app,
            now,
            "Streams must be a whole number from 1 to 14."
        ),
        "{:#?}",
        frame(&mut app, now)
    );
    typed(&mut app, "3");
    assert!(
        shows(&mut app, now, "HTTP/1.1 stream limit 3"),
        "{:#?}",
        frame(&mut app, now)
    );
    assert_eq!(checked(app.tick(now + REST)).streams.auto, 3);
    (0..4).for_each(|_| _ = press(&mut app, KeyCode::Up, now));
    press(&mut app, KeyCode::Enter, now);
    typed(&mut app, "5");
    assert!(
        shows(&mut app, now, "Warmup must be from 0s to 4s."),
        "{:#?}",
        frame(&mut app, now)
    );
    typed(&mut app, "soon");
    assert!(shows(
        &mut app,
        now,
        "Use a time like 800ms, 4s or 1m; a bare number is seconds."
    ));
    typed(&mut app, "1.5");
    assert!(
        shows(&mut app, now, "Warmup 1.5 s"),
        "{:#?}",
        frame(&mut app, now)
    );
    match command(&press(&mut app, KeyCode::Char('r'), now)) {
        Command::Run(config) => assert_eq!(config.warmup, Duration::from_millis(1500)),
        command => panic!("{command:?}"),
    }
}

#[test]
fn a_background_answer_arriving_late_never_presses_a_key() {
    let (mut app, now) = app();
    app.event(&prepared(), now);
    let mut effects = app.key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT), now);
    for c in "11;rgb:1e1e/1e1e/1e1e".chars() {
        effects.extend(press(&mut app, KeyCode::Char(c), now));
    }
    effects.extend(app.key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::ALT), now));
    assert_eq!(effects, [], "the answer started nothing");
    assert!(!shows(&mut app, now, "Settings esc close"));
    assert_matches!(
        command(&press(&mut app, KeyCode::Char('r'), now)),
        Command::Run(_)
    );
}
