//! Go's TUI tests (main_test.go and run_test.go) over the controller's snapshots.
use super::*;
use crate::model::{AuthPrompt, FailureScope, Point, ServerFailure, ServerLatency, ServerLatencyResult, StageResult};
use graphite_meter_core::{
    discovery::{LatencyTarget, LatencyTransport, Protocol, ThroughputTarget, ThroughputTransport},
    failure::FailureReason,
    latency::{LatencyAccumulator, LatencySummary, ProbeOutcome},
    measurement::{Direction, MeasurementResult},
};
use ratatui_core::{backend::TestBackend, terminal::Terminal};

/// 1.5 MB received in a second.
pub(crate) fn download_measurement() -> MeasurementResult {
    MeasurementResult {
        direction: Direction::Down,
        total_bytes: 1_500_000,
        mean_bytes_per_sec: Some(1_500_000.0),
        peak_bytes_per_sec: Some(1_500_000.0),
        samples: 4,
        elapsed_nanos: Some(1_000_000_000),
    }
}

/// The summary of replies with these round trips, then of timed-out probes.
pub(crate) fn probes(rtts: &[i64], timeouts: usize) -> LatencySummary {
    let mut probes = LatencyAccumulator::default();
    for &rtt_nanos in rtts {
        probes.record(ProbeOutcome::Reply {
            rtt_nanos,
            handling_nanos: 0,
        });
    }
    for _ in 0..timeouts {
        probes.record(ProbeOutcome::Timeout);
    }
    probes.snapshot()
}

/// A server's latency over a second.
pub(crate) fn latency_result(id: &str, summary: LatencySummary) -> ServerLatencyResult {
    ServerLatencyResult {
        elapsed: Some(Duration::from_secs(1)),
        id: id.into(),
        summary,
        ending: None,
    }
}

fn key(name: &str) -> KeyEvent {
    let (modifiers, base) = match name.split_once('+') {
        Some(("ctrl", base)) => (KeyModifiers::CONTROL, base),
        Some(("alt", base)) => (KeyModifiers::ALT, base),
        _ => (KeyModifiers::NONE, name),
    };
    let code = match base {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "tab" => KeyCode::Tab,
        "shift+tab" => KeyCode::BackTab,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pgdown" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "backspace" => KeyCode::Backspace,
        base => KeyCode::Char(base.chars().next().unwrap()),
    };
    KeyEvent::new(code, modifiers)
}

fn press(ui: &mut Ui, commands: &mpsc::Sender<Command>, names: &[&str]) -> bool {
    names.iter().any(|name| ui.key(key(name), commands))
}

/// The screen `ui` draws, one string per row.
fn rows(ui: &mut Ui) -> Vec<String> {
    let (width, height) = ui.size;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    let cells = terminal.backend().buffer().content().chunks(usize::from(width));
    cells
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect()
}

fn screen(ui: &mut Ui) -> String {
    rows(ui).join("\n")
}

fn plain(lines: &[ratatui_core::text::Line]) -> String {
    lines.iter().map(crate::report::plain).collect::<Vec<_>>().join("\n")
}

/// Go's testModel: setup at 120×40 with its paths ready.
fn setup() -> Ui {
    let mut ui = Ui::new(Config::default(), Snapshot::default());
    (ui.size, ui.check_started) = ((120, 40), None);
    ui
}

fn target(id: &str) -> (ThroughputTarget, LatencyTarget) {
    let origin = format!("https://{id}.example");
    (
        ThroughputTarget {
            base_url: origin.clone(),
            transport: ThroughputTransport::FetchStream,
            protocol: Protocol::Http2,
        },
        LatencyTarget {
            base_url: origin,
            transport: LatencyTransport::WebSocket,
        },
    )
}

/// Go's preparedFixture: a catalogue server per state, ready without an error; "sign in" needs one.
fn prepare(ui: &mut Ui, states: &[Option<&str>]) {
    ui.prepared = states
        .iter()
        .enumerate()
        .map(|(index, state)| {
            let id = char::from(b'a' + index as u8).to_string();
            let (throughput, latency) = target(&id);
            let ready = state.is_none();
            ServerSummary {
                name: id.to_uppercase(),
                origin: format!("https://{id}.example"),
                throughput: ready.then_some(throughput),
                latency: ready.then_some(latency),
                error: state.map(str::to_owned),
                sign_in: *state == Some("sign in"),
                id,
                ..ServerSummary::default()
            }
        })
        .collect();
    (ui.checked_at, ui.checked_key) = (Some(Instant::now()), Some(ui.config.preparation_key()));
}

