use super::setup::Field;
use super::*;
use crate::model::Stage;

#[test]
fn finished_latency_result_remains_visible_after_live_probes_end() {
    use crate::model::{ServerLatencyResult, StageResult};
    use graphite_meter_core::latency::{Distribution, LatencySummary};
    use ratatui::{Terminal, backend::TestBackend};

    let rtt = 1_500_000;
    let mut ui = Ui::new(
        Config::default(),
        Snapshot {
            phase: Phase::Complete,
            results: vec![StageResult {
                stage: Stage::Latency,
                elapsed: Duration::from_secs(1),
                down: None,
                up: None,
                intervals: Default::default(),
                omitted_intervals: 0,
                complete: true,
                server_latencies: vec![ServerLatencyResult {
                    elapsed: Some(Duration::from_secs(1)),
                    id: "self".into(),
                    summary: LatencySummary {
                        distribution: Some(Distribution {
                            min: rtt,
                            max: rtt,
                            mean: rtt,
                            p50: rtt,
                            p95: rtt,
                        }),
                        count: 4,
                        ..LatencySummary::default()
                    },
                    error: None,
                }],
                server_results: Vec::new(),
            }],
            ..Snapshot::default()
        },
    );
    ui.live = true;
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("1.5 ms"));

    // A selected peer absent from this stage must not inherit another
    // peer's RTT merely because its summary is first in the result.
    ui.latency_focus = Some("other".into());
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(!rendered.contains("1.5 ms"));
}

#[test]
fn active_run_requires_second_escape_but_setup_verification_cancels_immediately() {
    let (commands, mut received) = mpsc::channel(4);
    let mut ui = Ui::new(
        Config::default(),
        Snapshot {
            phase: Phase::Measuring,
            ..Snapshot::default()
        },
    );
    ui.live = true;
    ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands);
    assert_eq!(ui.cancel, CancelState::Confirming);
    assert!(received.try_recv().is_err());
    ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &commands);
    assert_eq!(ui.cancel, CancelState::Idle);
    assert!(!ui.notice().0.is_empty());
    assert!(received.try_recv().is_err());

    ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands);
    ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands);
    assert_eq!(ui.cancel, CancelState::Requested);
    assert!(matches!(received.try_recv(), Ok(Command::Cancel)));
    ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands);
    assert!(received.try_recv().is_err());

    ui.update(Snapshot {
        phase: Phase::Complete,
        ..Snapshot::default()
    });
    assert_eq!(ui.cancel, CancelState::Idle);
    ui.live = false;
    ui.update(Snapshot {
        phase: Phase::Preparing,
        ..Snapshot::default()
    });
    ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands);
    assert!(matches!(received.try_recv(), Ok(Command::Cancel)));
}

#[test]
fn reenabled_stage_runs_in_canonical_order() {
    let (commands, mut received) = mpsc::channel(4);
    let mut ui = Ui::new(Config::default(), Snapshot::default());
    let latency = ui
        .fields()
        .iter()
        .position(|field| *field == Field::LatencyStage);
    ui.rows.select(latency);
    for code in [KeyCode::Char(' '), KeyCode::Char(' '), KeyCode::Char('r')] {
        ui.key(KeyEvent::new(code, KeyModifiers::NONE), &commands);
    }
    let Ok(Command::Run(config)) = received.try_recv() else {
        panic!("run was not requested");
    };
    assert_eq!(config.stages, Config::default().stages);
}

#[tokio::test(start_paused = true)]
async fn approval_takes_priority_over_editing_and_keeps_long_browser_urls_reachable() {
    use crate::model::AuthPrompt;
    use ratatui::{Terminal, backend::TestBackend};

    let mut ui = Ui::new(
        Config::default(),
        Snapshot {
            phase: Phase::Preparing,
            auth: Some(AuthPrompt {
                deadline: tokio::time::Instant::now() + Duration::from_secs(120),
                origin: "https://meter.example".into(),
                code: "782411".into(),
                browser_url: format!(
                    "https://meter.example/auth/cli?challenge={}TAIL",
                    "x".repeat(300)
                ),
            }),
            ..Snapshot::default()
        },
    );
    ui.edit = Some(Edit::new(Field::Url, "original".into()));
    ui.help = true;
    ui.popup = Popup::Servers;
    let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
    let rendered = |terminal: &Terminal<TestBackend>| {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    };
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    assert!(rendered(&terminal).contains("Match this code: 782411"));
    tokio::time::advance(Duration::from_secs(30)).await;
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    assert!(rendered(&terminal).contains("waited 30 s · expires in 90 s"));
    assert!(rendered(&terminal).contains("Enter/Space/o open"));
    assert!(!rendered(&terminal).contains("TAIL"));

    let (commands, mut received) = mpsc::channel(4);
    ui.paste("ignored");
    assert_eq!(ui.edit.as_ref().unwrap().text(), "original");
    ui.key(
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
        &commands,
    );
    assert!(matches!(received.try_recv(), Ok(Command::OpenBrowser)));
    ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &commands);
    assert!(matches!(received.try_recv(), Ok(Command::OpenBrowser)));
    ui.key(
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        &commands,
    );
    assert!(matches!(received.try_recv(), Ok(Command::OpenBrowser)));
    for _ in 0..12 {
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &commands);
    }
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    assert!(rendered(&terminal).contains("TAIL"));
    assert!(rendered(&terminal).contains("Match this code: 782411"));
    ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands);
    assert!(matches!(received.try_recv(), Ok(Command::Cancel)));
    assert_eq!(ui.edit.as_ref().unwrap().text(), "original");

    let browser_url = ui.snapshot.auth.as_ref().unwrap().browser_url.clone();
    ui.update(Snapshot {
        auth: Some(AuthPrompt {
            deadline: tokio::time::Instant::now() + Duration::from_secs(120),
            origin: "https://meter.example".into(),
            code: "999999".into(),
            browser_url,
        }),
        ..Snapshot::default()
    });
    assert_eq!(ui.auth_scroll, 0);
}

