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
                    ending: None,
                }],
                ..Default::default()
            }],
            participants: vec!["self".into()],
            latency_focus: Some("self".into()),
            plan: vec![Stage::Latency],
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
    ui.latency_pick = Some("other".into());
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
    let latency = ui.fields().iter().position(|field| *field == Field::LatencyStage);
    ui.rows.select(latency);
    for code in [KeyCode::Char(' '), KeyCode::Char(' '), KeyCode::Char('r')] {
        ui.key(KeyEvent::new(code, KeyModifiers::NONE), &commands);
    }
    let Ok(Command::Run(config)) = received.try_recv() else {
        panic!("run was not requested");
    };
    assert_eq!(config.stages, Config::default().stages);
}

#[test]
fn setup_keys_work_during_the_path_check() {
    use crate::model::ServerSummary;
    use graphite_meter_core::discovery::ThroughputTransport;
    let (commands, mut received) = mpsc::channel(8);
    let checking = || Snapshot {
        phase: Phase::Checking,
        ..Snapshot::default()
    };
    let mut ui = Ui::new(Config::default(), checking());
    let press = |ui: &mut Ui, code| ui.key(KeyEvent::new(code, KeyModifiers::NONE), &commands);

    press(&mut ui, KeyCode::Char('s'));
    assert_eq!(ui.popup, Popup::None, "the chooser waits for the check");
    let server = |id: &str| ServerSummary {
        id: id.into(),
        name: id.into(),
        ..ServerSummary::default()
    };
    ui.update(Snapshot {
        servers: vec![server("a"), server("b")],
        ..Snapshot::default()
    });
    assert_eq!(ui.popup, Popup::Servers);
    press(&mut ui, KeyCode::Esc);

    ui.update(checking());
    let bidirectional = ui.fields().iter().position(|field| *field == Field::BidiStage);
    ui.rows.select(bidirectional);
    press(&mut ui, KeyCode::Char(' '));
    assert!(ui.config.stages.contains(&Stage::Bidirectional));
    press(&mut ui, KeyCode::Char('v'));
    assert!(matches!(received.try_recv(), Ok(Command::Verify(_))));
    ui.config.throughput_transport = Some(ThroughputTransport::FetchStream);
    press(&mut ui, KeyCode::Char('a'));
    assert_eq!(ui.config.throughput_transport, None);

    ui.update(checking());
    ui.rows.select(Some(0));
    press(&mut ui, KeyCode::Enter);
    assert!(matches!(received.try_recv(), Ok(Command::Run(_))));
    ui.update(checking());
    assert!(ui.live && ui.running(), "the check the run replaces is not setup");
    ui.update(Snapshot {
        phase: Phase::Preparing,
        ..Snapshot::default()
    });
    ui.update(checking());
    assert!(!ui.live, "a check after the run started returns to setup");
}

#[test]
fn enter_edits_stage_and_warmup_durations_like_go() {
    let (commands, _received) = mpsc::channel(4);
    let mut ui = Ui::new(Config::default(), Snapshot::default());
    let press = |ui: &mut Ui, code| ui.key(KeyEvent::new(code, KeyModifiers::NONE), &commands);
    ui.advanced = true;
    for (field, typed, expected) in [
        (Field::DownloadStage, "12", Ok(Duration::from_secs(12))),
        (Field::DownloadStage, "1.5m", Ok(Duration::from_secs(90))),
        (Field::DownloadStage, "0", Err("Download must be from 1 s to 300 s")),
        (Field::DownloadStage, "6m", Err("Download must be from 1 s to 300 s")),
        (Field::DownloadStage, "soon", Err("use a duration like 800ms")),
        (Field::Warmup, "0", Ok(Duration::ZERO)),
        (Field::Warmup, "800ms", Ok(Duration::from_millis(800))),
        (Field::Warmup, "5s", Err("Warmup must be from 0 s to 4 s")),
    ] {
        let row = ui.fields().iter().position(|shown| *shown == field);
        ui.rows.select(row);
        press(&mut ui, KeyCode::Enter);
        let opened = ui.edit.as_ref().map(|edit| edit.text());
        assert!(
            opened.as_ref().is_some_and(|text| text.ends_with('s')),
            "{typed}: {opened:?}"
        );
        ui.edit = Some(Edit::new(field, typed.into()));
        press(&mut ui, KeyCode::Enter);
        let duration = match field {
            Field::Warmup => ui.config.warmup,
            _ => ui.config.download_duration,
        };
        match expected {
            Ok(expected) => assert!(ui.edit.is_none() && duration == expected, "{typed}: {duration:?}"),
            Err(error) => {
                assert!(
                    ui.edit.is_some() && ui.notice.starts_with(error),
                    "{typed}: {}",
                    ui.notice
                );
                press(&mut ui, KeyCode::Esc);
            }
        }
    }
    let download = ui.fields().iter().position(|shown| *shown == Field::DownloadStage);
    ui.rows.select(download);
    press(&mut ui, KeyCode::Char(' '));
    assert!(
        !ui.config.stages.contains(&Stage::Download),
        "space turns the stage off"
    );
}

