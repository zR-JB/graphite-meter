//! Go's TUI tests (main_test.go and run_test.go) over the controller's snapshots.
use super::*;
use crate::model::{
    AuthPrompt, FailureScope, Point, ServerContribution, ServerFailure, ServerLatency, ServerLatencyResult, StageResult,
};
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

fn plain(lines: &[ratatui::text::Line]) -> String {
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

/// The latency stage's result: three replies from server a in `rtt` nanoseconds.
fn idle(rtt: i64) -> StageResult {
    let mut latency = result(Stage::Latency, None, None);
    latency.server_latencies = vec![latency_result("a", probes(&[rtt; 3], 0))];
    latency
}

/// A server's lost connection in a stage's throughput.
pub(crate) fn lost(id: &str, stage: Stage) -> ServerFailure {
    let (scope, reason) = (FailureScope::Throughput, FailureReason::ConnectionLost);
    ServerFailure {
        server_id: id.into(),
        stage,
        scope,
        reason,
        at: Duration::ZERO,
    }
}

/// The final report Go prints for the shown run, without styles.
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

type Change = fn(&Ui) -> bool;

#[test]
fn setup_keys_move_and_change_rows_as_go_does() {
    let (commands, mut sent) = mpsc::channel(32);
    let mut ui = setup();
    #[rustfmt::skip]
    let cases = [("up", Setting::Start), ("down", Setting::Catalogue), ("tab", Setting::Servers)];
    for (name, want) in cases {
        press(&mut ui, &commands, &[name]);
        assert_eq!(ui.current(), want, "after {name}");
    }
    press(&mut ui, &commands, &["shift+tab"]);
    assert_eq!(ui.current(), Setting::Catalogue);
    press(&mut ui, &commands, &["down"; 20]);
    assert_eq!(ui.current(), Setting::Advanced, "the collapsed list ends at Advanced");
    press(&mut ui, &commands, &["right", "down", "right"]);
    assert!(ui.advanced && ui.config.warmup == Duration::from_millis(900));
    at(&mut ui, Setting::Start);
    assert!(screen(&mut ui).contains("enter start test")); // the start row offers enter
    press(&mut ui, &commands, &["enter"]);
    assert!(matches!(sent.try_recv(), Ok(Command::Run(_)))); // enter on Start test starts
    // Each row from a fresh setup with Advanced shown: its keys, the notice, whether the paths
    // are checked again, and the change. Esc does nothing in setup.
    let stage = Setting::Stage;
    let edited: Change = |ui| ui.edit.as_ref().is_some_and(|edit| edit.setting == ui.current());
    let quiet: Change = |ui| ui.edit.is_none() && ui.popup == Popup::None && !ui.live;
    #[rustfmt::skip]
    let cases: [(Setting, &[&str], &str, bool, Change); 21] = [
        (stage(Stage::Upload), &["space"], "Upload off.", true, |ui| ui.config.stages.len() == 2),
        (stage(Stage::Bidirectional), &["space"], "Bidirectional on.", false, |ui| ui.config.stages.len() == 4),
        (stage(Stage::Download), &["right", "esc"], "Download 11 s.", false,
            |ui| ui.config.download_duration.as_secs() == 11),
        (stage(Stage::Download), &["enter"], "Enter applies, esc cancels.", false, edited),
        (Setting::Catalogue, &["enter"], "Enter applies, esc cancels.", false, edited),
        (Setting::LoadedLatency, &["enter"], "Loaded latency off.", false, |ui| !ui.config.loaded_latency),
        (Setting::LoadedLatency, &["left", "left"], "Loaded latency off.", false, |ui| !ui.config.loaded_latency),
        (Setting::LoadedLatency, &["left", "right"], "Loaded latency on.", false, |ui| ui.config.loaded_latency),
        (Setting::Warmup, &["left"], "Warmup 700 ms.", false, |ui| ui.config.warmup.as_millis() == 700),
        (Setting::Cadence(false), &["enter"], "Idle latency cadence: Fast (80 ms).", true,
            |ui| ui.config.ping_interval.as_millis() == 80),
        (Setting::Cadence(false), &["left"], "Idle latency cadence: Slow (600 ms).", true,
            |ui| ui.config.ping_interval.as_millis() == 600),
        (Setting::Cadence(true), &["enter"], "Loaded latency cadence: Slow (600 ms).", true,
            |ui| ui.config.loaded_ping_interval.as_millis() == 600),
        (Setting::ForceStreams, &["enter"], "Stream count: Forced · 6 per direction.", true,
            |ui| ui.config.streams == 6),
        (Setting::Streams, &["right"], "Stream count: Automatic · up to 7 per direction.", true,
            |ui| ui.config.auto_streams == 7),
        (Setting::Insecure, &["enter"], "Skip TLS verify on.", true, |ui| ui.config.insecure),
        (Setting::Advanced, &["enter"], "", false, |ui| !ui.advanced),
        (Setting::Protocol, &["right"], "HTTP version: HTTP/1.1.", true,
            |ui| ui.config.throughput_protocol == Some(Protocol::Http1)),
        (Setting::Catalogue, &["right", "left"], "", false, quiet),
        (Setting::Servers, &["right", "left"], "", false, quiet),
        (Setting::Start, &["right", "left"], "", false, quiet),
        (Setting::Start, &["a"], "Automatic paths applied to every selected server.", true, quiet),
    ];
    for (row, keys, notice, recheck, changed) in cases {
        let mut ui = setup();
        ui.advanced = true;
        at(&mut ui, row);
        press(&mut ui, &commands, keys);
        let state = (changed(&ui), ui.current(), ui.notice.as_str(), ui.recheck.is_some());
        assert_eq!(state, (true, row, notice, recheck), "{keys:?}");
    }
    // A path change is checked once changes settle, and keeps its notice.
    let mut ui = setup();
    at(&mut ui, Setting::Protocol);
    press(&mut ui, &commands, &["right"]);
    assert!(!ui.recheck(&commands), "the check waits for changes to settle");
    ui.recheck = Some(Instant::now());
    assert!(ui.recheck(&commands) && ui.notice == "HTTP version: HTTP/1.1.");
    let Ok(Command::Verify(config)) = sent.try_recv() else {
        panic!("changed paths were not checked again");
    };
    assert_eq!(config.throughput_protocol, Some(Protocol::Http1));
    // A forced stream count renames its row and stops at 14.
    ui.advanced = true;
    assert_eq!(ui.row(Setting::Streams).label, "Maximum H1 streams per direction");
    at(&mut ui, Setting::Streams);
    press(&mut ui, &commands, &["right", "up", "space"]);
    assert_eq!(ui.config.streams, 7);
    assert_eq!(ui.notice, "Stream count: Forced · 7 per direction.");
    assert_eq!(ui.row(Setting::Streams).label, "Streams per server and direction");
    press(&mut ui, &commands, &["down"]);
    press(&mut ui, &commands, &["right"; 20]);
    assert_eq!(ui.config.streams, 14, "the count stops at 14");
    // Reset asks first.
    ui.config.warmup = Duration::from_secs(1);
    at(&mut ui, Setting::Reset);
    press(&mut ui, &commands, &["enter"]);
    assert!(ui.config.warmup == Duration::from_secs(1) && ui.reset_prompt); // reset asks first
    press(&mut ui, &commands, &["x"]);
    assert!(!ui.reset_prompt && ui.notice == "Settings kept.");
    press(&mut ui, &commands, &["enter", "enter"]);
    assert!(ui.config.warmup == Config::default().warmup && !ui.reset_prompt);
}

#[test]
fn edits_apply_and_refuse_as_go_parses_them() {
    let (commands, _sent) = mpsc::channel(32);
    let (download, upload) = (Setting::Stage(Stage::Download), Setting::Stage(Stage::Upload));
    let origin = "use an http:// or https:// origin, for example https://meter.example";
    let upload_bound = "Upload must be from 1 s to 300 s";
    let duration = "use a duration like 800ms, 4s, or 1m; a bare number is seconds";
    let streams = "streams must be a whole number from 1 to 14";
    #[rustfmt::skip]
    let cases = [
        (Setting::Catalogue, "m.example:8443/", Ok("https://m.example:8443")),
        (Setting::Catalogue, "127.0.0.1:7247", Ok("http://127.0.0.1:7247")),
        (Setting::Catalogue, "https://METER.example", Ok("https://meter.example")),
        (Setting::Catalogue, "ftp://x", Err(origin)),
        (Setting::Warmup, "0", Ok("0s")),
        (Setting::Warmup, "5s", Err("Warmup must be from 0 s to 4 s")),
        (download, "12", Ok("12s")),
        (download, "1.5m", Ok("1m30s")),
        (upload, "0", Err(upload_bound)),
        (upload, "6m", Err(upload_bound)),
        (upload, "soon", Err(duration)),
        (Setting::Streams, "8", Ok("8 0")),
        (Setting::Streams, "15", Err(streams)),
        (Setting::Streams, "x", Err(streams)),
    ];
    for (row, typed, expected) in cases {
        let mut ui = setup();
        ui.begin_edit(row, String::new());
        for character in typed.chars() {
            ui.key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE), &commands);
        }
        press(&mut ui, &commands, &["enter"]);
        let config = &ui.config;
        let got = match row {
            Setting::Catalogue => config.url.clone(),
            Setting::Warmup => setup::go_duration(config.warmup),
            Setting::Stage(_) => setup::go_duration(config.download_duration),
            _ => format!("{} {}", config.auto_streams, config.streams),
        };
        let Err(error) = expected else {
            assert!(ui.edit.is_none() && Ok(got.as_str()) == expected, "{typed}: {got}");
            continue;
        };
        // Go shows the error in the footer's error style and clears it as you type.
        let edit = ui.edit.as_ref().expect("the edit stays open");
        assert_eq!((edit.error.as_str(), edit.text()), (error, typed.into()));
        assert_eq!(ui.notice, error);
        assert!(screen(&mut ui).contains(error), "{typed}");
        press(&mut ui, &commands, &["backspace"]);
        assert!(ui.edit.as_ref().is_some_and(|edit| edit.error.is_empty()));
        press(&mut ui, &commands, &["esc"]);
        assert_eq!(ui.notice, "Edit canceled.");
    }
    // A forced count edits the forced streams.
    let mut ui = setup();
    ui.config.streams = 1;
    ui.begin_edit(Setting::Streams, "9".into());
    press(&mut ui, &commands, &["enter"]);
    assert_eq!((ui.config.auto_streams, ui.config.streams), (6, 9));
    // An unchanged catalogue keeps its servers.
    ui.config.servers = vec!["near".into()];
    ui.begin_edit(Setting::Catalogue, "HTTP://127.0.0.1:7246/".into());
    press(&mut ui, &commands, &["enter"]);
    assert_eq!(ui.config.servers.len(), 1);
    assert_eq!(ui.notice, "Catalogue http://127.0.0.1:7246.");
    ui.begin_edit(Setting::Catalogue, "meter.example".into());
    press(&mut ui, &commands, &["enter"]);
    assert!(ui.config.servers.is_empty() && ui.recheck.is_some());
    // Esc discards an edit, q types, textinput's keys move and cut, and ctrl+c quits.
    ui.begin_edit(Setting::Catalogue, ui.config.url.clone());
    press(&mut ui, &commands, &["x", "esc"]);
    assert!(ui.edit.is_none() && ui.config.url == "https://meter.example"); // esc discards the edit
    ui.begin_edit(Setting::Catalogue, String::new());
    assert!(!press(&mut ui, &commands, &["q"]) && ui.edit.as_ref().unwrap().text() == "q");
    let edit = ui.edit.as_mut().unwrap();
    #[rustfmt::skip]
    let cases = [(&["ctrl+a"][..], "q"), (&["end", "x", "left", "ctrl+k"], "q"), (&["x", "left", "ctrl+u"], "x")];
    for (keys, text) in cases {
        for name in keys {
            edit.key(name, name.chars().next().filter(|_| name.len() == 1));
        }
        assert_eq!(edit.text(), text, "{keys:?}");
    }
    for name in ["ctrl+u", "ctrl+k"] {
        edit.key(name, None);
    }
    edit.insert("one two");
    edit.key("ctrl+w", None);
    assert_eq!(edit.text(), "one ");
    edit.key("alt+b", None);
    edit.key("alt+d", None);
    assert_eq!(edit.text(), " ");
    assert!(press(&mut ui, &commands, &["ctrl+c"]), "ctrl+c quits from the editor");
}