/// Go's runModel: a run with the bidirectional stage whose servers reported.
fn running(ids: &[&str]) -> Ui {
    let mut ui = setup();
    ui.config.stages.push(Stage::Bidirectional);
    (ui.live, ui.run) = (true, run::Run::new(ui.config.clone()));
    let servers = ids
        .iter()
        .map(|id| {
            let (throughput, latency) = target(id);
            ServerSummary {
                id: (*id).into(),
                name: id.to_uppercase(),
                location: "Somewhere".into(),
                throughput: Some(throughput),
                latency: Some(latency),
                ..ServerSummary::default()
            }
        })
        .collect();
    ui.update(Snapshot {
        phase: Phase::Preparing,
        servers,
        participants: ids.iter().map(|id| (*id).into()).collect(),
        latency_focus: Some(ids[0].into()),
        plan: ui.config.stages.clone(),
        ..Snapshot::default()
    });
    ui
}

fn step(ui: &mut Ui, change: impl FnOnce(&mut Snapshot)) {
    let mut snapshot = ui.snapshot.clone();
    change(&mut snapshot);
    ui.update(snapshot);
}

/// The run moves to a stage's phase.
fn stage(ui: &mut Ui, stage: Stage, phase: Phase) {
    step(ui, |snapshot| (snapshot.stage, snapshot.phase) = (Some(stage), phase));
}

/// The run fails with an error.
fn fail(ui: &mut Ui, error: &str) {
    step(ui, |snapshot| {
        (snapshot.phase, snapshot.error) = (Phase::Failed, Some(error.into()))
    });
}

/// A sample `quarters` quarter seconds in: the download rate, and server a's last reply and timeout streak.
fn sample(ui: &mut Ui, quarters: u64, down_bps: Option<f64>, latest_ms: Option<f64>, timeouts: u32) {
    let elapsed = Duration::from_millis(250 * quarters);
    step(ui, |snapshot| {
        snapshot.latest = Point {
            elapsed,
            down_bps,
            sample_count: 1,
            ..Point::default()
        };
        snapshot.server_latencies = vec![ServerLatency {
            id: "a".into(),
            latest_ms,
            timeouts,
            steps: vec![(Instant::now(), latest_ms.unwrap_or(f64::NAN), 1)],
        }];
    });
}

fn at(ui: &mut Ui, setting: Setting) {
    ui.row = ui.rows().iter().position(|row| *row == setting).unwrap();
}

pub(crate) fn result(stage: Stage, down: Option<f64>, up: Option<f64>) -> StageResult {
    let measurement = |direction, mean| MeasurementResult {
        direction,
        mean_bytes_per_sec: mean,
        ..download_measurement()
    };
    StageResult {
        stage,
        elapsed: Duration::from_secs(1),
        down: down.map(|mean| measurement(Direction::Down, Some(mean))),
        up: up.map(|mean| measurement(Direction::Up, Some(mean))),
        ..StageResult::default()
    }
}

fn report(ui: &Ui) -> String {
    crate::report::render(&ui.snapshot, 100, Theme::default()).unwrap()
}

fn prompt(code: &str, url: &str) -> AuthPrompt {
    AuthPrompt {
        deadline: Instant::now() + crate::net::AUTHORIZATION_TIMEOUT,
        origin: "https://meter.example".into(),
        code: code.into(),
        browser_url: url.into(),
    }
}

/// The shown run's views as plain text, for Go's view tests.
impl Ui {
    fn live_text(&self, width: usize, height: usize) -> String {
        let live = self
            .shown()
            .map(|(snapshot, run)| self.live_view(snapshot, run, width, height));
        plain(&live.unwrap_or_default())
    }
}

