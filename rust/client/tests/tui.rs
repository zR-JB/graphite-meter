//! The interface without a terminal: keys, events and ticks in, frames, chrome and effects out.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use graphite_meter_client::{
    config::Config,
    controller::Command,
    events::{Event, SignInEnd, SignInPrompt},
    net::{ThroughputPath, approval::Unapproved},
    run::prepare::{Paths, ServerPath},
    text::Profile,
    tui::{App, Effect},
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

const WIDTH: u16 = 120;

/// Setup for the default settings, its first check already asked for.
fn app() -> (App, Instant) {
    let start = Instant::now();
    let mut app = App::new(Config::default(), Profile::Plain, start);
    assert_eq!(checked(app.tick(start)), Config::default());
    (app, start)
}

/// The frame's rows, each run of spaces as one.
/// The settings of the one check `effects` ask for.
fn checked(effects: Vec<Effect>) -> Config {
    match &effects[..] {
        [Effect::Command(command)] => match command.as_ref() {
            Command::Check(config) => config.clone(),
            command => panic!("{command:?}"),
        },
        effects => panic!("{effects:?}"),
    }
}

fn frame(app: &mut App, now: Instant) -> Vec<String> {
    let mut buffer = Buffer::empty(Rect::new(0, 0, WIDTH, 40));
    app.draw(&mut buffer, now);
    let rows = buffer.content().chunks(usize::from(WIDTH));
    let row = |cells: &[ratatui_core::buffer::Cell]| cells.iter().map(|cell| cell.symbol()).collect::<String>();
    rows.map(|cells| row(cells).split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// The focused row of the frame's body.
fn focused(app: &mut App, now: Instant) -> String {
    let rows = frame(app, now);
    let mut focused = rows.into_iter().filter(|row| row.contains('›'));
    let row = focused.next().expect("a focused row");
    assert_eq!(focused.next(), None);
    row
}

fn press(app: &mut App, code: KeyCode, now: Instant) -> Vec<Effect> {
    app.key(KeyEvent::new(code, KeyModifiers::NONE), now)
}

fn presses(app: &mut App, code: KeyCode, times: usize, now: Instant) {
    for _ in 0..times {
        assert_eq!(press(app, code, now), []);
    }
}

fn shows(app: &mut App, now: Instant, text: &str) -> bool {
    frame(app, now).iter().any(|row| row.contains(text))
}

/// A catalogue of five servers of which the check prepared the first.
fn prepared() -> Event {
    let origin = |name: &str| Origin::parse(&format!("https://{name}.example")).unwrap();
    let entry = |name: &str| ServerEntry {
        id: ServerId::parse(name).unwrap(),
        url: BaseUrl::Origin(origin(name)),
        name: name.to_uppercase(),
        location: String::new(),
        additional_origins: Vec::new(),
    };
    let throughput = ThroughputPath {
        origin: origin("a"),
        transport: ThroughputTransport::FetchStream,
        protocol: Protocol::Http2,
    };
    let paths = Paths {
        throughput,
        latency: None,
        stage_limit: Duration::from_secs(300),
        idle_rtt: Duration::ZERO,
    };
    let server = ServerPath {
        id: ServerId::parse("a").unwrap(),
        name: "A".into(),
        location: String::new(),
        origin: origin("a"),
        offered: None,
        path: Ok(paths),
    };
    let catalogue = ["a", "b", "c", "d", "e"].map(entry);
    Event::Prepared { servers: Arc::new([server]), catalogue: Arc::new(catalogue) }
}

#[test]
fn the_first_frame_focuses_start_test_while_the_paths_are_checked() {
    let start = Instant::now();
    let mut app = App::new(Config::default(), Profile::Plain, start);
    let rows = frame(&mut app, start);
    assert!(rows[0].contains("Graphite Meter") && rows[0].ends_with("Checking paths"), "{rows:#?}");
    assert!(focused(&mut app, start).contains("› Start test ⠋ checking paths"));
    assert!(
        rows.iter()
            .any(|row| row.contains("enter start test • ↑/↓ move • ? keys • q quit")),
        "{rows:#?}"
    );
}

#[test]
fn arrows_move_between_rows_and_change_values() {
    let (mut app, now) = app();
    press(&mut app, KeyCode::Up, now);
    assert!(focused(&mut app, now).contains("Start test"));
    press(&mut app, KeyCode::Down, now);
    assert!(focused(&mut app, now).contains("Catalogue URL http://127.0.0.1:7246"));
    presses(&mut app, KeyCode::Down, 2, now);
    assert!(focused(&mut app, now).contains("Throughput path Automatic · each server"));
    press(&mut app, KeyCode::Right, now);
    assert!(focused(&mut app, now).contains("Throughput path Fetch streams · every server"));
    presses(&mut app, KeyCode::Down, 3, now);
    assert!(focused(&mut app, now).contains("Latency ● 4 s"));
    press(&mut app, KeyCode::Right, now);
    assert!(focused(&mut app, now).contains("Latency ● 5 s"));
    presses(&mut app, KeyCode::Left, 2, now);
    assert!(focused(&mut app, now).contains("Latency ● 3 s"));
    assert!(shows(&mut app, now, "Latency 3 s."));
}

#[test]
fn space_toggles_a_stage() {
    let (mut app, now) = app();
    presses(&mut app, KeyCode::Down, 7, now);
    assert!(focused(&mut app, now).contains("Download ● 10 s"));
    press(&mut app, KeyCode::Char(' '), now);
    assert!(focused(&mut app, now).contains("Download ○ 10 s"));
    assert!(shows(&mut app, now, "Download off."));
    press(&mut app, KeyCode::Char(' '), now);
    assert!(focused(&mut app, now).contains("Download ● 10 s"));
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
    let typed = 4096 - "http://127.0.0.1:7246".len() - 1;
    presses(&mut app, KeyCode::Char('a'), typed, now);
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
    let later = now + Duration::from_millis(350);
    let config = checked(app.tick(later));
    assert_eq!(config.url.to_string(), "https://meter.example");
}

#[test]
fn the_chooser_keeps_one_to_four_servers() {
    let (mut app, now) = app();
    app.event(&prepared(), now);
    press(&mut app, KeyCode::Char('s'), now);
    assert!(shows(&mut app, now, "Test servers · 1 selected"));
    press(&mut app, KeyCode::Char(' '), now);
    assert!(shows(&mut app, now, "At least one server takes the test."));
    for _ in 0..4 {
        press(&mut app, KeyCode::Down, now);
        press(&mut app, KeyCode::Char(' '), now);
    }
    assert!(shows(&mut app, now, "Test servers · 4 selected"));
    assert!(shows(&mut app, now, "At most four servers share one test."));
    press(&mut app, KeyCode::Enter, now);
    assert!(shows(&mut app, now, "Checking the selected servers…"));
    let later = now + Duration::from_millis(350);
    let config = checked(app.tick(later));
    let ids: Vec<_> = config.servers.iter().map(ServerId::as_str).collect();
    assert_eq!(ids, ["a", "b", "c", "d"]);
}

#[test]
fn question_mark_lists_every_key_of_the_screen() {
    let (mut app, now) = app();
    app.event(&prepared(), now);
    assert!(!shows(&mut app, now, "recheck paths"));
    press(&mut app, KeyCode::Char('?'), now);
    for key in ["r start test", "space on/off", "enter start or open", "v recheck paths", "s test servers"] {
        assert!(shows(&mut app, now, key), "{key}");
    }
    assert!(shows(&mut app, now, "a automatic paths"));
    press(&mut app, KeyCode::Char('s'), now);
    for key in ["space select", "enter apply", "esc cancel"] {
        assert!(shows(&mut app, now, key), "{key}");
    }
    assert!(!shows(&mut app, now, "recheck paths"));
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

const PAGE: &str = "https://meter.example/auth/cli?challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

/// The sign-in screen for `PAGE`, which expires two minutes after `now`.
fn signing_in(now: Instant) -> App {
    let mut app = App::new(Config::default(), Profile::Plain, now);
    let prompt = SignInPrompt {
        issuer: "Meter".into(),
        url: PAGE.into(),
        code: "ABCD-EFGH".into(),
        deadline: (now + Duration::from_secs(120)).into(),
    };
    app.event(&Event::SignIn(prompt), now);
    app
}

/// The text of `row` from `column` on, as drawn.
fn drawn(buffer: &Buffer, row: u16, column: u16) -> String {
    let width = buffer.area.width;
    (column..width)
        .map(|x| buffer[(x, row)].symbol())
        .collect::<String>()
        .trim_end()
        .to_owned()
}

#[test]
fn sign_in_shows_the_code_and_links_the_page() {
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
    let mut buffer = Buffer::empty(Rect::new(0, 0, WIDTH, 40));
    let chrome = app.draw(&mut buffer, later);
    let [link] = &chrome.links[..] else { panic!("{:?}", chrome.links) };
    assert_eq!((link.url.as_str(), link.text.text()), (PAGE, PAGE.to_owned()));
    assert_eq!(drawn(&buffer, link.row, link.column), PAGE);
}

#[test]
fn a_wrapped_link_records_the_rows_it_is_scrolled_to() {
    let now = Instant::now();
    let mut app = signing_in(now);
    let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 12));
    assert_eq!(app.draw(&mut buffer, now).links, [], "the link is below the viewport");
    press(&mut app, KeyCode::PageDown, now);
    let chrome = app.draw(&mut buffer, now);
    let texts: Vec<_> = chrome.links.iter().map(|link| link.text.text()).collect();
    assert_eq!(texts.concat(), PAGE);
    assert_eq!(chrome.links.iter().map(|link| link.row).collect::<Vec<_>>(), [7, 8, 9]);
    for link in &chrome.links {
        assert_eq!(drawn(&buffer, link.row, link.column), link.text.text());
    }
}

#[test]
fn enter_space_and_o_open_the_page_and_esc_cancels() {
    let now = Instant::now();
    for code in [KeyCode::Enter, KeyCode::Char(' '), KeyCode::Char('o')] {
        let mut app = signing_in(now);
        assert_eq!(press(&mut app, code, now), [Effect::OpenBrowser(PAGE.into())], "{code:?}");
        assert!(shows(&mut app, now, "⠋ Waiting for approval…"));
        assert!(shows(&mut app, now, "Sign-in page opened in the browser."));
    }
    let mut app = signing_in(now);
    assert_eq!(press(&mut app, KeyCode::Esc, now), [Effect::Command(Box::new(Command::Stop))]);
    app.event(&Event::SignInEnded(SignInEnd::Cancelled), now);
    assert!(shows(&mut app, now, "Sign-in canceled. Press v to request a new code."));
    assert!(shows(&mut app, now, "Start test"));
}

#[test]
fn an_expired_sign_in_asks_for_a_new_code() {
    let now = Instant::now();
    let mut app = signing_in(now);
    app.event(&Event::SignInEnded(SignInEnd::Expired), now);
    app.event(&Event::CheckFailed(Unapproved::Expired.failure()), now);
    assert!(shows(&mut app, now, "Sign-in expired. Press v to request a new code."));
    assert!(!shows(&mut app, now, "Match this code"));
}
