//! Go's TUI tests (main_test.go and run_test.go) over the controller's snapshots.
use super::*;
use crate::model::{AuthPrompt, FailureScope, ServerFailure, ServerLatencyResult, StageResult};
use graphite_meter_core::{
    discovery::{Capabilities, LatencyTarget, LatencyTransport, Protocol, ThroughputTarget, ThroughputTransport},
    failure::FailureReason,
    latency::{LatencyAccumulator, LatencySummary, ProbeOutcome},
    measurement::{Direction, MeasurementResult},
};
use ratatui::{Terminal, backend::TestBackend};

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
    let code = match name {
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
        "ctrl+c" => return KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        "ctrl+g" => return KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
        "alt+]" => return KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT),
        "alt+\\" => return KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::ALT),
        name => KeyCode::Char(name.chars().next().unwrap()),
    };
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn press(ui: &mut Ui, commands: &mpsc::Sender<Command>, names: &[&str]) -> bool {
    names.iter().any(|name| ui.key(key(name), commands))
}

fn channel() -> (mpsc::Sender<Command>, mpsc::Receiver<Command>) {
    mpsc::channel(32)
}

/// The screen `ui` draws, one string per row.
fn rows(ui: &mut Ui) -> Vec<String> {
    let (width, height) = ui.size;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    buffer
        .content()
        .chunks(usize::from(width))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect()
}

fn screen(ui: &mut Ui) -> String {
    rows(ui).join("\n")
}

fn plain(lines: &[ratatui::text::Line]) -> String {
    lines.iter().map(crate::report::plain).collect::<Vec<_>>().join("\n")
}

/// Go's testModel: setup at 120×40 with its paths ready.
fn setup() -> Ui {
    let mut ui = Ui::new(Config::default(), Snapshot::default());
    ui.size = (120, 40);
    ui.check_started = None;
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
            let (mut throughput, mut latency) = target(id);
            (throughput.base_url, latency.base_url) = (format!("https://{id}"), format!("https://{id}"));
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

fn at(ui: &mut Ui, setting: Setting) {
    ui.row = ui.rows().iter().position(|row| *row == setting).unwrap();
}

fn result(stage: Stage, down: Option<f64>, up: Option<f64>) -> StageResult {
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
    fn details_text(&self, width: usize) -> String {
        let Some((snapshot, _)) = self.shown() else {
            return String::new();
        };
        plain(&crate::report::Report::new(snapshot, self.latency_server(), width, self.theme).details(true))
    }

    fn track_text(&self, width: usize) -> String {
        let track = self
            .shown()
            .map(|(snapshot, run)| self.stage_track(snapshot, run, width));
        plain(&track.unwrap_or_default())
    }

    fn live_text(&self, width: usize, height: usize) -> String {
        let live = self
            .shown()
            .map(|(snapshot, run)| self.live_view(snapshot, run, width, height));
        plain(&live.unwrap_or_default())
    }
}

#[test]
fn arrow_keys_move_rows_and_change_values() {
    let (commands, mut sent) = channel();
    let mut ui = setup();
    let defaults = Config::default();
    for (name, want) in [
        ("up", Setting::Start),
        ("down", Setting::Catalogue),
        ("tab", Setting::Servers),
        ("shift+tab", Setting::Catalogue),
    ] {
        press(&mut ui, &commands, &[name]);
        assert_eq!(ui.current(), want, "after {name}");
    }
    press(&mut ui, &commands, &["down"; 20]);
    assert_eq!(ui.current(), Setting::Advanced, "the collapsed list ends at Advanced");
    press(&mut ui, &commands, &["right", "down", "right"]);
    assert!(ui.advanced && ui.config.warmup == defaults.warmup + Duration::from_millis(100));
    at(&mut ui, Setting::Stage(Stage::Download));
    press(&mut ui, &commands, &["right", "space"]);
    assert_eq!(
        ui.config.download_duration,
        defaults.download_duration + Duration::from_secs(1)
    );
    assert!(!ui.config.stages.contains(&Stage::Download));
    at(&mut ui, Setting::Cadence(false));
    press(&mut ui, &commands, &["left"]);
    assert_eq!(
        ui.config.ping_interval,
        Duration::from_millis(600),
        "left from reply-driven"
    );
    at(&mut ui, Setting::Stage(Stage::Download));
    press(&mut ui, &commands, &["enter"]);
    assert!(
        ui.edit
            .as_ref()
            .is_some_and(|edit| edit.setting == Setting::Stage(Stage::Download))
    );
    ui.edit = None;
    at(&mut ui, Setting::Start);
    assert!(
        screen(&mut ui).contains("enter start test"),
        "the start row offers enter"
    );
    press(&mut ui, &commands, &["enter"]);
    assert!(
        matches!(sent.try_recv(), Ok(Command::Run(_))),
        "enter on Start test starts"
    );
}

#[test]
fn rows_activate_and_recheck_as_go_does() {
    let stage = |stage| Setting::Stage(stage);
    for (row, name, recheck) in [
        (stage(Stage::Upload), "space", true),
        (stage(Stage::Bidirectional), "space", false),
        (Setting::LoadedLatency, "enter", false),
        (stage(Stage::Download), "enter", false),
        (Setting::Cadence(false), "enter", true),
        (Setting::Cadence(true), "enter", true),
        (Setting::ForceStreams, "enter", true),
        (Setting::Insecure, "enter", true),
        (Setting::Catalogue, "enter", false),
        (Setting::Advanced, "enter", false),
    ] {
        let (commands, _sent) = channel();
        let mut ui = setup();
        ui.advanced = true;
        at(&mut ui, row);
        press(&mut ui, &commands, &[name]);
        let config = &ui.config;
        let applied = match row {
            Setting::Stage(Stage::Upload) => !config.stages.contains(&Stage::Upload),
            Setting::Stage(Stage::Bidirectional) => config.stages.contains(&Stage::Bidirectional),
            Setting::LoadedLatency => !config.loaded_latency,
            Setting::Cadence(false) => config.ping_interval == Duration::from_millis(80),
            Setting::Cadence(true) => config.loaded_ping_interval == Duration::from_millis(600),
            Setting::ForceStreams => config.streams == 6,
            Setting::Insecure => config.insecure,
            Setting::Advanced => !ui.advanced,
            row => ui.edit.as_ref().is_some_and(|edit| edit.setting == row),
        };
        assert!(applied, "{row:?}");
        assert_eq!(ui.recheck.is_some(), recheck, "{row:?}");
    }
}

#[test]
fn edits_commit_as_go_parses_them() {
    let download = Setting::Stage(Stage::Download);
    let upload = Setting::Stage(Stage::Upload);
    for (row, forced, typed, expected) in [
        (
            Setting::Catalogue,
            false,
            "meter.example:8443/",
            Some("https://meter.example:8443"),
        ),
        (
            Setting::Catalogue,
            false,
            "127.0.0.1:7247",
            Some("http://127.0.0.1:7247"),
        ),
        (
            Setting::Catalogue,
            false,
            "https://METER.example",
            Some("https://meter.example"),
        ),
        (Setting::Catalogue, false, "ftp://meter.example", None),
        (Setting::Warmup, false, "0", Some("0s")),
        (download, false, "12", Some("12s")),
        (download, false, "1.5m", Some("1m30s")),
        (upload, false, "0", None),
        (upload, false, "6m", None),
        (Setting::Warmup, false, "5s", None),
        (upload, false, "soon", None),
        (Setting::Streams, false, "8", Some("8 0")),
        (Setting::Streams, true, "9", Some("6 9")),
        (Setting::Streams, false, "15", None),
    ] {
        let (commands, _sent) = channel();
        let mut ui = setup();
        if forced {
            ui.config.streams = 1;
        }
        ui.begin_edit(row, String::new());
        for character in typed.chars() {
            ui.key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE), &commands);
        }
        press(&mut ui, &commands, &["enter"]);
        let Some(expected) = expected else {
            let edit = ui.edit.as_ref().expect("the edit stays open");
            assert!(!edit.error.is_empty() && edit.text() == typed, "{typed}");
            assert_eq!(ui.notice, edit.error);
            continue;
        };
        assert!(ui.edit.is_none(), "{typed}");
        let config = &ui.config;
        let got = match row {
            Setting::Catalogue => config.url.clone(),
            Setting::Warmup => setup::go_duration(config.warmup),
            Setting::Stage(Stage::Download) => setup::go_duration(config.download_duration),
            _ => format!("{} {}", config.auto_streams, config.streams),
        };
        assert_eq!(got, expected, "{typed}");
    }
}

