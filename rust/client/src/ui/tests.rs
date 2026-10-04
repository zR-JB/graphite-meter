//! Go's TUI tests (main_test.go and run_test.go) over the controller's snapshots.
use super::*;
use crate::model::{AuthPrompt, FailureScope, ServerFailure};
use graphite_meter_core::{
    discovery::{LatencyTarget, LatencyTransport, Protocol, ThroughputTarget, ThroughputTransport},
    failure::FailureReason,
};
use ratatui_core::{backend::TestBackend, terminal::Terminal};

fn key(name: &str) -> KeyEvent {
    let (modifiers, base) = match name.split_once('+') {
        Some(("ctrl", base)) => (KeyModifiers::CONTROL, base),
        Some(("alt", base)) => (KeyModifiers::ALT, base),
        _ => (KeyModifiers::NONE, name),
    };
    let named = NAMED.iter().find(|(_, named)| *named == base);
    KeyEvent::new(named.map_or(KeyCode::Char(base.chars().next().unwrap()), |(code, _)| *code), modifiers)
}

fn press(ui: &mut Ui, commands: &mpsc::Sender<Command>, names: &[&str]) -> bool {
    names.iter().any(|name| ui.key(key(name), commands))
}

/// The screen `ui` draws, a line per row.
fn screen(ui: &mut Ui) -> String {
    let (width, height) = ui.size;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    let cells = terminal.backend().buffer().content().chunks(usize::from(width));
    let rows: Vec<String> = cells
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect();
    rows.join("\n")
}

/// Go's testModel: setup at 120×40 with its paths ready.
fn setup() -> Ui {
    let mut ui = Ui::new(Config::default(), Snapshot::default());
    (ui.size, ui.check_started) = ((120, 40), None);
    ui
}

/// A catalogue server whose check reached both its paths.
fn server(id: &str) -> ServerSummary {
    let origin = format!("https://{id}.example");
    let throughput = ThroughputTarget {
        base_url: origin.clone(),
        transport: ThroughputTransport::FetchStream,
        protocol: Protocol::Http2,
    };
    let latency = LatencyTarget {
        base_url: origin.clone(),
        transport: LatencyTransport::WebSocket,
    };
    ServerSummary {
        id: id.into(),
        name: id.to_uppercase(),
        origin,
        throughput: Some(throughput),
        latency: Some(latency),
        ..ServerSummary::default()
    }
}

/// Go's preparedFixture: a catalogue server per state, ready without an error; "sign in" needs one.
fn prepare(ui: &mut Ui, states: &[Option<&str>]) {
    let prepared = states.iter().zip('a'..).map(|(state, id)| {
        let (server, ready) = (server(&id.to_string()), state.is_none());
        ServerSummary {
            throughput: server.throughput.filter(|_| ready),
            latency: server.latency.filter(|_| ready),
            error: state.map(str::to_owned),
            sign_in: *state == Some("sign in"),
            ..server
        }
    });
    ui.prepared = prepared.collect();
    (ui.checked_at, ui.checked_key) = (Some(Instant::now()), Some(ui.config.preparation_key()));
}