#[test]
fn setup_rows_are_grouped_like_the_go_client() {
    use Field::*;
    let mut ui = Ui::new(Config::default(), Snapshot::default());
    assert!(
        ui.fields()
            == [
                Start,
                Url,
                Servers,
                ThroughputTransport,
                Protocol,
                LatencyTransport,
                LatencyStage,
                DownloadStage,
                UploadStage,
                BidiStage,
                LoadedLatency,
                Advanced,
            ]
    );
    ui.advanced = true;
    assert!(ui.fields()[12..].starts_with(&[Warmup]) && ui.fields().ends_with(&[Insecure, Reset]));
}

#[test]
fn setup_names_each_checked_server_state() {
    use crate::model::ServerSummary;
    use graphite_meter_core::discovery::{LatencyTarget, LatencyTransport};
    use ratatui::{Terminal, backend::TestBackend};
    let ready = ServerSummary {
        id: "near".into(),
        name: "Near".into(),
        latency: Some(LatencyTarget {
            base_url: "https://near.example".into(),
            transport: LatencyTransport::WebSocket,
        }),
        ..ServerSummary::default()
    };
    let failed = ServerSummary {
        id: "far".into(),
        name: "Far".into(),
        error: Some("connection refused".into()),
        ..ServerSummary::default()
    };
    let mut ui = Ui::new(
        Config::default(),
        Snapshot {
            phase: Phase::Checking,
            servers: vec![ready.clone()],
            ..Snapshot::default()
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
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
    assert!(rendered(&terminal).contains("Near · Ready"));
    assert!(rendered(&terminal).contains("Checking selected servers"));
    ui.update(Snapshot {
        servers: vec![ready, failed],
        ..Snapshot::default()
    });
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    assert!(rendered(&terminal).contains("Far · Failed"));
    assert!(!rendered(&terminal).contains("Checking selected servers"));
}

#[test]
fn server_chooser_discards_on_escape_and_checks_again_on_enter() {
    use crate::model::ServerSummary;
    let (commands, _received) = mpsc::channel(4);
    let server = |id: &str, checked: bool| ServerSummary {
        id: id.into(),
        name: id.into(),
        error: checked.then(|| "refused".into()),
        ..ServerSummary::default()
    };
    let mut ui = Ui::new(
        Config::default(),
        Snapshot {
            servers: vec![server("a", true), server("b", true), server("c", false)],
            ..Snapshot::default()
        },
    );
    let press = |ui: &mut Ui, code| ui.key(KeyEvent::new(code, KeyModifiers::NONE), &commands);
    press(&mut ui, KeyCode::Char('s'));
    assert_eq!(ui.popup, Popup::Servers);
    assert_eq!(ui.config.servers, ["a", "b"], "the default selection starts checked");
    for code in [KeyCode::Down, KeyCode::Down, KeyCode::Char(' '), KeyCode::Esc] {
        press(&mut ui, code);
    }
    assert_eq!(ui.popup, Popup::None);
    assert!(ui.config.servers.is_empty(), "esc applied the draft");
    assert!(ui.recheck.is_none());
    for code in [KeyCode::Char('s'), KeyCode::Char(' '), KeyCode::Enter] {
        press(&mut ui, code);
    }
    assert_eq!(ui.config.servers, ["b"]);
    assert!(ui.recheck.is_some(), "the new selection is checked");
}

#[test]
fn a_run_that_never_starts_keeps_the_last_results() {
    use crate::model::StageResult;
    let (commands, mut received) = mpsc::channel(4);
    let finished = Snapshot {
        phase: Phase::Complete,
        participants: vec!["self".into()],
        results: vec![StageResult {
            stage: Stage::Download,
            elapsed: Duration::from_secs(1),
            down: Some(download_measurement()),
            ..Default::default()
        }],
        plan: vec![Stage::Download],
        ..Snapshot::default()
    };
    let mut ui = Ui::new(Config::default(), finished);
    ui.live = true;
    let press = |ui: &mut Ui, code| ui.key(KeyEvent::new(code, KeyModifiers::NONE), &commands);
    let unstarted = |phase, error: Option<&str>| Snapshot {
        phase,
        error: error.map(Into::into),
        ..Snapshot::default()
    };
    press(&mut ui, KeyCode::Char('r'));
    assert!(matches!(received.try_recv(), Ok(Command::Run(_))));
    ui.update(unstarted(Phase::Preparing, None));
    let reason = "Test could not start: Server could not be reached";
    ui.update(unstarted(Phase::Failed, Some(reason)));
    assert!(ui.live && ui.snapshot.phase == Phase::Complete && ui.snapshot.results.len() == 1);
    assert_eq!(ui.notice(), (reason, true));

    press(&mut ui, KeyCode::Esc);
    press(&mut ui, KeyCode::Char('r'));
    assert!(matches!(received.try_recv(), Ok(Command::Run(_))));
    ui.update(unstarted(Phase::Preparing, None));
    ui.update(unstarted(Phase::Cancelled, None));
    assert!(!ui.live, "a run from setup that never starts returns there");
    assert_eq!(ui.notice(), ("Test stopped before it started.", false));
}

#[test]
fn reset_asks_first_and_keeps_the_catalogue_and_servers() {
    let (commands, _received) = mpsc::channel(4);
    let config = Config {
        url: "https://meter.example".into(),
        servers: vec!["near".into()],
        warmup: Duration::from_secs(1),
        insecure: true,
        ..Config::default()
    };
    let mut ui = Ui::new(config.clone(), Snapshot::default());
    let press = |ui: &mut Ui, code| ui.key(KeyEvent::new(code, KeyModifiers::NONE), &commands);
    ui.advanced = true;
    let reset = ui.fields().iter().position(|field| *field == Field::Reset);
    ui.rows.select(reset);
    press(&mut ui, KeyCode::Enter);
    assert_eq!(ui.config, config, "reset asks first");
    press(&mut ui, KeyCode::Char('r'));
    assert_eq!(ui.config, config, "another key keeps the settings");
    assert_eq!(ui.notice, "Settings kept.");
    press(&mut ui, KeyCode::Enter);
    press(&mut ui, KeyCode::Enter);
    assert_eq!(
        ui.config,
        Config {
            url: config.url,
            servers: config.servers,
            ..Config::default()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn path_settings_are_checked_again_once_changes_settle() {
    use graphite_meter_core::discovery::ThroughputTransport;
    let (commands, mut received) = mpsc::channel(4);
    let mut ui = Ui::new(Config::default(), Snapshot::default());
    let press = |ui: &mut Ui, code| ui.key(KeyEvent::new(code, KeyModifiers::NONE), &commands);
    let select = |ui: &mut Ui, field| {
        let row = ui.fields().iter().position(|shown| *shown == field);
        ui.rows.select(row);
    };

    select(&mut ui, Field::DownloadStage);
    press(&mut ui, KeyCode::Right);
    tokio::time::advance(RECHECK_DELAY).await;
    assert!(!ui.recheck(&commands), "a duration does not change the paths");

    select(&mut ui, Field::ThroughputTransport);
    for _ in 0..2 {
        press(&mut ui, KeyCode::Right);
        tokio::time::advance(RECHECK_DELAY / 2).await;
        assert!(!ui.recheck(&commands));
    }
    tokio::time::advance(RECHECK_DELAY / 2).await;
    assert!(ui.recheck(&commands));
    let Ok(Command::Verify(config)) = received.try_recv() else {
        panic!("changed paths were not checked again");
    };
    assert_eq!(config.throughput_transport, Some(ThroughputTransport::WebTransport));
    assert!(received.try_recv().is_err());

    ui.update(Snapshot {
        phase: Phase::Complete,
        ..Snapshot::default()
    });
    ui.live = true;
    press(&mut ui, KeyCode::Esc);
    assert!(!ui.live);
    tokio::time::advance(RECHECK_DELAY).await;
    assert!(ui.recheck(&commands), "setup after a run checks the paths again");
    assert!(matches!(received.try_recv(), Ok(Command::Verify(_))));
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
                browser_url: format!("https://meter.example/auth/cli?challenge={}TAIL", "x".repeat(300)),
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
    ui.key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE), &commands);
    assert!(matches!(received.try_recv(), Ok(Command::OpenBrowser)));
    ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &commands);
    assert!(matches!(received.try_recv(), Ok(Command::OpenBrowser)));
    ui.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), &commands);
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
fn escaping_sign_in_returns_to_setup_with_the_cancel_notice() {
    use crate::model::AuthPrompt;
    let prompt = AuthPrompt {
        deadline: tokio::time::Instant::now() + Duration::from_secs(120),
        origin: "https://meter.example".into(),
        code: "782411".into(),
        browser_url: "https://meter.example/auth/cli?challenge=x".into(),
    };
    for (live, phase, ended) in [
        (false, Phase::Checking, Phase::Setup),
        (true, Phase::Preparing, Phase::Cancelled),
    ] {
        let (commands, mut received) = mpsc::channel(4);
        let mut ui = Ui::new(
            Config::default(),
            Snapshot {
                phase,
                auth: Some(prompt.clone()),
                ..Snapshot::default()
            },
        );
        ui.live = live;
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands);
        assert!(matches!(received.try_recv(), Ok(Command::Cancel)));
        for phase in [phase, ended] {
            ui.update(Snapshot {
                phase,
                ..Snapshot::default()
            });
            assert_eq!(ui.notice().0, "Sign-in canceled. Press v to request a new code.");
        }
        assert!(!ui.live);
    }
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
            ..Default::default()
        }],
        latency_focus: Some("near".into()),
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
    assert!(rendered.contains("Download: ✓ 12.00 Mbit/s"));
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
        direction: Direction::Down,
        total_bytes: 1_500_000,
        mean_bytes_per_sec: Some(1_500_000.0),
        peak_bytes_per_sec: Some(1_500_000.0),
        samples: 4,
        elapsed_nanos: Some(1_000_000_000),
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
                server_latencies: vec![
                    ServerLatencyResult {
                        elapsed: Some(Duration::from_secs(1)),
                        id: "dropped".into(),
                        summary: loaded.snapshot(),
                        ending: None,
                    },
                    ServerLatencyResult {
                        elapsed: Some(Duration::from_secs(1)),
                        id: "self".into(),
                        summary: idle.snapshot(),
                        ending: None,
                    },
                ],
                ..Default::default()
            },
            StageResult {
                stage: Stage::Download,
                elapsed: Duration::from_secs(1),
                down: Some(download_measurement()),
                server_latencies: vec![ServerLatencyResult {
                    elapsed: Some(Duration::from_secs(1)),
                    id: "self".into(),
                    summary: loaded.snapshot(),
                    ending: None,
                }],
                ..Default::default()
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
    assert_eq!(snapshot.history.points.front().unwrap().elapsed, Duration::ZERO);
    assert!(snapshot.history.points.len() <= 480);
    snapshot.server_latencies.push(host);
    snapshot.latency_focus = Some("self".into());
    snapshot.plan = vec![Stage::Latency, Stage::Download];
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
    for text in ["Throughput", "Latency · ms", "−0.3 ms", "Probe timeouts", "120 s"] {
        assert!(rendered.contains(text), "missing {text}: {rendered}");
    }
}