#[test]
fn edit_errors_read_as_go_writes_them() {
    let (commands, _sent) = channel();
    let mut ui = setup();
    for (row, typed, error) in [
        (Setting::Streams, "x", "streams must be a whole number from 1 to 14"),
        (Setting::Stage(Stage::Upload), "0", "Upload must be from 1 s to 300 s"),
        (Setting::Warmup, "5s", "Warmup must be from 0 s to 4 s"),
        (
            Setting::Stage(Stage::Upload),
            "soon",
            "use a duration like 800ms, 4s, or 1m; a bare number is seconds",
        ),
        (
            Setting::Catalogue,
            "ftp://x",
            "use an http:// or https:// origin, for example https://meter.example",
        ),
    ] {
        ui.begin_edit(row, typed.into());
        press(&mut ui, &commands, &["enter"]);
        assert_eq!(ui.edit.as_ref().map(|edit| edit.error.as_str()), Some(error));
        // Go shows the error in the footer's error style and clears it as you type.
        assert!(screen(&mut ui).contains(error));
        press(&mut ui, &commands, &["backspace"]);
        assert!(ui.edit.as_ref().is_some_and(|edit| edit.error.is_empty()));
        press(&mut ui, &commands, &["esc"]);
        assert_eq!(ui.notice, "Edit canceled.");
    }
}

#[test]
fn an_unchanged_catalogue_keeps_its_servers() {
    let (commands, _sent) = channel();
    let mut ui = setup();
    ui.config.servers = vec!["near".into()];
    ui.begin_edit(Setting::Catalogue, "HTTP://127.0.0.1:7246/".into());
    press(&mut ui, &commands, &["enter"]);
    assert_eq!(
        (ui.config.servers.len(), ui.notice.as_str()),
        (1, "Catalogue http://127.0.0.1:7246.")
    );
    ui.begin_edit(Setting::Catalogue, "meter.example".into());
    press(&mut ui, &commands, &["enter"]);
    assert!(ui.config.servers.is_empty() && ui.recheck.is_some());
}

#[test]
fn edit_keys_discard_and_quit() {
    let (commands, _sent) = channel();
    let mut ui = setup();
    ui.begin_edit(Setting::Catalogue, ui.config.url.clone());
    press(&mut ui, &commands, &["x", "esc"]);
    assert!(
        ui.edit.is_none() && ui.config.url == Config::default().url,
        "esc applied the edit"
    );
    ui.begin_edit(Setting::Catalogue, String::new());
    assert!(!press(&mut ui, &commands, &["q"]) && ui.edit.as_ref().unwrap().text() == "q");
    for (keys, text) in [
        (&["ctrl+a"][..], "q"),
        (&["end", "x", "left", "ctrl+k"], "q"),
        (&["x", "left", "ctrl+u"], "x"),
    ] {
        let edit = ui.edit.as_mut().unwrap();
        for name in keys {
            edit.key(name, name.chars().next().filter(|_| name.len() == 1));
        }
        assert_eq!(edit.text(), text, "{keys:?}");
    }
    let edit = ui.edit.as_mut().unwrap();
    edit.key("ctrl+u", None);
    edit.key("ctrl+k", None);
    edit.insert("one two");
    edit.key("ctrl+w", None);
    assert_eq!(edit.text(), "one ");
    edit.key("alt+b", None);
    edit.key("alt+d", None);
    assert_eq!(edit.text(), " ");
    assert!(press(&mut ui, &commands, &["ctrl+c"]), "ctrl+c quits from the editor");
}

#[test]
fn sign_in_keys_own_enter_and_the_link_stands_alone() {
    let (commands, mut sent) = channel();
    let mut ui = setup();
    let url = "https://meter.example/auth/cli";
    ui.update(Snapshot {
        phase: Phase::Checking,
        auth: Some(prompt("ABCD", url)),
        ..Snapshot::default()
    });
    assert_eq!(ui.notice, "Check the code, then press enter to open the sign-in page.");
    press(&mut ui, &commands, &["enter", "space", "o", "enter"]);
    let opened = std::iter::from_fn(|| sent.try_recv().ok()).filter(|command| matches!(command, Command::OpenBrowser));
    assert_eq!(opened.count(), 4, "every press opens the page");
    assert!(ui.edit.is_none() && ui.opened);
    assert_eq!(ui.status_label(), "Checking sign-in");
    assert!(ui.short_help().iter().all(|binding| binding.desc != CHANGE.desc));
    let screen = rows(&mut ui);
    assert!(screen.iter().any(
        |row| row.contains("Sign in to https://127.0.0.1:7246") || row.contains("Sign in to http://127.0.0.1:7246")
    ));
    assert!(
        screen.iter().any(|row| row.contains("Match this code │ ABCD │")),
        "{screen:#?}"
    );
    assert!(
        screen.contains(&format!(" {url:<119}")),
        "the link sits inside a frame: {screen:#?}"
    );
    assert!(screen.iter().any(|row| row.contains("Waiting for approval…")));
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
}

#[test]
fn an_approval_that_succeeds_checks_the_paths_again() {
    let mut ui = setup();
    step(&mut ui, |snapshot| {
        (snapshot.auth, snapshot.phase) = (Some(prompt("ABCD", "https://x/")), Phase::Checking)
    });
    step(&mut ui, |snapshot| snapshot.auth = None);
    assert_eq!(ui.notice, "Signed in. Checking the authenticated paths…");
}