#[test]
fn setup_rows_and_start_notes_read_as_go_writes_them() {
    let (commands, _sent) = mpsc::channel(32);
    let mut ui = setup();
    ui.advanced = true;
    ui.config.ping_interval = Duration::from_millis(1500);
    let listed: Vec<_> = ui.rows().into_iter().map(|row| ui.row(row)).collect();
    let listed: Vec<_> = listed
        .iter()
        .map(|row| format!("{} | {}", row.label, crate::report::plain(&row.value)))
        .collect();
    assert_eq!(
        listed,
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
    for (row, help) in [
        (
            Setting::Stage(Stage::Upload),
            "Client to server, receiver-timed. ←/→ ±1 s (1 s–300 s), space on/off.",
        ),
        (
            Setting::Cadence(true),
            "Probe spacing during transfers. ←/→ reply-driven, 80, 250, 600 ms.",
        ),
        (
            Setting::Warmup,
            "Ramp-up before each window, at least ten round trips. ←/→ ±100 ms (0 ms–4 s).",
        ),
    ] {
        assert_eq!(ui.row(row).help, help);
    }
    // The focused row's highlight covers the padding after its label, as Go pads inside the label's style.
    (ui.theme, ui.row) = (Theme::new(crate::theme::Profile::Ansi256, true), 3);
    let (lines, focused) = ui.setup_list(60);
    let label = &lines[focused].spans[1];
    assert!(label.content.ends_with("  "));
    assert_eq!(label.style.bg, ui.theme.selected.bg);
    // The start note says what the run takes, or what stops it.
    let mut ui = setup();
    let note = |ui: &mut Ui| rows(ui)[4].clone();
    assert!(note(&mut ui).contains("Start test   3 stages · about 26 s"));
    ui.config.stages.clear();
    assert!(note(&mut ui).contains("select at least one stage"));
    press(&mut ui, &commands, &["r"]);
    let refused = "Test cannot start: select at least one stage: latency, download, upload or bidirectional.";
    assert_eq!(ui.notice, refused);
    assert_eq!(ui.current(), Setting::Stage(Stage::Latency), "Go moves to the stages");
    (ui.config, ui.signed_out) = (Config::default(), true);
    assert!(note(&mut ui).contains("sign in first; v requests a new code"));
    ui.signed_out = false;
    ui.recheck_soon();
    assert!(note(&mut ui).contains("checking paths"));
}

#[test]
fn path_rows_cycle_the_checked_servers_paths() {
    let (commands, _sent) = mpsc::channel(32);
    let mut ui = setup();
    ui.config.url = "http://127.0.0.1:7246".into();
    use ThroughputTransport::{FetchStream, WebTransport, WebTransportDatagram};
    let path = |base_url: &str, transport, protocol| ThroughputTarget {
        base_url: base_url.into(),
        transport,
        protocol,
    };
    let fetch = path(&ui.config.url, FetchStream, Protocol::Http1);
    let tls = |transport| path("https://127.0.0.1:7247", transport, Protocol::Http3);
    let (_, mut latency) = target("");
    latency.base_url.clone_from(&ui.config.url);
    let offered = Capabilities {
        upload_checkpoint: true,
        throughput: vec![fetch.clone(), tls(WebTransport), tls(WebTransportDatagram)],
        latency: vec![latency.clone()],
    };
    ui.prepared = vec![ServerSummary {
        id: "self".into(),
        name: "Lab".into(),
        throughput: Some(fetch),
        latency: Some(latency),
        offered: Some(offered),
        ..ServerSummary::default()
    }];
    let value = |ui: &Ui, setting| crate::report::plain(&ui.row(setting).value);
    let (throughput, latency) = (Setting::Path(false), Setting::Path(true));
    assert_eq!(value(&ui, throughput), "Automatic · → :7246");
    let help = ui.row(throughput).help;
    assert_eq!(help, "How transfers reach the server. ←/→ picks one of 3.");
    at(&mut ui, throughput);
    press(&mut ui, &commands, &["right"]);
    assert_eq!(value(&ui, throughput), "Fetch streams · HTTP/1.1 · clear · :7246");
    assert_eq!(ui.notice, "Throughput path: Fetch streams · HTTP/1.1 · clear.");
    assert!(ui.row(Setting::Protocol).inert && value(&ui, Setting::Protocol) == "HTTP/1.1");
    at(&mut ui, Setting::Protocol);
    press(&mut ui, &commands, &["right"]);
    assert_eq!(ui.notice, "This path serves HTTP/1.1 only.");
    at(&mut ui, throughput);
    press(&mut ui, &commands, &["right"]);
    assert_eq!(value(&ui, throughput), "WebTransport streams · HTTP/3 · TLS · :7247");
    press(&mut ui, &commands, &["right"]);
    assert_eq!(value(&ui, throughput), "Automatic · → :7246");
    assert_eq!(value(&ui, latency), "Automatic · → :7246");
    ui.config.latency_transport = Some(LatencyTransport::WebTransport);
    assert_eq!(value(&ui, latency), "WebTransport datagrams · automatic origin");
    assert!(ui.row(latency).help.starts_with("Not offered by the checked server."));
    // Several servers share each transport, naming those that lack it.
    ui.prepared.push(ServerSummary {
        id: "far".into(),
        name: "Far".into(),
        error: Some("refused".into()),
        ..ServerSummary::default()
    });
    ui.config.latency_transport = None;
    assert_eq!(value(&ui, latency), "Automatic · each server");
    at(&mut ui, latency);
    press(&mut ui, &commands, &["left"]);
    assert_eq!(value(&ui, latency), "WebTransport datagrams · unavailable on Lab, Far");
}

#[test]
fn servers_ready_and_chosen_as_go_shows_them() {
    let (commands, _sent) = mpsc::channel(32);
    let mut ui = setup();
    prepare(&mut ui, &[None, Some("sign in"), Some("connection refused")]);
    let plan = screen(&mut ui);
    #[rustfmt::skip]
    let cases = ["● A", "Ready", "○ B", "Sign in", "✗ C", "Failed", "connection refused", "u Use available servers"];
    for want in cases {
        assert!(plan.contains(want), "{want} in {plan}");
    }
    assert!(ui.can_use_available());
    assert!(crate::report::plain(&ui.row(Setting::Servers).value).contains("1 of 3 ready"));
    // Go mutes the checked paths unless every selected server is ready.
    ui.theme = Theme::new(crate::theme::Profile::Ansi256, true);
    #[rustfmt::skip]
    let summary = ui.layout().body.into_iter().flat_map(|line| line.spans).find(|span| span.content.starts_with("Fetch"));
    assert_eq!(summary.map(|span| span.style), Some(ui.theme.muted));
    press(&mut ui, &commands, &["u"]);
    assert_eq!(ui.notice, "Using the available servers.");
    assert_eq!(ui.config.servers, ["a"]);
    ui.recheck = None;
    ui.checked_at = Some(Instant::now() - FRESHNESS - Duration::from_secs(1));
    let plan = screen(&mut ui);
    assert!(plan.contains("Recheck needed") && !plan.contains("Ready"), "{plan}");
    // The chooser waits for the check and caps the draft at four servers.
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
    for want in ["Test servers · 4 selected", "○ A · Ready", "https://b.example"] {
        assert!(chooser.contains(want), "{want} in {chooser}");
    }
    press(&mut ui, &commands, &["esc"]);
    assert!(ui.popup == Popup::None && ui.config.servers.is_empty() && ui.notice == "Server selection unchanged.");
    press(&mut ui, &commands, &["s", "down", "space", "enter"]);
    assert_eq!(ui.config.servers, ["a", "b"]);
    assert!(ui.recheck.is_some() && ui.notice == "Checking the selected servers…");
}

#[test]
fn hints_follow_the_mode_and_fit_the_width() {
    let (commands, _sent) = mpsc::channel(32);
    let last = |ui: &mut Ui| crate::report::plain(ui.screen().last().unwrap()).trim().to_owned();
    let mut ui = setup();
    ui.size = (120, 24);
    at(&mut ui, Setting::Stage(Stage::Download));
    let hints = "r start test • ↑/↓ move • ←/→ change • space on/off • enter edit • ? keys • q quit";
    assert_eq!(last(&mut ui), hints);
    ui.size = (40, 24);
    assert_eq!(last(&mut ui), "r start test • pgdn more • q quit"); // hints drop from the middle
    (ui.help, ui.size) = (true, (120, 24));
    let help = plain(&ui.screen()[20..]);
    assert!(help.contains("r   start test    space on/off           a    automatic paths    q quit"));
    ui.help = false;
    at(&mut ui, Setting::Catalogue);
    press(&mut ui, &commands, &["enter"]);
    assert_eq!(last(&mut ui), "←/→ move • enter apply • esc cancel • ctrl+c quit");
    let mut run = running(&["a", "b"]);
    step(&mut run, |snapshot| snapshot.phase = Phase::Measuring);
    for (keys, want) in [
        (
            &[][..],
            "esc stop test • d details • l latency server • ? keys • q quit",
        ),
        (&["d"], "↑/↓ scroll • esc close • q quit"),
        (&["esc", "esc"], "esc confirm stop • q quit"),
    ] {
        press(&mut run, &commands, keys);
        assert_eq!(last(&mut run), want);
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
    // Quitting stops the run first and reports it; a second interrupt quits at once.
    for name in ["q", "ctrl+c"] {
        let mut ui = running(&["a"]);
        stage(&mut ui, Stage::Latency, Phase::Measuring);
        assert!(!press(&mut ui, &commands, &[name])); // no quit before the test stops
        assert!(ui.quitting && matches!(sent.try_recv(), Ok(Command::Cancel)));
        step(&mut ui, |snapshot| snapshot.phase = Phase::Cancelled);
        let exit = ui.exit();
        assert!(!exit.running && exit.interrupted == (name == "ctrl+c"));
        assert_eq!(exit.shown.map(|shown| shown.phase), Some(Phase::Cancelled)); // the stopped run is reported
    }
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| snapshot.phase = Phase::Measuring);
    assert!(!ui.interrupt(&commands));
    assert!(press(&mut ui, &commands, &["ctrl+c"])); // a second interrupt quits at once
    assert!(ui.exit().running && ui.exit().shown.is_none());
    // Run again keeps the last results until the next run starts, and after a failed start.
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
    assert!(ui.notice.starts_with("Test could not start:") && ui.recheck.is_some());
    assert!(!screen(&mut ui).lines().last().unwrap_or_default().is_empty());
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
fn status_title_and_progress_follow_the_run_like_go() {
    let mut ui = running(&["a"]);
    assert_eq!(ui.title(), "Graphite Meter · Checking paths");
    assert_eq!(ui.progress(), Some(Progress::Done(0)));
    #[rustfmt::skip]
    let cases = [(Stage::Latency, Phase::Preparing, "Checking paths"), (Stage::Download, Phase::Warmup, "Warmup"),
        (Stage::Bidirectional, Phase::Measuring, "Bidirectional")];
    for (stage, phase, want) in cases {
        self::stage(&mut ui, stage, phase);
        assert_eq!(ui.status_label(), want);
    }
    step(&mut ui, |snapshot| snapshot.results.push(idle(1_000_000)));
    stage(&mut ui, Stage::Download, Phase::Measuring);
    ui.run.since = Some(Instant::now() - Duration::from_secs(5));
    // 4 s of latency and 5 s of download in a 34 s plan.
    assert_eq!(ui.title(), "Graphite Meter · Download");
    assert_eq!(ui.progress(), Some(Progress::Done(26)));
    #[rustfmt::skip]
    let cases = [(Phase::Complete, "Complete"), (Phase::Partial, "Partial"), (Phase::Cancelled, "Stopped"),
        (Phase::Failed, "Failed"), (Phase::Incomplete, "Incomplete")];
    for (phase, want) in cases {
        step(&mut ui, |snapshot| snapshot.phase = phase);
        assert_eq!((ui.status_label(), ui.progress()), (want, None));
    }
    // A check that ended before the view read its first snapshot settles, as Go takes it whenever it comes.
    #[rustfmt::skip]
    let failed = Ui::new(Config::default(), Snapshot { phase: Phase::Failed, error: Some("x".into()), ..Default::default() });
    assert_eq!(failed.status_label(), "Test could not start");
    let mut setup = setup();
    assert_eq!(setup.status_label(), "Not started");
    step(&mut setup, |snapshot| snapshot.phase = Phase::Checking);
    assert_eq!(setup.status_label(), "Not started");
    fail(&mut setup, "refused");
    assert_eq!(setup.status_label(), "Test could not start");
    setup.signed_out = true;
    assert_eq!(setup.status_label(), "Sign in");
    setup.config.stages.clear();
    assert_eq!(setup.status_label(), "Test cannot start");
    let mut chrome = Chrome::default();
    for (title, progress, sequences) in [
        (
            "Checking paths",
            Some(Progress::Checking),
            "\x1b]2;Graphite Meter · Checking paths\x07\x1b]9;4;3\x07",
        ),
        ("Checking paths", Some(Progress::Checking), ""),
        (
            "Download",
            Some(Progress::Done(37)),
            "\x1b]2;Graphite Meter · Download\x07\x1b]9;4;1;37\x07",
        ),
    ] {
        assert_eq!(chrome.update(format!("Graphite Meter · {title}"), progress), sequences);
    }
    assert_eq!(chrome.update(String::new(), None), "\x1b]2;\x07\x1b]9;4;0\x07");
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
fn the_live_view_follows_the_stage_and_its_samples() {
    let mut ui = running(&["a"]);
    #[rustfmt::skip]
    let cases = [(Stage::Latency, &["Idle latency", "3.0 ms"][..], &["↓", "↑"][..]),
        (Stage::Download, &["↓", "Loaded latency"], &["↑"]),
        (Stage::Bidirectional, &["↓", "↑", "Loaded latency"], &[])];
    for (stage, want, without) in cases {
        self::stage(&mut ui, stage, Phase::Measuring);
        for quarters in [1, 2] {
            sample(&mut ui, quarters, Some(8e6), Some(3.0), 0);
            std::thread::sleep(Duration::from_millis(60));
        }
        let live = ui.live_text(60, 16);
        assert!(want.iter().all(|want| live.contains(want)), "{stage:?}: {live}");
        assert!(!without.iter().any(|unwanted| live.contains(unwanted)), "{live}");
        assert!(live.chars().any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)), "{live}");
    }
    // Rates wait for evidence, and a window that restarts reads as such.
    let mut ui = running(&["a"]);
    stage(&mut ui, Stage::Download, Phase::Measuring);
    assert!(ui.live_text(60, 16).contains("↓ —"), "a rate before the first sample");
    sample(&mut ui, 1, Some(8e6), None, 0);
    assert!(ui.live_text(60, 16).contains("↓ 8.00 Mbit/s"));
    step(&mut ui, |snapshot| snapshot.latest.down_bps = None);
    assert!(ui.live_text(60, 16).contains("↓ — window restarting"));
    // Go's TestIdleReadingHoldsTheLastReplyThroughTimeouts: a reply ends the timeout streak.
    let mut ui = running(&["a"]);
    stage(&mut ui, Stage::Latency, Phase::Measuring);
    #[rustfmt::skip]
    let cases = [(1, Some(12.0), 0, "Idle latency 12.0 ms"), (2, None, 2, "Idle latency 12.0 ms  probe timeout ×2"),
        (3, Some(12.0), 0, "Idle latency 12.0 ms")];
    for (quarters, latest, timeouts, reading) in cases {
        sample(&mut ui, quarters, None, latest, timeouts);
        let live = ui.live_text(60, 16);
        assert!(live.contains(reading), "{live}");
        assert!(timeouts > 0 || !live.contains("timeout"), "{live}");
    }
    // A stage opens with no samples, as Snapshot::open_stage starts one.
    step(&mut ui, |snapshot| {
        snapshot.open_stage(Stage::Download, ["a".to_owned()].into_iter())
    });
    stage(&mut ui, Stage::Latency, Phase::Measuring);
    let live = ui.live_text(60, 16);
    assert!(live.contains("Idle latency —") && !live.contains("12.0 ms"), "{live}");
}

#[test]
fn the_stage_track_follows_the_stages() {
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| snapshot.results.push(idle(4_000_000)));
    stage(&mut ui, Stage::Download, Phase::Measuring);
    ui.run.since = Some(Instant::now() - Duration::from_millis(2500));
    let track = ui.track_text(80);
    for want in ["✓ 4.0 ms median", "2.5 s / 10 s", "○ 10 s"] {
        assert!(track.contains(want), "{want} in {track}");
    }
    step(&mut ui, |snapshot| {
        snapshot.results.push(result(Stage::Download, None, None));
        snapshot.results.push(result(Stage::Upload, None, Some(1e6)));
        snapshot.failures.push(ServerFailure {
            scope: FailureScope::Latency,
            reason: FailureReason::Timeout,
            ..lost("a", Stage::Upload)
        });
        snapshot.results[1].down = Some(MeasurementResult {
            mean_bytes_per_sec: None,
            ..download_measurement()
        });
    });
    let track = ui.track_text(80);
    #[rustfmt::skip]
    let cases = ["✓ 4.0 ms median", "Download      ✗ Failed", "Upload        ! ↑ 8.00 Mbit/s Partial"];
    for want in cases {
        assert!(track.contains(want), "{want} in {track}");
    }
    step(&mut ui, |snapshot| snapshot.phase = Phase::Cancelled);
    assert!(ui.track_text(80).contains("Bidirectional — Skipped"));
}