#[test]
fn setup_keys_change_paths_and_debounce_the_check() {
    let (commands, mut sent) = mpsc::channel(8);
    let mut ui = setup();
    press(&mut ui, &commands, &["up"]);
    assert_eq!(ui.current(), Setting::Start);
    ui.advanced = true;
    at(&mut ui, Setting::Protocol);
    press(&mut ui, &commands, &["right"]);
    assert_eq!(ui.config.throughput_protocol, Some(Protocol::Http1));
    assert!(!ui.recheck(&commands));
    ui.recheck = Some(Instant::now());
    assert!(ui.recheck(&commands));
    assert!(matches!(sent.try_recv(), Ok(Command::Verify(_))));
    at(&mut ui, Setting::Reset);
    press(&mut ui, &commands, &["enter"]);
    assert!(ui.reset_prompt);
    press(&mut ui, &commands, &["enter"]);
    assert_eq!(ui.config, Config::default());
    press(&mut ui, &commands, &["r"]);
    assert!(matches!(sent.try_recv(), Ok(Command::Run(_))));
}

#[test]
fn edits_apply_refuse_and_discard() {
    let (commands, _sent) = mpsc::channel(8);
    let mut ui = setup();
    ui.config.servers = vec!["near".into()];
    ui.begin_edit(Setting::Catalogue, "meter.example".into());
    press(&mut ui, &commands, &["enter"]);
    assert_eq!(ui.config.url, "https://meter.example");
    assert!(ui.config.servers.is_empty() && ui.recheck.is_some());
    ui.begin_edit(Setting::Warmup, "5s".into());
    press(&mut ui, &commands, &["enter"]);
    assert!(!ui.edit.as_ref().unwrap().error.is_empty());
    press(&mut ui, &commands, &["backspace"]);
    assert!(ui.edit.as_ref().unwrap().error.is_empty());
    press(&mut ui, &commands, &["esc"]);
    assert!(ui.edit.is_none());
    ui.begin_edit(Setting::Catalogue, "界".repeat(30));
    let edit = ui.edit.as_ref().unwrap().view(&ui, 12);
    assert!(edit.width() <= 12);
    assert!(edit.spans.iter().any(|span| {
        span.style
            .add_modifier
            .contains(ratatui_core::style::Modifier::REVERSED)
    }));
    assert!(press(&mut ui, &commands, &["ctrl+c"]));
}

#[test]
fn available_servers_and_chooser_recheck_the_selection() {
    let (commands, _sent) = mpsc::channel(8);
    let mut ui = setup();
    prepare(&mut ui, &[None, Some("sign in"), Some("connection refused")]);
    assert!(ui.can_use_available());
    press(&mut ui, &commands, &["u"]);
    assert_eq!(ui.config.servers, ["a"]);
    ui.recheck = None;
    ui.checked_at = Some(Instant::now() - FRESHNESS - Duration::from_secs(1));
    assert!(screen(&mut ui).contains("Recheck needed"));
    prepare(&mut ui, &[None, None]);
    press(&mut ui, &commands, &["s", "down", "space", "enter"]);
    assert_eq!(ui.config.servers, ["a", "b"]);
    assert!(ui.recheck.is_some());
}

#[test]
fn a_late_background_answer_never_becomes_keys() {
    let (commands, mut sent) = mpsc::channel(32);
    let mut ui = setup();
    let answer = "11;rgb:ffff/ffff/ffff".split("").filter(|part| !part.is_empty());
    let answer: Vec<_> = std::iter::once("alt+]").chain(answer).chain(["alt+\\"]).collect();
    assert!(!press(&mut ui, &commands, &answer));
    assert!(sent.try_recv().is_err() && !ui.live && ui.popup == Popup::None && !ui.help); // the answer runs no keys
    assert_eq!(crate::theme::DARK.get(), Some(&false)); // the light answer sets the palette
    press(&mut ui, &commands, &["alt+]", "1", "1", "ctrl+g", "r"]);
    assert!(matches!(sent.try_recv(), Ok(Command::Run(_)))); // keys after the answer work
}