#[test]
fn remote_errors_cannot_write_terminal_controls() {
    let remote = "closed\x1b]52;c;cHduZWQ=\x07\u{9b}2J\r";
    let mut failed = setup();
    step(&mut failed, |snapshot| {
        (snapshot.phase, snapshot.error) = (Phase::Failed, Some(remote.into()))
    });
    let mut partial = setup();
    prepare(&mut partial, &[None, Some(remote)]);
    let mut run = running(&["a"]);
    step(&mut run, |snapshot| {
        (snapshot.phase, snapshot.error) = (Phase::Failed, Some(remote.into()))
    });
    let report = crate::report::render(&run.snapshot, 120, Theme::default()).unwrap();
    let mut multi = running(&["a", "b"]);
    step(&mut multi, |snapshot| {
        snapshot.stage = Some(Stage::Download);
        snapshot.failures.push(ServerFailure {
            server_id: "b".into(),
            stage: Stage::Download,
            scope: FailureScope::Throughput,
            reason: FailureReason::ConnectionLost,
            at: Duration::ZERO,
        });
        snapshot.servers[1].name = remote.into();
    });
    multi.popup = Popup::Details;
    for (index, view) in [
        screen(&mut failed),
        screen(&mut partial),
        screen(&mut run),
        report,
        screen(&mut multi),
    ]
    .into_iter()
    .enumerate()
    {
        assert!(index > 3 || view.contains("closed"), "{view}");
        assert!(
            !view.contains(['\x07', '\r', '\u{9b}']) && !view.contains("\x1b]"),
            "{view:?}"
        );
    }
}

#[test]
fn run_keys_stop_after_asking_and_return_to_a_fresh_check() {
    let (commands, mut sent) = channel();
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| {
        (snapshot.phase, snapshot.stage) = (Phase::Measuring, Some(Stage::Latency))
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
            server_id: "a".into(),
            stage: Stage::Latency,
            scope: FailureScope::Latency,
            reason: FailureReason::InsufficientEvidence,
            at: Duration::from_secs(1),
        });
    });
    assert_eq!(ui.notice, "");
    press(&mut ui, &commands, &["esc"]);
    assert!(
        !ui.live && ui.prepare() == Prepare::Checking,
        "esc returns to a freshly checked setup"
    );
}

#[test]
fn quitting_during_a_run_stops_it_and_reports() {
    for name in ["q", "ctrl+c"] {
        let (commands, mut sent) = channel();
        let mut ui = running(&["a"]);
        step(&mut ui, |snapshot| {
            (snapshot.phase, snapshot.stage) = (Phase::Measuring, Some(Stage::Latency))
        });
        assert!(
            !press(&mut ui, &commands, &[name]),
            "{name} quit before the test stopped"
        );
        assert!(ui.quitting && matches!(sent.try_recv(), Ok(Command::Cancel)));
        step(&mut ui, |snapshot| snapshot.phase = Phase::Cancelled);
        let exit = ui.exit();
        assert!(!exit.running && exit.interrupted == (name == "ctrl+c"));
        assert_eq!(
            exit.shown.map(|shown| shown.phase),
            Some(Phase::Cancelled),
            "the stopped run is reported"
        );
    }
    let (commands, _sent) = channel();
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| snapshot.phase = Phase::Measuring);
    assert!(!ui.interrupt(&commands));
    assert!(
        press(&mut ui, &commands, &["ctrl+c"]),
        "a second interrupt quits at once"
    );
    assert!(ui.exit().running && ui.exit().shown.is_none());
}

#[test]
fn a_return_to_setup_leaves_no_report() {
    let (commands, _sent) = channel();
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| snapshot.phase = Phase::Complete);
    assert!(ui.exit().shown.is_some());
    press(&mut ui, &commands, &["esc"]);
    assert!(ui.exit().shown.is_none(), "Go prints no report after a return to setup");
}

#[test]
fn status_labels_follow_the_lifecycle() {
    let mut ui = running(&["a"]);
    for (stage, phase, want) in [
        (Stage::Latency, Phase::Preparing, "Checking paths"),
        (Stage::Download, Phase::Warmup, "Warmup"),
        (Stage::Bidirectional, Phase::Measuring, "Bidirectional"),
    ] {
        step(&mut ui, |snapshot| {
            (snapshot.stage, snapshot.phase) = (Some(stage), phase)
        });
        assert_eq!(ui.status_label(), want);
    }
    for (phase, want) in [
        (Phase::Partial, "Partial"),
        (Phase::Cancelled, "Stopped"),
        (Phase::Failed, "Failed"),
        (Phase::Incomplete, "Incomplete"),
    ] {
        let mut finished = running(&["a"]);
        step(&mut finished, |snapshot| snapshot.phase = phase);
        assert_eq!(finished.status_label(), want);
    }
    let mut setup = setup();
    assert_eq!(setup.status_label(), "Not started");
    step(&mut setup, |snapshot| snapshot.phase = Phase::Checking);
    assert_eq!(setup.status_label(), "Not started");
    step(&mut setup, |snapshot| {
        (snapshot.phase, snapshot.error) = (Phase::Failed, Some("refused".into()))
    });
    assert_eq!(setup.status_label(), "Test could not start");
    setup.signed_out = true;
    assert_eq!(setup.status_label(), "Sign in");
    setup.config.stages.clear();
    assert_eq!(setup.status_label(), "Test cannot start");
}

#[test]
fn a_failed_stage_reads_its_reason_and_details_keep_the_facts() {
    let mut ui = running(&["a"]);
    let lost = |stage| ServerFailure {
        server_id: "a".into(),
        stage,
        scope: FailureScope::Throughput,
        reason: FailureReason::ConnectionLost,
        at: Duration::ZERO,
    };
    step(&mut ui, |snapshot| {
        snapshot.stage = Some(Stage::Bidirectional);
        let mut bidirectional = result(Stage::Bidirectional, None, None);
        bidirectional.down = Some(MeasurementResult {
            mean_bytes_per_sec: None,
            total_bytes: 42,
            ..download_measurement()
        });
        bidirectional.server_results = vec![crate::model::ServerContribution {
            id: "a".into(),
            ..Default::default()
        }];
        let mut population = latency_result("a", probes(&[], 0));
        population.summary.unresolved = 2;
        population.ending = Some(crate::model::Ending::Failed(FailureReason::ConnectionLost));
        bidirectional.server_latencies = vec![population];
        snapshot.results.push(bidirectional);
        snapshot.failures.push(lost(Stage::Bidirectional));
        snapshot.phase = Phase::Incomplete;
    });
    let screen = screen(&mut ui);
    for want in [
        "Bi-dir ↓: Connection lost",
        "Loaded latency · Bidirectional",
        "Bi-dir ↓",
        "—",
    ] {
        assert!(screen.contains(want), "{want} in {screen}");
    }
    ui.popup = Popup::Details;
    assert!(ui.details_text(116).contains("unfinished probes 2"));
}