/// Go's runModel: a run with the bidirectional stage whose servers reported.
fn running(ids: &[&str]) -> Ui {
    let mut ui = setup();
    ui.config.stages.push(Stage::Bidirectional);
    (ui.live, ui.run) = (true, run::Run::new(ui.config.clone()));
    let located = |id: &&str| ServerSummary { location: "Somewhere".into(), ..server(id) };
    ui.update(Snapshot {
        phase: Phase::Preparing,
        servers: ids.iter().map(located).collect(),
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

/// The run fails with an error.
fn fail(ui: &mut Ui, error: &str) {
    step(ui, |snapshot| (snapshot.phase, snapshot.error) = (Phase::Failed, Some(error.into())));
}

/// A server's lost connection in a stage's throughput.
fn lost(id: &str, stage: Stage) -> ServerFailure {
    ServerFailure {
        server_id: id.into(),
        stage,
        scope: FailureScope::Throughput,
        reason: FailureReason::ConnectionLost,
        at: Duration::ZERO,
    }
}

const SIGN_IN_URL: &str = "https://meter.example/auth/cli";

/// The controller moves to `phase`, showing a sign-in code or none.
fn sign_in(ui: &mut Ui, phase: Phase, code: Option<&str>) {
    let auth = code.map(|code| AuthPrompt {
        deadline: Instant::now() + crate::net::AUTHORIZATION_TIMEOUT,
        code: code.into(),
        browser_url: SIGN_IN_URL.into(),
    });
    step(ui, |snapshot| (snapshot.phase, snapshot.auth) = (phase, auth));
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
    sign_in(&mut ui, Phase::Checking, Some("ABCD"));
    assert_eq!(ui.notice, "Check the code, then press enter to open the sign-in page.");
    press(&mut ui, &commands, &["enter", "space", "o", "enter"]);
    let opened = std::iter::from_fn(|| sent.try_recv().ok()).filter(|command| matches!(command, Command::OpenBrowser));
    assert_eq!(opened.count(), 4, "every press opens the page");
    assert!(ui.edit.is_none() && ui.opened && ui.status_label() == "Checking sign-in");
    assert!(ui.short_help().iter().all(|binding| binding.desc != CHANGE.desc));
    let screen = screen(&mut ui);
    for want in ["Sign in to http", "Match this code │ ABCD │", "Waiting for approval…"] {
        assert!(screen.contains(want), "{want}: {screen}");
    }
    assert!(screen.contains(&format!("\n {SIGN_IN_URL:<119}\n"))); // the link sits outside a frame
    press(&mut ui, &commands, &["esc"]);
    assert!(matches!(sent.try_recv(), Ok(Command::Cancel)));
    sign_in(&mut ui, Phase::Setup, None);
    assert_eq!(ui.status_label(), "Sign in");
    assert!(ui.notice.contains('v'), "{}", ui.notice);
    press(&mut ui, &commands, &["r"]);
    assert!(!ui.live && ui.notice == "Test cannot start: sign in first. Press v to request a new code.");
    // An expired approval asks for a new code.
    ui.recheck_soon();
    ui.recheck = None;
    sign_in(&mut ui, Phase::Checking, Some("EFGH"));
    step(&mut ui, |snapshot| {
        (snapshot.auth, snapshot.phase, snapshot.error) = (None, Phase::Failed, Some(SIGN_IN_EXPIRED.into()));
    });
    assert_eq!(ui.status_label(), "Sign in");
    assert!(ui.notice.contains("expired"), "{}", ui.notice);
    // An approval that succeeds checks the paths again.
    let mut ui = setup();
    sign_in(&mut ui, Phase::Checking, Some("ABCD"));
    sign_in(&mut ui, Phase::Checking, None);
    assert_eq!(ui.notice, "Signed in. Checking the authenticated paths…");
    // Escaping a sign-in returns to setup with the cancel notice, in setup or in a run.
    for (live, phase, ended) in [(false, Phase::Checking, Phase::Setup), (true, Phase::Preparing, Phase::Cancelled)] {
        let mut ui = setup();
        ui.live = live;
        sign_in(&mut ui, phase, Some("782411"));
        press(&mut ui, &commands, &["esc"]);
        assert!(matches!(sent.try_recv(), Ok(Command::Cancel)));
        for phase in [phase, ended] {
            sign_in(&mut ui, phase, None);
            assert_eq!(ui.notice, "Sign-in canceled. Press v to request a new code.");
        }
        assert!(!ui.live);
    }
}

#[test]
fn run_keys_stop_quit_and_return_as_go_does() {
    let (commands, mut sent) = mpsc::channel(32);
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| {
        (snapshot.stage, snapshot.phase) = (Some(Stage::Latency), Phase::Measuring)
    });
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
    assert!(!ui.live && ui.prepare() == PathState::Checking); // esc returns to a freshly checked setup
    assert!(ui.exit().shown.is_none(), "Go prints no report after a return to setup");
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
    let views = [screen(&mut failed), screen(&mut partial), screen(&mut run), report, screen(&mut multi)];
    for (index, view) in views.into_iter().enumerate() {
        assert!(index > 3 || view.contains("closed"), "{view}");
        assert!(!view.contains(['\x07', '\r', '\u{9b}']), "{view:?}");
        assert!(!view.contains("\x1b]"), "{view:?}");
    }
}