#[test]
fn sign_in_opens_cancels_and_expires_as_go_does() {
    let (commands, mut sent) = mpsc::channel(32);
    let mut ui = setup();
    let url = "https://meter.example/auth/cli";
    step(&mut ui, |snapshot| {
        (snapshot.phase, snapshot.auth) = (Phase::Checking, Some(prompt("ABCD", url)))
    });
    assert_eq!(ui.notice, "Check the code, then press enter to open the sign-in page.");
    press(&mut ui, &commands, &["enter", "space", "o", "enter"]);
    let opened = std::iter::from_fn(|| sent.try_recv().ok()).filter(|command| matches!(command, Command::OpenBrowser));
    assert_eq!(opened.count(), 4, "every press opens the page");
    assert!(ui.edit.is_none() && ui.opened && ui.status_label() == "Checking sign-in");
    assert!(ui.short_help().iter().all(|binding| binding.desc != CHANGE.desc));
    let screen = rows(&mut ui);
    for want in ["Sign in to http", "Match this code │ ABCD │", "Waiting for approval…"] {
        assert!(screen.iter().any(|row| row.contains(want)), "{want}: {screen:#?}");
    }
    assert!(screen.contains(&format!(" {url:<119}"))); // the link sits outside a frame
    press(&mut ui, &commands, &["esc"]);
    assert!(matches!(sent.try_recv(), Ok(Command::Cancel)));
    step(&mut ui, |snapshot| {
        (snapshot.auth, snapshot.phase) = (None, Phase::Setup)
    });
    assert_eq!(ui.status_label(), "Sign in");
    assert!(ui.notice.contains('v'), "{}", ui.notice);
    press(&mut ui, &commands, &["r"]);
    assert!(!ui.live && ui.notice == "Test cannot start: sign in first. Press v to request a new code.");
    // An expired approval asks for a new code.
    ui.recheck_soon();
    ui.recheck = None;
    step(&mut ui, |snapshot| {
        (snapshot.auth, snapshot.phase) = (Some(prompt("EFGH", url)), Phase::Checking)
    });
    step(&mut ui, |snapshot| {
        (snapshot.auth, snapshot.phase, snapshot.error) = (None, Phase::Failed, Some(SIGN_IN_EXPIRED.into()));
    });
    assert_eq!(ui.status_label(), "Sign in");
    assert!(ui.notice.contains("expired"), "{}", ui.notice);
    // An approval that succeeds checks the paths again.
    let mut ui = setup();
    step(&mut ui, |snapshot| {
        (snapshot.auth, snapshot.phase) = (Some(prompt("ABCD", url)), Phase::Checking)
    });
    step(&mut ui, |snapshot| snapshot.auth = None);
    assert_eq!(ui.notice, "Signed in. Checking the authenticated paths…");
    // Escaping a sign-in returns to setup with the cancel notice, in setup or in a run.
    for (live, phase, ended) in [
        (false, Phase::Checking, Phase::Setup),
        (true, Phase::Preparing, Phase::Cancelled),
    ] {
        let mut ui = setup();
        ui.live = live;
        step(&mut ui, |snapshot| {
            (snapshot.phase, snapshot.auth) = (phase, Some(prompt("782411", url)))
        });
        press(&mut ui, &commands, &["esc"]);
        assert!(matches!(sent.try_recv(), Ok(Command::Cancel)));
        for phase in [phase, ended] {
            step(&mut ui, |snapshot| (snapshot.phase, snapshot.auth) = (phase, None));
            assert_eq!(ui.notice, "Sign-in canceled. Press v to request a new code.");
        }
        assert!(!ui.live);
    }
}