#[test]
fn the_report_names_every_population() {
    let mut ui = running(&["a"]);
    let idle = latency_result("a", probes(&[10_000_000; 16], 0));
    let loaded = latency_result("a", probes(&[17_800_000; 40], 2));
    step(&mut ui, |snapshot| {
        let mut download = result(Stage::Download, Some(117_500_000.0), None);
        download.down.as_mut().unwrap().peak_bytes_per_sec = Some(125_000_000.0);
        download.server_latencies = vec![loaded];
        let mut latency = result(Stage::Latency, None, None);
        latency.server_latencies = vec![idle];
        snapshot.results = vec![latency, download, result(Stage::Upload, None, Some(5_000_000.0))];
        snapshot.phase = Phase::Complete;
    });
    let text = crate::report::render(&ui.snapshot, 100, Theme::default()).unwrap();
    for want in [
        "Complete",
        "Idle",
        "Loaded latency · Download",
        "10.0 ms",
        "940.0 Mbit/s",
        "peak 1000 ·",
        "17.8 ms",
        "+7.8 ms",
        "40.00 Mbit/s",
        "Bidirectional",
    ] {
        assert!(text.contains(want), "{want} in {text}");
    }
    assert!(
        !text.to_lowercase().contains("loss") && !text.contains("Idle latency:"),
        "{text}"
    );
    ui.live = false;
    assert!(ui.exit().shown.is_none(), "no report before any run");
}

#[test]
fn the_live_view_follows_the_stage() {
    let mut ui = running(&["a"]);
    for (stage, want, without) in [
        (Stage::Latency, &["Idle latency", "3.0 ms"][..], &["↓", "↑"][..]),
        (Stage::Download, &["↓", "Loaded latency"], &["↑"]),
        (Stage::Bidirectional, &["↓", "↑", "Loaded latency"], &[]),
    ] {
        step(&mut ui, |snapshot| {
            (snapshot.stage, snapshot.phase) = (Some(stage), Phase::Measuring)
        });
        for elapsed in [1, 2] {
            step(&mut ui, |snapshot| {
                snapshot.latest = crate::model::Point {
                    elapsed: Duration::from_millis(250 * elapsed),
                    down_bps: Some(8e6),
                    sample_count: 1,
                    ..Default::default()
                };
                snapshot.server_latencies = vec![crate::model::ServerLatency {
                    id: "a".into(),
                    latest_ms: Some(3.0),
                    ..Default::default()
                }];
            });
            std::thread::sleep(Duration::from_millis(60));
        }
        let live = ui.live_text(60, 16);
        for want in want {
            assert!(live.contains(want), "{stage:?} lost {want}: {live}");
        }
        for unwanted in without {
            assert!(!live.contains(unwanted), "{stage:?} drew {unwanted}: {live}");
        }
        assert!(
            live.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)),
            "{stage:?} drew no chart: {live}"
        );
    }
}

#[test]
fn the_idle_reading_holds_the_last_reply_and_a_new_stage_starts_empty() {
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| {
        (snapshot.stage, snapshot.phase) = (Some(Stage::Latency), Phase::Measuring)
    });
    // Go's TestIdleReadingHoldsTheLastReplyThroughTimeouts: a reply ends the timeout streak.
    for (elapsed, latest, timeouts, reading) in [
        (1, Some(12.0), 0, "Idle latency 12.0 ms"),
        (2, None, 2, "Idle latency 12.0 ms  probe timeout ×2"),
        (3, Some(12.0), 0, "Idle latency 12.0 ms"),
    ] {
        step(&mut ui, |snapshot| {
            snapshot.latest = crate::model::Point {
                elapsed: Duration::from_millis(250 * elapsed),
                sample_count: 1,
                ..Default::default()
            };
            snapshot.server_latencies = vec![crate::model::ServerLatency {
                id: "a".into(),
                latest_ms: latest,
                timeouts,
            }];
        });
        let live = ui.live_text(60, 16);
        assert!(
            live.contains(reading) && (timeouts > 0 || !live.contains("probe timeout")),
            "{live}"
        );
    }
    // A stage opens with no samples, as Snapshot::open_stage starts one.
    step(&mut ui, |snapshot| {
        snapshot.open_stage(Stage::Download, ["a".to_owned()].into_iter());
    });
    step(&mut ui, |snapshot| {
        (snapshot.stage, snapshot.phase) = (Some(Stage::Latency), Phase::Measuring)
    });
    let live = ui.live_text(60, 16);
    assert!(live.contains("Idle latency —") && !live.contains("12.0 ms"), "{live}");
}

#[test]
fn the_stage_track_follows_the_stages() {
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| {
        let mut latency = result(Stage::Latency, None, None);
        latency.server_latencies = vec![latency_result("a", probes(&[4_000_000; 3], 0))];
        snapshot.results.push(latency);
        (snapshot.stage, snapshot.phase) = (Some(Stage::Download), Phase::Measuring);
    });
    ui.run.since = Some(Instant::now() - Duration::from_millis(2500));
    let track = ui.track_text(80);
    for want in ["✓ 4.0 ms median", "2.5 s / 10 s", "○ 10 s"] {
        assert!(track.contains(want), "{want} in {track}");
    }
    step(&mut ui, |snapshot| {
        snapshot.results.push(result(Stage::Download, None, None));
        let mut upload = result(Stage::Upload, None, Some(1e6));
        upload.stage = Stage::Upload;
        snapshot.results.push(upload);
        snapshot.failures.push(ServerFailure {
            server_id: "a".into(),
            stage: Stage::Upload,
            scope: FailureScope::Latency,
            reason: FailureReason::Timeout,
            at: Duration::ZERO,
        });
        snapshot.results[1].down = Some(MeasurementResult {
            mean_bytes_per_sec: None,
            ..download_measurement()
        });
    });
    let track = ui.track_text(80);
    for want in [
        "✓ 4.0 ms median",
        "Download      ✗ Failed",
        "Upload        ! ↑ 8.00 Mbit/s Partial",
    ] {
        assert!(track.contains(want), "{want} in {track}");
    }
    step(&mut ui, |snapshot| snapshot.phase = Phase::Cancelled);
    assert!(
        ui.track_text(80).contains("Bidirectional — Skipped"),
        "{}",
        ui.track_text(80)
    );
}

#[test]
fn multi_server_runs_name_their_latency_server() {
    let (commands, _sent) = channel();
    let mut ui = running(&["a", "b"]);
    step(&mut ui, |snapshot| {
        let mut latency = result(Stage::Latency, None, None);
        latency.server_latencies = vec![
            latency_result("a", probes(&[10_000_000; 3], 0)),
            latency_result("b", probes(&[90_000_000; 3], 0)),
        ];
        snapshot.results = vec![latency, result(Stage::Download, Some(3e6), None)];
        snapshot.stage = Some(Stage::Download);
        snapshot.failures.push(ServerFailure {
            server_id: "b".into(),
            stage: Stage::Download,
            scope: FailureScope::Throughput,
            reason: FailureReason::ConnectionLost,
            at: Duration::ZERO,
        });
    });
    assert_eq!(
        ui.notice, "B: Connection lost",
        "the failure names its server and reason"
    );
    step(&mut ui, |snapshot| snapshot.phase = Phase::Complete);
    let shown = screen(&mut ui);
    assert!(shown.contains("latency to A") && shown.contains("10.0 ms"), "{shown}");
    press(&mut ui, &commands, &["l"]);
    let shown = screen(&mut ui);
    assert!(
        ui.latency_server() == Some("b") && shown.contains("latency to B") && shown.contains("90.0 ms"),
        "{shown}"
    );
    let report = crate::report::render(&ui.snapshot, 100, Theme::default()).unwrap();
    assert!(
        report.contains("Latency to A") && report.contains("10.0 ms"),
        "the report follows the run's focus: {report}"
    );
    press(&mut ui, &commands, &["d"]);
    let details = screen(&mut ui);
    let (all, own) = (details.find("│ All servers "), details.find("│ A "));
    assert!(
        ui.popup == Popup::Details && all.is_some_and(|all| own.is_some_and(|own| all < own)),
        "{details}"
    );
    assert!(details.contains("24.00 Mbit/s"), "{details}");
    press(&mut ui, &commands, &["esc"]);
    assert_eq!(ui.popup, Popup::None);
}