#[test]
fn terminal_text_cannot_emit_controls_or_direction_overrides() {
    let text = safe_text("server\x1b]52;c;secret\x07\r\n\u{202e}name", 100);
    assert!(text.chars().all(safe_character));
    assert!(!text.contains('\x1b'));
    assert_eq!(safe_text("abcdef", 3), "abc");
}
#[test]
fn minimum_supported_terminal_keeps_live_measurement_visible() {
    use crate::model::{Point, ServerLatency, StageResult};
    use ratatui::{Terminal, backend::TestBackend};

    let snapshot = Snapshot {
        phase: Phase::Measuring,
        stage: Some(Stage::Upload),
        latest: Point {
            elapsed: Duration::from_secs(3),
            up_bps: Some(12_000_000.0),
            ..Point::default()
        },
        server_latencies: vec![ServerLatency {
            id: "near".into(),
            latest_ms: Some(25.0),
            ..ServerLatency::default()
        }],
        results: vec![StageResult {
            stage: Stage::Download,
            elapsed: Duration::from_secs(1),
            down: Some(download_measurement()),
            up: None,
            intervals: Default::default(),
            omitted_intervals: 0,
            complete: true,
            server_latencies: Vec::new(),
            server_results: Vec::new(),
        }],
        ..Snapshot::default()
    };
    let mut ui = Ui::new(Config::default(), snapshot);
    ui.live = true;
    let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Upload · 3.0 s"));
    assert!(rendered.contains("Upload 12.00 Mbit/s"));
    assert!(rendered.contains("Latency 25.0 ms"));
    assert!(rendered.contains("Download: 12.00 Mbit/s"));
    assert!(rendered.contains("d Details"));

    ui.help = true;
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    let expanded = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(expanded.contains("Tab/Shift-Tab"));
    assert!(expanded.contains("Ctrl-C stop"));
    assert_eq!(ui.popup, Popup::None);
    ui.help = false;
    ui.snapshot.phase = Phase::Complete;
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    let completed = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(completed.contains("Enter Run again"));
}

fn download_measurement() -> graphite_meter_core::measurement::MeasurementResult {
    use graphite_meter_core::measurement::*;
    MeasurementResult {
        stage: Stage::Download,
        direction: Direction::Down,
        total_bytes: 1_500_000,
        mean_bytes_per_sec: Some(1_500_000.0),
        peak_bytes_per_sec: Some(1_500_000.0),
        samples: 4,
        elapsed_nanos: Some(1_000_000_000),
        unavailable_reason: None,
    }
}

#[test]
fn stacked_run_keeps_charts_and_signed_loaded_latency_visible() {
    use crate::model::{Point, ServerLatency, ServerLatencyResult, StageResult};
    use graphite_meter_core::latency::{LatencyAccumulator, ProbeOutcome};
    use ratatui::{Terminal, backend::TestBackend};
    let mut idle = LatencyAccumulator::default();
    idle.record(ProbeOutcome::Reply {
        rtt_nanos: 500_000,
        handling_nanos: 0,
    });
    let mut loaded = LatencyAccumulator::default();
    loaded.record(ProbeOutcome::Reply {
        rtt_nanos: 200_000,
        handling_nanos: 0,
    });
    let mut host = ServerLatency {
        id: "self".into(),
        latest_ms: Some(0.2),
        ..ServerLatency::default()
    };
    let mut snapshot = Snapshot {
        phase: Phase::Measuring,
        stage: Some(Stage::Download),
        results: vec![
            StageResult {
                stage: Stage::Latency,
                elapsed: Duration::from_secs(60),
                down: None,
                up: None,
                intervals: Default::default(),
                omitted_intervals: 0,
                complete: true,
                server_latencies: vec![ServerLatencyResult {
                    elapsed: Some(Duration::from_secs(1)),
                    id: "self".into(),
                    summary: idle.snapshot(),
                    error: None,
                }],
                server_results: Vec::new(),
            },
            StageResult {
                stage: Stage::Download,
                elapsed: Duration::from_secs(1),
                down: Some(download_measurement()),
                up: None,
                intervals: Default::default(),
                omitted_intervals: 0,
                complete: true,
                server_latencies: vec![ServerLatencyResult {
                    elapsed: Some(Duration::from_secs(1)),
                    id: "self".into(),
                    summary: loaded.snapshot(),
                    error: None,
                }],
                server_results: Vec::new(),
            },
        ],
        ..Snapshot::default()
    };
    for index in 0..1000 {
        let elapsed = Duration::from_millis(index * 80);
        host.history.add(Point {
            elapsed,
            latency_ms: (index != 500).then_some(0.2),
            ..Point::default()
        });
        snapshot.history.add(Point {
            elapsed,
            down_bps: (index != 500).then_some(12_000_000.0),
            ..Point::default()
        });
    }
    assert_eq!(
        snapshot.history.points.front().unwrap().elapsed,
        Duration::ZERO
    );
    assert!(snapshot.history.points.len() <= 480);
    snapshot.server_latencies.push(host);
    let config = Config {
        stages: vec![Stage::Latency, Stage::Download],
        latency_duration: Duration::from_secs(60),
        download_duration: Duration::from_secs(60),
        ..Config::default()
    };
    let mut ui = Ui::new(config, snapshot);
    ui.live = true;
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    for text in [
        "Throughput",
        "Latency · ms",
        "−0.3 ms",
        "Probe timeouts",
        "120 s",
    ] {
        assert!(rendered.contains(text), "missing {text}: {rendered}");
    }
}
