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

fn prompt(code: &str, url: &str) -> AuthPrompt {
    AuthPrompt {
        deadline: Instant::now() + crate::net::AUTHORIZATION_TIMEOUT,
        origin: "https://meter.example".into(),
        code: code.into(),
        browser_url: url.into(),
    }
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