#[test]
fn latency_follows_the_run_focus() {
    let (commands, _sent) = channel();
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
fn readiness_rows_and_available_servers() {
    let (commands, _sent) = channel();
    let mut ui = setup();
    prepare(&mut ui, &[None, Some("sign in"), Some("connection refused")]);
    let plan = screen(&mut ui);
    for want in [
        "● A",
        "Ready",
        "○ B",
        "Sign in",
        "✗ C",
        "Failed",
        "connection refused",
        "u Use available servers",
    ] {
        assert!(plan.contains(want), "{want} in {plan}");
    }
    assert!(ui.can_use_available());
    assert!(crate::report::plain(&ui.row(Setting::Servers).value).contains("1 of 3 ready"));
    press(&mut ui, &commands, &["u"]);
    assert_eq!(
        (ui.notice.as_str(), ui.config.servers.as_slice()),
        ("Using the available servers.", &["a".to_owned()][..])
    );
    ui.recheck = None;
    ui.checked_at = Some(Instant::now() - FRESHNESS - Duration::from_secs(1));
    let plan = screen(&mut ui);
    assert!(plan.contains("Recheck needed") && !plan.contains("Ready"), "{plan}");
}

#[test]
fn run_again_keeps_the_last_results_until_the_next_run_starts() {
    let (commands, mut sent) = channel();
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| {
        snapshot.results.push(result(Stage::Download, Some(1e6), None));
        snapshot.phase = Phase::Complete;
    });
    let previous = ui.snapshot.clone();
    press(&mut ui, &commands, &["r"]);
    assert!(matches!(sent.try_recv(), Ok(Command::Run(_))));
    ui.update(Snapshot {
        phase: Phase::Preparing,
        ..Snapshot::default()
    });
    assert!(
        ui.shown()
            .is_some_and(|(shown, _)| shown.results.len() == previous.results.len())
    );
    assert!(ui.status_label() == "Checking paths" && ui.running() && ui.progress() == Some(Progress::Checking));
    ui.update(Snapshot {
        phase: Phase::Failed,
        error: Some("Test could not start: Server could not be reached".into()),
        ..Snapshot::default()
    });
    assert!(
        ui.shown().is_some_and(|(shown, _)| shown.phase == Phase::Complete),
        "a failed start lost the results"
    );
    assert!(ui.notice.starts_with("Test could not start:") && ui.recheck.is_some());
    assert!(!screen(&mut ui).lines().last().unwrap_or_default().is_empty());
}

#[test]
fn the_chooser_waits_for_the_check_and_caps_the_draft() {
    let (commands, _sent) = channel();
    let mut ui = setup();
    step(&mut ui, |snapshot| snapshot.phase = Phase::Checking);
    press(&mut ui, &commands, &["s"]);
    assert!(ui.open_chooser && ui.notice.contains("when the path check finishes"));
    prepare(&mut ui, &[None, Some("x"), Some("x"), Some("x"), Some("x")]);
    let servers: Vec<_> = ui
        .prepared
        .iter()
        .enumerate()
        .map(|(index, server)| ServerSummary {
            error: None,
            throughput: None,
            latency: (index == 0).then(|| target("a").1),
            ..server.clone()
        })
        .collect();
    step(&mut ui, |snapshot| {
        (snapshot.phase, snapshot.servers) = (Phase::Setup, servers)
    });
    assert_eq!(ui.popup, Popup::Servers, "the chooser opens after the check");
    assert_eq!(ui.notice, "Choose up to 4. Their speeds are combined.");
    let keys = "space down space down space down space down space up up up up space";
    press(&mut ui, &commands, &keys.split(' ').collect::<Vec<_>>());
    assert_eq!(ui.draft.len(), 4);
    assert_eq!(ui.notice, "At most four servers share one test.");
    let chooser = screen(&mut ui);
    assert!(
        chooser.contains("Test servers · 4 selected")
            && chooser.contains("○ A · Ready")
            && chooser.contains("https://b.example"),
        "{chooser}"
    );
    press(&mut ui, &commands, &["esc"]);
    assert!(ui.popup == Popup::None && ui.config.servers.is_empty() && ui.notice == "Server selection unchanged.");
    press(&mut ui, &commands, &["s", "down", "space", "enter"]);
    assert_eq!(ui.config.servers, ["a", "b"]);
    assert!(ui.recheck.is_some() && ui.notice == "Checking the selected servers…");
}

#[test]
fn every_view_fits_the_terminal() {
    let mut small = setup();
    small.size = (30, 10);
    assert!(screen(&mut small).contains("Enlarge the terminal"));
    for (width, height) in [(40, 12), (80, 24), (160, 50)] {
        let mut setup = setup();
        setup.size = (width, height);
        setup.config.url = "https://a-very-long-hostname.internal.example.com:7247".into();
        prepare(&mut setup, &[None, Some(&"a long failure ".repeat(8)), None]);
        setup.notice = "a long notice ".repeat(12);
        let mut run = running(&["a", "b"]);
        run.size = (width, height);
        step(&mut run, |snapshot| {
            snapshot.results = vec![result(Stage::Download, Some(1e9), None)]
        });
        let mut frames = vec![("run", run.screen()), ("setup", setup.screen())];
        setup.advanced = true;
        setup.row = setup.rows().len() - 1;
        frames.push(("advanced", setup.screen()));
        run.popup = Popup::Details;
        frames.push(("details", run.screen()));
        setup.begin_edit(Setting::Catalogue, setup.config.url.clone());
        frames.push(("edit", setup.screen()));
        setup.edit = None;
        setup.snapshot.auth = Some(prompt("ABCD", &format!("https://x/{}", "a".repeat(200))));
        frames.push(("sign-in", setup.screen()));
        setup.snapshot.auth = None;
        setup.popup = Popup::Servers;
        frames.push(("servers", setup.screen()));
        for (name, frame) in frames {
            let lines: Vec<_> = frame.iter().map(crate::report::plain).collect();
            assert!(
                lines.len() <= usize::from(height),
                "{name} at {width}x{height}: {} lines",
                lines.len()
            );
            assert!(lines[0].contains("Graphite Meter"), "{name} lost its header");
            assert!(
                lines.last().is_some_and(|last| last.trim_end().ends_with("quit")),
                "{name} at {width}x{height}: {lines:#?}"
            );
            for line in &lines {
                let trimmed = line.trim();
                assert!(
                    !trimmed.starts_with('│') || trimmed.ends_with('│'),
                    "{name} at {width}x{height}: {trimmed}"
                );
                assert!(
                    !trimmed.starts_with('╭') || trimmed.ends_with('╮'),
                    "{name} at {width}x{height}: {trimmed}"
                );
            }
            for line in &frame {
                assert!(
                    line.width() <= usize::from(width),
                    "{name} at {width}x{height}: {line:?}"
                );
            }
        }
    }
}