#[test]
fn results_show_what_each_stage_measured_or_why_not() {
    // Every population, with Added and Go's facts.
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
    let text = report(&ui);
    #[rustfmt::skip]
    let wants = ["Complete", "Idle", "Loaded latency · Download", "10.0 ms", "940.0 Mbit/s", "peak 1000 ·", "17.8 ms",
        "+7.8 ms", "40.00 Mbit/s", "Bidirectional"];
    for want in wants {
        assert!(text.contains(want), "{want} in {text}");
    }
    assert!(!text.to_lowercase().contains("loss"), "{text}");
    assert!(!text.contains("Idle latency:"), "{text}");
    ui.live = false;
    assert!(ui.exit().shown.is_none(), "no report before any run");
    // A failed stage reads its reason, and details keep the facts.
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| {
        snapshot.stage = Some(Stage::Bidirectional);
        let mut bidirectional = result(Stage::Bidirectional, None, None);
        bidirectional.down = Some(MeasurementResult {
            mean_bytes_per_sec: None,
            total_bytes: 42,
            ..download_measurement()
        });
        bidirectional.server_results = vec![ServerContribution {
            id: "a".into(),
            ..Default::default()
        }];
        let mut population = latency_result("a", probes(&[], 0));
        population.summary.unresolved = 2;
        population.ending = Some(crate::model::Ending::Failed(FailureReason::ConnectionLost));
        bidirectional.server_latencies = vec![population];
        snapshot.results.push(bidirectional);
        snapshot.failures.push(lost("a", Stage::Bidirectional));
        snapshot.phase = Phase::Incomplete;
    });
    let shown = screen(&mut ui);
    #[rustfmt::skip]
    let cases = ["Bi-dir ↓: Connection lost", "Loaded latency · Bidirectional", "Bi-dir ↓", "—"];
    for want in cases {
        assert!(shown.contains(want), "{want} in {shown}");
    }
    ui.popup = Popup::Details;
    assert!(ui.details_text(116).contains("unfinished probes 2"));
    // Missing evidence is never shown as measured.
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| {
        snapshot.plan = vec![Stage::Latency, Stage::Upload];
        let mut upload = result(Stage::Upload, None, None);
        upload.up = Some(MeasurementResult::unavailable(Direction::Up, 0));
        snapshot.results = vec![self::idle(1_000_000), upload];
        snapshot.phase = Phase::Incomplete;
    });
    let (details, report) = (ui.details_text(120), report(&ui));
    for wrong in ["0 B", "·  ·", "Added"] {
        assert!(!details.contains(wrong), "{wrong}: {details}");
        assert!(!report.contains(wrong), "{wrong}: {report}");
    }
    // A failed run shows no activity.
    let mut ui = running(&["a"]);
    fail(&mut ui, "refused");
    let (shown, report) = (screen(&mut ui), self::report(&ui));
    for stale in ["Checking paths", "○", "Median"] {
        assert!(!shown.contains(stale), "{stale}: {shown}");
        assert!(!report.contains(stale), "{stale}: {report}");
    }
    assert!(shown.contains("— Skipped"), "{shown}");
    assert!(report.ends_with("refused"), "{report}");
    // A finished run gives the room to the timeline.
    let mut ui = running(&["a"]);
    step(&mut ui, |snapshot| {
        snapshot.results = vec![self::idle(1_000_000), result(Stage::Download, Some(1e9), None)];
        snapshot.phase = Phase::Complete;
    });
    for size in [(80, 24), (120, 40)] {
        ui.size = size;
        let shown = screen(&mut ui);
        assert!(shown.contains("Timeline") && shown.contains("Results"), "{shown}");
        assert!(!shown.contains('✓'), "{shown}");
    }
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
        (setup.snapshot.auth, setup.popup) = (None, Popup::Servers);
        frames.push(("servers", setup.screen()));
        for (name, frame) in frames {
            let at = format!("{name} at {width}x{height}");
            let lines: Vec<_> = frame.iter().map(crate::report::plain).collect();
            assert!(lines.len() <= usize::from(height), "{at}");
            assert!(lines[0].contains("Graphite Meter"), "{at}");
            assert!(lines.last().unwrap().trim_end().ends_with("quit"), "{at}: {lines:#?}");
            for (line, text) in frame.iter().zip(&lines) {
                let trimmed = text.trim();
                assert!(!trimmed.starts_with('│') || trimmed.ends_with('│'), "{at}: {trimmed}");
                assert!(!trimmed.starts_with('╭') || trimmed.ends_with('╮'), "{at}: {trimmed}");
                assert!(line.width() <= usize::from(width), "{at}: {line:?}");
            }
        }
    }
}