#[test]
fn run_keys_stop_quit_and_return_as_go_does() {
    let (commands, mut sent) = mpsc::channel(32);
    let mut ui = running(&["a"]);
    stage(&mut ui, Stage::Latency, Phase::Measuring);
    press(&mut ui, &commands, &["x", "esc"]);
    assert!(ui.stop_prompt && ui.notice == "Stop the test? esc confirms, any other key continues.");
    press(&mut ui, &commands, &["x"]);
    assert!(!ui.stop_prompt && ui.running() && ui.notice == "Test continues.");
    press(&mut ui, &commands, &["r"]);
    assert!(sent.try_recv().is_err(), "r restarted a running test");
    press(&mut ui, &commands, &["esc", "esc"]);
    assert!(matches!(sent.try_recv(), Ok(Command::Cancel)) && ui.notice == "Stopping the test…");
    // As in Go's event order, the run's end clears the failure its stopped stage records.
    step(&mut ui, |snapshot| {
        snapshot.phase = Phase::Cancelled;
        snapshot.failures.push(ServerFailure {
            scope: FailureScope::Latency,
            reason: FailureReason::InsufficientEvidence,
            ..lost("a", Stage::Latency)
        });
    });
    assert_eq!(ui.notice, "");
    assert!(ui.exit().shown.is_some());
    press(&mut ui, &commands, &["esc"]);
    assert!(!ui.live && ui.prepare() == Prepare::Checking); // esc returns to a freshly checked setup
    assert!(ui.exit().shown.is_none(), "Go prints no report after a return to setup");
    // Run again keeps the last results until the next run starts, and after a failed start, whose check waits for esc.
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| {
        snapshot.results.push(result(Stage::Download, Some(1e6), None));
        snapshot.phase = Phase::Complete;
    });
    while sent.try_recv().is_ok() {}
    press(&mut ui, &commands, &["r"]);
    assert!(matches!(sent.try_recv(), Ok(Command::Run(_))));
    ui.update(Snapshot {
        phase: Phase::Preparing,
        ..Snapshot::default()
    });
    assert!(ui.shown().is_some_and(|(shown, _)| shown.results.len() == 1));
    assert!(ui.status_label() == "Checking paths" && ui.running() && ui.progress() == Some(Progress::Checking));
    ui.update(Snapshot {
        phase: Phase::Failed,
        error: Some("Test could not start: Server could not be reached".into()),
        ..Snapshot::default()
    });
    assert!(ui.shown().is_some_and(|(shown, _)| shown.phase == Phase::Complete)); // a failed start keeps the results
    assert!(ui.notice.starts_with("Test could not start:") && ui.status_label() == "Complete");
    assert!(!screen(&mut ui).lines().last().unwrap_or_default().is_empty());
    ui.recheck = ui.recheck.map(|_| Instant::now()); // a due check's Checking snapshot would bring setup back
    assert!(!ui.recheck(&commands) && sent.try_recv().is_err() && ui.shown().is_some());
    assert!(!press(&mut ui, &commands, &["esc"]) && !ui.live && ui.recheck.is_some()); // esc checks anew
    // A start that replaced the first check keeps it checking; stopped, it checks again, where Go's spins on.
    let mut ui = setup();
    step(&mut ui, |snapshot| snapshot.phase = Phase::Checking);
    press(&mut ui, &commands, &["r"]);
    step(&mut ui, |snapshot| snapshot.phase = Phase::Preparing);
    assert!(ui.prepare() == Prepare::Checking && screen(&mut ui).contains("checking paths"));
    step(&mut ui, |snapshot| snapshot.phase = Phase::Cancelled);
    assert!(!ui.live && ui.recheck.is_some() && ui.notice == "Test stopped before it started.");
}

#[test]
fn multi_server_runs_name_their_latency_server() {
    let (commands, _sent) = mpsc::channel(32);
    let mut ui = running(&["a", "b"]);
    step(&mut ui, |snapshot| {
        let mut latency = result(Stage::Latency, None, None);
        latency.server_latencies = vec![
            latency_result("a", probes(&[10_000_000; 3], 0)),
            latency_result("b", probes(&[90_000_000; 3], 0)),
        ];
        snapshot.results = vec![latency, result(Stage::Download, Some(3e6), None)];
        snapshot.stage = Some(Stage::Download);
        snapshot.failures.push(lost("b", Stage::Download));
    });
    assert_eq!(ui.notice, "B: Connection lost"); // the failure names its server and reason
    step(&mut ui, |snapshot| snapshot.phase = Phase::Complete);
    let shown = screen(&mut ui);
    assert!(shown.contains("latency to A") && shown.contains("10.0 ms"), "{shown}");
    press(&mut ui, &commands, &["l"]);
    let shown = screen(&mut ui);
    assert_eq!(ui.latency_server(), Some("b"));
    assert!(shown.contains("latency to B") && shown.contains("90.0 ms"), "{shown}");
    let report = self::report(&ui);
    // The report follows the run's focus.
    assert!(report.contains("Latency to A"), "{report}");
    assert!(report.contains("10.0 ms"), "{report}");
    press(&mut ui, &commands, &["d"]);
    let details = screen(&mut ui);
    let (all, own) = (details.find("│ All servers "), details.find("│ A "));
    assert_eq!(ui.popup, Popup::Details);
    assert!(all.is_some_and(|all| own.is_some_and(|own| all < own)), "{details}");
    assert!(details.contains("24.00 Mbit/s"), "{details}");
    press(&mut ui, &commands, &["esc"]);
    assert_eq!(ui.popup, Popup::None);
    // The pick follows the run's focus once the picked server leaves.
    let mut ui = running(&["a", "b"]);
    press(&mut ui, &commands, &["l"]);
    for (focus, participants, want) in [("a", &["a", "b"][..], "b"), ("a", &["a"], "a"), ("b", &["b"], "b")] {
        step(&mut ui, |snapshot| {
            snapshot.latency_focus = Some(focus.into());
            snapshot.participants = participants.iter().map(|id| (*id).into()).collect();
        });
        assert_eq!(ui.latency_server(), Some(want), "{focus} with {participants:?}");
    }
}