#[test]
fn a_failed_run_shows_no_activity() {
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| {
        (snapshot.phase, snapshot.error) = (Phase::Failed, Some("refused".into()))
    });
    let (shown, report) = (
        screen(&mut ui),
        crate::report::render(&ui.snapshot, 100, Theme::default()).unwrap(),
    );
    for stale in ["Checking paths", "○", "Median"] {
        assert!(
            !shown.contains(stale) && !report.contains(stale),
            "{stale}: {shown}\n{report}"
        );
    }
    assert!(
        shown.contains("— Skipped") && report.ends_with("refused"),
        "{shown}\n{report}"
    );
}

#[test]
fn scrolling_reveals_the_whole_body() {
    let (commands, _sent) = channel();
    let mut ui = running(&["a", "b"]);
    ui.size = (80, 12);
    step(&mut ui, |snapshot| {
        snapshot.phase = Phase::Complete;
        snapshot.results = [Stage::Download, Stage::Upload, Stage::Bidirectional]
            .map(|stage| {
                let mut result = result(stage, Some(1e9), stage.uploads().then_some(1e9));
                result.server_results = ["a", "b"]
                    .map(|id| crate::model::ServerContribution {
                        id: id.into(),
                        ..Default::default()
                    })
                    .into();
                result
            })
            .into();
        for stage in [Stage::Download, Stage::Upload, Stage::Bidirectional] {
            for id in ["a", "b"] {
                snapshot.failures.push(ServerFailure {
                    server_id: id.into(),
                    stage,
                    scope: FailureScope::Throughput,
                    reason: FailureReason::ConnectionLost,
                    at: Duration::ZERO,
                });
            }
        }
    });
    let layout = ui.layout();
    assert!(layout.body.len() > layout.body_height && plain(&ui.screen()).contains("pgdn more"));
    ui.size = (40, 12);
    let layout = ui.layout();
    let footer = plain(&ui.screen());
    assert!(
        footer.contains("pgdn more") && footer.trim_end().ends_with("quit"),
        "{footer}"
    );
    let body: Vec<_> = layout
        .body
        .iter()
        .map(|line| crate::report::plain(line).trim().to_owned())
        .collect();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..body.len() {
        seen.extend(
            ui.screen()
                .iter()
                .map(|line| crate::report::plain(line).trim().to_owned()),
        );
        press(&mut ui, &commands, &["down"]);
    }
    assert!(body.iter().all(|line| seen.contains(line)), "a line was never shown");
    press(&mut ui, &commands, &["home"]);
    assert_eq!(ui.body, 0);
}

#[test]
fn reset_asks_first() {
    let (commands, _sent) = channel();
    let mut ui = setup();
    (ui.config.warmup, ui.advanced) = (Duration::from_secs(1), true);
    at(&mut ui, Setting::Reset);
    press(&mut ui, &commands, &["enter"]);
    assert!(
        ui.config.warmup == Duration::from_secs(1) && ui.reset_prompt,
        "reset asks first"
    );
    press(&mut ui, &commands, &["x"]);
    assert!(ui.config.warmup == Duration::from_secs(1) && !ui.reset_prompt && ui.notice == "Settings kept.");
    press(&mut ui, &commands, &["enter", "enter"]);
    assert!(ui.config.warmup == Config::default().warmup && !ui.reset_prompt);
}

#[test]
fn live_rates_wait_for_evidence() {
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| {
        (snapshot.stage, snapshot.phase) = (Some(Stage::Download), Phase::Measuring)
    });
    assert!(ui.live_text(60, 16).contains("↓ —"), "a rate before the first sample");
    step(&mut ui, |snapshot| {
        snapshot.latest = crate::model::Point {
            elapsed: Duration::from_millis(250),
            down_bps: Some(8e6),
            sample_count: 1,
            ..Default::default()
        };
    });
    assert!(
        ui.live_text(60, 16).contains("↓ 8.00 Mbit/s"),
        "{}",
        ui.live_text(60, 16)
    );
    step(&mut ui, |snapshot| snapshot.latest.down_bps = None);
    assert!(ui.live_text(60, 16).contains("↓ — window restarting"));
}

#[test]
fn a_finished_run_gives_the_room_to_the_timeline() {
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| {
        let mut latency = result(Stage::Latency, None, None);
        latency.server_latencies = vec![latency_result("a", probes(&[1_000_000; 3], 0))];
        snapshot.results = vec![latency, result(Stage::Download, Some(1e9), None)];
        snapshot.phase = Phase::Complete;
    });
    for size in [(80, 24), (120, 40)] {
        ui.size = size;
        let shown = screen(&mut ui);
        assert!(
            shown.contains("Timeline") && shown.contains("Results") && !shown.contains('✓'),
            "{shown}"
        );
    }
}

#[test]
fn missing_evidence_is_never_shown_as_measured() {
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| {
        snapshot.plan = vec![Stage::Latency, Stage::Upload];
        let mut latency = result(Stage::Latency, None, None);
        latency.server_latencies = vec![latency_result("a", probes(&[1_000_000; 3], 0))];
        let mut upload = result(Stage::Upload, None, None);
        upload.up = Some(MeasurementResult {
            direction: Direction::Up,
            mean_bytes_per_sec: None,
            peak_bytes_per_sec: None,
            total_bytes: 0,
            samples: 0,
            elapsed_nanos: None,
        });
        snapshot.results = vec![latency, upload];
        snapshot.phase = Phase::Incomplete;
    });
    let (details, report) = (
        ui.details_text(120),
        crate::report::render(&ui.snapshot, 100, Theme::default()).unwrap(),
    );
    for wrong in ["0 B", "·  ·", "Added"] {
        assert!(
            !details.contains(wrong) && !report.contains(wrong),
            "{wrong}:\n{details}\n{report}"
        );
    }
}

#[test]
fn footer_hints_drop_from_the_middle_and_keep_quit() {
    let mut ui = setup();
    at(&mut ui, Setting::Stage(Stage::Download));
    let hints = |ui: &mut Ui, width| {
        ui.size = (width, 24);
        crate::report::plain(ui.screen().last().unwrap()).trim().to_owned()
    };
    assert_eq!(
        hints(&mut ui, 120),
        "r start test • ↑/↓ move • ←/→ change • space on/off • enter edit • ? keys • q quit"
    );
    assert_eq!(hints(&mut ui, 40), "r start test • pgdn more • q quit");
    (ui.help, ui.size) = (true, (120, 24));
    let help = plain(&ui.screen()[20..]);
    assert!(
        help.contains("r   start test    space on/off           a    automatic paths    q quit"),
        "{help}"
    );
}