#[test]
fn scrolling_reveals_the_whole_body() {
    let (commands, _sent) = mpsc::channel(32);
    let mut ui = running(&["a", "b"]);
    ui.size = (80, 12);
    step(&mut ui, |snapshot| {
        snapshot.phase = Phase::Complete;
        for stage in [Stage::Download, Stage::Upload, Stage::Bidirectional] {
            let mut result = result(stage, Some(1e9), stage.uploads().then_some(1e9));
            result.server_results = ["a", "b"]
                .map(|id| ServerContribution {
                    id: id.into(),
                    ..Default::default()
                })
                .into();
            snapshot.results.push(result);
            snapshot.failures.extend(["a", "b"].map(|id| lost(id, stage)));
        }
    });
    let layout = ui.layout();
    assert!(layout.body.len() > layout.body_height && plain(&ui.screen()).contains("pgdn more"));
    ui.size = (40, 12);
    let footer = plain(&ui.screen());
    assert!(footer.contains("pgdn more"), "{footer}");
    assert!(footer.trim_end().ends_with("quit"), "{footer}");
    let text = |lines: &[ratatui::text::Line]| -> Vec<String> {
        lines
            .iter()
            .map(|line| crate::report::plain(line).trim().to_owned())
            .collect()
    };
    let body = text(&ui.layout().body);
    let mut seen = std::collections::HashSet::new();
    for _ in 0..body.len() {
        seen.extend(text(&ui.screen()));
        press(&mut ui, &commands, &["down"]);
    }
    assert!(body.iter().all(|line| seen.contains(line)), "a line was never shown");
    press(&mut ui, &commands, &["home"]);
    assert_eq!(ui.body, 0);
}