#[test]
fn remote_errors_cannot_write_terminal_controls() {
    let remote = "closed\x1b]52;c;cHduZWQ=\x07\u{9b}2J\r";
    let mut failed = setup();
    fail(&mut failed, remote);
    let mut partial = setup();
    prepare(&mut partial, &[None, Some(remote)]);
    let mut run = running(&["a"]);
    fail(&mut run, remote);
    let report = crate::report::render(&run.snapshot, 120, Theme::default()).unwrap();
    let mut multi = running(&["a", "b"]);
    step(&mut multi, |snapshot| {
        snapshot.stage = Some(Stage::Download);
        snapshot.failures.push(lost("b", Stage::Download));
        snapshot.servers[1].name = remote.into();
    });
    multi.popup = Popup::Details;
    let views = [
        screen(&mut failed),
        screen(&mut partial),
        screen(&mut run),
        report,
        screen(&mut multi),
    ];
    for (index, view) in views.into_iter().enumerate() {
        assert!(index > 3 || view.contains("closed"), "{view}");
        assert!(!view.contains(['\x07', '\r', '\u{9b}']), "{view:?}");
        assert!(!view.contains("\x1b]"), "{view:?}");
    }
}

#[test]
fn the_live_view_draws_rates_and_round_trips() {
    let mut ui = running(&["a"]);
    stage(&mut ui, Stage::Bidirectional, Phase::Measuring);
    for quarters in [1, 2] {
        sample(&mut ui, quarters, Some(8e6), Some(3.0), 0);
        std::thread::sleep(Duration::from_millis(60));
    }
    let shown = ui.live_text(60, 16);
    for text in ["↓", "↑", "Loaded latency", "3.0 ms", "solid", "dashed"] {
        assert!(shown.contains(text), "{shown}");
    }
    assert!(shown.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)), "{shown}");
}

#[test]
fn representative_views_fit_the_terminal() {
    let mut small = setup();
    small.size = (30, 10);
    assert!(screen(&mut small).contains("Enlarge the terminal"));
    let mut setup = setup();
    setup.size = (40, 12);
    setup.begin_edit(Setting::Catalogue, "https://界.example".into());
    let mut run = running(&["a", "b"]);
    run.size = (80, 24);
    run.popup = Popup::Details;
    for (ui, width, height) in [(&setup, 40, 12), (&run, 80, 24)] {
        let frame = ui.screen();
        assert!(frame.len() <= height);
        assert!(frame.iter().all(|line| line.width() <= width));
        assert!(plain(&frame).contains("Graphite Meter"));
    }
}

#[test]
fn mouse_scrolling_obeys_the_stop_prompt() {
    let mut ui = running(&["a"]);
    ui.size = (80, 12);
    step(&mut ui, |snapshot| {
        snapshot.results.push(result(Stage::Download, Some(1e6), None));
        snapshot.phase = Phase::Complete;
    });
    assert!(ui.mouse(MouseEventKind::ScrollDown));
    assert!(ui.mouse(MouseEventKind::ScrollUp));
    assert_eq!(ui.body, 0);
    ui.stop_prompt = true;
    assert!(!ui.mouse(MouseEventKind::ScrollDown));
}