#[test]
fn each_mode_offers_its_own_keys() {
    let (commands, _sent) = channel();
    let mut ui = setup();
    at(&mut ui, Setting::Catalogue);
    press(&mut ui, &commands, &["enter"]);
    let last = |ui: &mut Ui| crate::report::plain(ui.screen().last().unwrap()).trim().to_owned();
    assert_eq!(last(&mut ui), "←/→ move • enter apply • esc cancel • ctrl+c quit");
    press(&mut ui, &commands, &["esc"]);
    let mut run = running(&["a", "b"]);
    step(&mut run, |snapshot| snapshot.phase = Phase::Measuring);
    assert_eq!(
        last(&mut run),
        "esc stop test • d details • l latency server • ? keys • q quit"
    );
    press(&mut run, &commands, &["d"]);
    assert_eq!(last(&mut run), "↑/↓ scroll • esc close • q quit");
    press(&mut run, &commands, &["esc", "esc"]);
    assert_eq!(last(&mut run), "esc confirm stop • q quit");
}

#[test]
fn a_late_background_answer_never_becomes_keys() {
    let (commands, mut sent) = channel();
    let mut ui = setup();
    let answer: Vec<_> = std::iter::once("alt+]")
        .chain("11;rgb:ffff/ffff/ffff".split("").filter(|part| !part.is_empty()))
        .chain(["alt+\\"])
        .collect();
    assert!(!press(&mut ui, &commands, &answer));
    assert!(
        sent.try_recv().is_err() && !ui.live && ui.popup == Popup::None && !ui.help,
        "the answer ran keys"
    );
    press(&mut ui, &commands, &["alt+]", "1", "1", "ctrl+g", "r"]);
    assert!(
        matches!(sent.try_recv(), Ok(Command::Run(_))),
        "keys after the answer work"
    );
}

#[test]
fn path_rows_cycle_the_checked_servers_paths() {
    let (commands, _sent) = channel();
    let mut ui = setup();
    ui.config.url = "http://127.0.0.1:7246".into();
    let fetch = |origin: &str, protocol| ThroughputTarget {
        base_url: origin.into(),
        transport: ThroughputTransport::FetchStream,
        protocol,
    };
    let offered = Capabilities {
        upload_checkpoint: true,
        throughput: vec![
            fetch("http://127.0.0.1:7246", Protocol::Http1),
            ThroughputTarget {
                base_url: "https://127.0.0.1:7247".into(),
                transport: ThroughputTransport::WebTransport,
                protocol: Protocol::Http3,
            },
            ThroughputTarget {
                base_url: "https://127.0.0.1:7247".into(),
                transport: ThroughputTransport::WebTransportDatagram,
                protocol: Protocol::Http3,
            },
        ],
        latency: vec![LatencyTarget {
            base_url: "http://127.0.0.1:7246".into(),
            transport: LatencyTransport::WebSocket,
        }],
    };
    ui.prepared = vec![ServerSummary {
        id: "self".into(),
        name: "Lab".into(),
        throughput: Some(fetch("http://127.0.0.1:7246", Protocol::Http1)),
        latency: offered.latency.first().cloned(),
        offered: Some(offered),
        ..ServerSummary::default()
    }];
    let value = |ui: &Ui, setting| crate::report::plain(&ui.row(setting).value);
    assert_eq!(value(&ui, Setting::Path(false)), "Automatic · → :7246");
    assert_eq!(
        ui.row(Setting::Path(false)).help,
        "How transfers reach the server. ←/→ picks one of 3."
    );
    at(&mut ui, Setting::Path(false));
    press(&mut ui, &commands, &["right"]);
    assert_eq!(
        value(&ui, Setting::Path(false)),
        "Fetch streams · HTTP/1.1 · clear · :7246"
    );
    assert_eq!(ui.notice, "Throughput path: Fetch streams · HTTP/1.1 · clear.");
    assert!(ui.row(Setting::Protocol).inert && value(&ui, Setting::Protocol) == "HTTP/1.1");
    at(&mut ui, Setting::Protocol);
    press(&mut ui, &commands, &["right"]);
    assert_eq!(ui.notice, "This path serves HTTP/1.1 only.");
    at(&mut ui, Setting::Path(false));
    press(&mut ui, &commands, &["right"]);
    assert_eq!(
        value(&ui, Setting::Path(false)),
        "WebTransport streams · HTTP/3 · TLS · :7247"
    );
    press(&mut ui, &commands, &["right"]);
    assert_eq!(value(&ui, Setting::Path(false)), "Automatic · → :7246");
    assert_eq!(value(&ui, Setting::Path(true)), "Automatic · → :7246");
    ui.config.latency_transport = Some(LatencyTransport::WebTransport);
    assert_eq!(
        value(&ui, Setting::Path(true)),
        "WebTransport datagrams · automatic origin"
    );
    assert!(
        ui.row(Setting::Path(true))
            .help
            .starts_with("Not offered by the checked server.")
    );
    // Several servers share each transport, naming those that lack it.
    ui.prepared.push(ServerSummary {
        id: "far".into(),
        name: "Far".into(),
        error: Some("refused".into()),
        ..ServerSummary::default()
    });
    ui.config.latency_transport = None;
    assert_eq!(value(&ui, Setting::Path(true)), "Automatic · each server");
    at(&mut ui, Setting::Path(true));
    press(&mut ui, &commands, &["left"]);
    assert_eq!(
        value(&ui, Setting::Path(true)),
        "WebTransport datagrams · unavailable on Lab, Far"
    );
}

#[test]
fn stream_rows_force_a_count_or_bound_http1() {
    let (commands, _sent) = channel();
    let mut ui = setup();
    ui.advanced = true;
    let value = |ui: &Ui, setting| crate::report::plain(&ui.row(setting).value);
    assert_eq!(
        (ui.row(Setting::Streams).label, value(&ui, Setting::Streams).as_str()),
        ("Maximum H1 streams per direction", "6")
    );
    at(&mut ui, Setting::Streams);
    press(&mut ui, &commands, &["right"]);
    assert_eq!(ui.notice, "Stream count: Automatic · up to 7 per direction.");
    press(&mut ui, &commands, &["up", "space"]);
    assert_eq!(ui.config.streams, 7);
    assert_eq!(ui.notice, "Stream count: Forced · 7 per direction.");
    assert_eq!(ui.row(Setting::Streams).label, "Streams per server and direction");
    press(&mut ui, &commands, &["down"]);
    press(&mut ui, &commands, &["right"; 20]);
    assert_eq!(ui.config.streams, 14, "the count stops at 14");
}

#[test]
fn setup_rows_read_as_go_writes_them() {
    let mut ui = setup();
    ui.advanced = true;
    ui.config.ping_interval = Duration::from_millis(1500);
    let rows: Vec<_> = ui
        .rows()
        .into_iter()
        .map(|row| {
            let row = ui.row(row);
            format!("{} | {}", row.label, crate::report::plain(&row.value))
        })
        .collect();
    assert_eq!(
        rows,
        [
            "Start test | ",
            "Catalogue URL | http://127.0.0.1:7246",
            "Test servers | —",
            "Throughput path | Automatic · each server",
            "HTTP version | Automatic",
            "Latency path | Automatic · each server",
            "Latency | ● 4 s",
            "Download | ● 10 s",
            "Upload | ● 10 s",
            "Bidirectional | ○ 10 s",
            "Loaded latency | ●",
            "Advanced | ▾ shown",
            "Warmup | 800 ms",
            "Idle latency cadence | Custom (1.5 s)",
            "Loaded latency cadence | Medium (250 ms)",
            "Force exact stream count | ○",
            "Maximum H1 streams per direction | 6",
            "Skip TLS verify | ○",
            "Reset settings | ",
        ]
    );
    assert_eq!(
        ui.row(Setting::Stage(Stage::Upload)).help,
        "Client to server, receiver-timed. ←/→ ±1 s (1 s–300 s), space on/off."
    );
    assert_eq!(
        ui.row(Setting::Cadence(true)).help,
        "Probe spacing during transfers. ←/→ reply-driven, 80, 250, 600 ms."
    );
    assert_eq!(
        ui.row(Setting::Warmup).help,
        "Ramp-up before each window, at least ten round trips. ←/→ ±100 ms (0 ms–4 s)."
    );
}

#[test]
fn start_notes_say_what_the_run_takes_or_what_stops_it() {
    let (commands, _sent) = channel();
    let mut ui = setup();
    let note = |ui: &mut Ui| rows(ui)[4].clone();
    assert!(
        note(&mut ui).contains("Start test   3 stages · about 26 s"),
        "{}",
        note(&mut ui)
    );
    ui.config.stages.clear();
    assert!(note(&mut ui).contains("select at least one stage"));
    press(&mut ui, &commands, &["r"]);
    assert_eq!(
        ui.notice,
        "Test cannot start: select at least one stage: latency, download, upload or bidirectional."
    );
    assert_eq!(ui.current(), Setting::Stage(Stage::Latency), "Go moves to the stages");
    ui.config = Config::default();
    ui.signed_out = true;
    assert!(note(&mut ui).contains("sign in first; v requests a new code"));
    ui.signed_out = false;
    ui.recheck_soon();
    assert!(note(&mut ui).contains("checking paths"));
}

#[test]
fn left_and_right_change_only_what_go_adjusts() {
    let (commands, _sent) = channel();
    let mut ui = setup();
    for row in [Setting::Catalogue, Setting::Servers, Setting::Start] {
        at(&mut ui, row);
        press(&mut ui, &commands, &["right", "left"]);
        assert!(ui.edit.is_none() && ui.popup == Popup::None && !ui.live, "{row:?}");
    }
    at(&mut ui, Setting::LoadedLatency);
    press(&mut ui, &commands, &["left"]);
    assert!(!ui.config.loaded_latency && ui.notice == "Loaded latency off.");
    press(&mut ui, &commands, &["left"]);
    assert!(!ui.config.loaded_latency, "left sets off");
    press(&mut ui, &commands, &["right"]);
    assert!(ui.config.loaded_latency && ui.notice == "Loaded latency on.");
    at(&mut ui, Setting::Stage(Stage::Download));
    press(&mut ui, &commands, &["right"]);
    assert_eq!(ui.notice, "Download 11 s.");
    press(&mut ui, &commands, &["esc"]);
    assert_eq!(
        (ui.notice.as_str(), ui.current()),
        ("Download 11 s.", Setting::Stage(Stage::Download)),
        "esc does nothing in setup"
    );
    ui.advanced = true;
    at(&mut ui, Setting::Cadence(false));
    press(&mut ui, &commands, &["right"]);
    assert_eq!(ui.notice, "Idle latency cadence: Fast (80 ms).");
    press(&mut ui, &commands, &["a"]);
    assert_eq!(ui.notice, "Automatic paths applied to every selected server.");
}

#[test]
fn path_settings_are_checked_again_once_changes_settle() {
    let (commands, mut sent) = channel();
    let mut ui = setup();
    at(&mut ui, Setting::Stage(Stage::Download));
    press(&mut ui, &commands, &["right"]);
    assert!(ui.recheck.is_none(), "a duration does not change the paths");
    at(&mut ui, Setting::Protocol);
    press(&mut ui, &commands, &["right"]);
    assert!(
        ui.recheck.is_some() && !ui.recheck(&commands),
        "the check waits for changes to settle"
    );
    ui.recheck = Some(Instant::now());
    assert!(ui.recheck(&commands));
    let Ok(Command::Verify(config)) = sent.try_recv() else {
        panic!("changed paths were not checked again");
    };
    assert_eq!(config.throughput_protocol, Some(Protocol::Http1));
    assert_eq!(
        ui.notice, "HTTP version: HTTP/1.1.",
        "the check keeps the change's notice"
    );
}

#[test]
fn window_title_and_progress_bar_follow_the_run_like_go() {
    let mut ui = running(&["a"]);
    assert_eq!(ui.title(), "Graphite Meter · Checking paths");
    assert_eq!(ui.progress(), Some(Progress::Done(0)));
    step(&mut ui, |snapshot| {
        let mut latency = result(Stage::Latency, None, None);
        latency.server_latencies = vec![latency_result("a", probes(&[1_000_000; 3], 0))];
        snapshot.results.push(latency);
        (snapshot.stage, snapshot.phase) = (Some(Stage::Download), Phase::Measuring);
    });
    ui.run.since = Some(Instant::now() - Duration::from_secs(5));
    // 4 s of latency and 5 s of download in a 34 s plan.
    assert_eq!(ui.title(), "Graphite Meter · Download");
    assert_eq!(ui.progress(), Some(Progress::Done(26)));
    step(&mut ui, |snapshot| snapshot.phase = Phase::Complete);
    assert_eq!(
        (ui.title().as_str(), ui.progress()),
        ("Graphite Meter · Complete", None)
    );
    let mut chrome = Chrome::default();
    assert_eq!(
        chrome.update("Graphite Meter · Checking paths".into(), Some(Progress::Checking)),
        "\x1b]2;Graphite Meter · Checking paths\x07\x1b]9;4;3\x07"
    );
    assert_eq!(
        chrome.update("Graphite Meter · Checking paths".into(), Some(Progress::Checking)),
        ""
    );
    assert_eq!(
        chrome.update("Graphite Meter · Download".into(), Some(Progress::Done(37))),
        "\x1b]2;Graphite Meter · Download\x07\x1b]9;4;1;37\x07"
    );
    assert_eq!(chrome.update(String::new(), None), "\x1b]2;\x07\x1b]9;4;0\x07");
}

#[test]
fn escaping_sign_in_returns_to_setup_with_the_cancel_notice() {
    for (live, phase, ended) in [
        (false, Phase::Checking, Phase::Setup),
        (true, Phase::Preparing, Phase::Cancelled),
    ] {
        let (commands, mut sent) = channel();
        let mut ui = setup();
        ui.live = live;
        ui.update(Snapshot {
            phase,
            auth: Some(prompt("782411", "https://meter.example/auth/cli?challenge=x")),
            ..Snapshot::default()
        });
        press(&mut ui, &commands, &["esc"]);
        assert!(matches!(sent.try_recv(), Ok(Command::Cancel)));
        for phase in [phase, ended] {
            ui.update(Snapshot {
                phase,
                ..Snapshot::default()
            });
            assert_eq!(ui.notice, "Sign-in canceled. Press v to request a new code.");
        }
        assert!(!ui.live);
    }
}
