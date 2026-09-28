use super::setup::Field;
use super::*;
use crate::model::{AuthPrompt, ServerLatencyResult, ServerSummary, Stage, StageResult};
use graphite_meter_core::{
    latency::{Distribution, LatencyAccumulator, LatencySummary, ProbeOutcome},
    measurement::{Direction, MeasurementResult},
};
use ratatui::{Terminal, backend::TestBackend};

/// The screen `ui` draws at this size, one string per row.
pub(super) fn rows(ui: &mut Ui, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(usize::from(width))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect()
}

fn screen(ui: &mut Ui, width: u16, height: u16) -> String {
    rows(ui, width, height).concat()
}

fn press(ui: &mut Ui, commands: &mpsc::Sender<Command>, codes: &[KeyCode]) {
    for code in codes {
        ui.key(KeyEvent::new(*code, KeyModifiers::NONE), commands);
    }
}

fn select(ui: &mut Ui, field: Field) {
    let row = ui.fields().iter().position(|shown| *shown == field);
    ui.rows.select(row);
}

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

/// A finished idle latency stage to one server, whose every reply took `rtt` nanoseconds.
pub(crate) fn latency_snapshot(rtt: u64) -> Snapshot {
    let summary = LatencySummary {
        distribution: Some(Distribution {
            min: rtt,
            max: rtt,
            mean: rtt,
            p50: rtt,
            p95: rtt,
        }),
        count: 4,
        ..LatencySummary::default()
    };
    Snapshot {
        phase: Phase::Complete,
        results: vec![StageResult {
            stage: Stage::Latency,
            elapsed: Duration::from_secs(1),
            server_latencies: vec![latency_result("self", summary)],
            ..Default::default()
        }],
        participants: vec!["self".into()],
        latency_focus: Some("self".into()),
        plan: vec![Stage::Latency],
        ..Snapshot::default()
    }
}

fn prompt(code: &str, browser_url: String) -> AuthPrompt {
    AuthPrompt {
        deadline: tokio::time::Instant::now() + Duration::from_secs(120),
        origin: "https://meter.example".into(),
        code: code.into(),
        browser_url,
    }
}

#[test]
fn finished_latency_result_remains_visible_after_live_probes_end() {
    let mut ui = Ui::new(Config::default(), latency_snapshot(1_500_000));
    ui.live = true;
    assert!(screen(&mut ui, 100, 30).contains("1.5 ms"));

    // A selected peer absent from this stage must not inherit another
    // peer's RTT merely because its summary is first in the result.
    ui.latency_pick = Some("other".into());
    assert!(!screen(&mut ui, 100, 30).contains("1.5 ms"));
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
    press(&mut ui, &commands, &[KeyCode::Esc]);
    assert_eq!(ui.cancel, CancelState::Confirming);
    assert!(received.try_recv().is_err());
    press(&mut ui, &commands, &[KeyCode::Tab]);
    assert_eq!(ui.cancel, CancelState::Idle);
    assert!(!ui.notice().0.is_empty());
    assert!(received.try_recv().is_err());

    press(&mut ui, &commands, &[KeyCode::Esc, KeyCode::Esc]);
    assert_eq!(ui.cancel, CancelState::Requested);
    assert!(matches!(received.try_recv(), Ok(Command::Cancel)));
    press(&mut ui, &commands, &[KeyCode::Esc]);
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
    press(&mut ui, &commands, &[KeyCode::Esc]);
    assert!(matches!(received.try_recv(), Ok(Command::Cancel)));
}

#[test]
fn reenabled_stage_runs_in_canonical_order() {
    let (commands, mut received) = mpsc::channel(4);
    let mut ui = Ui::new(Config::default(), Snapshot::default());
    select(&mut ui, Field::LatencyStage);
    press(
        &mut ui,
        &commands,
        &[KeyCode::Char(' '), KeyCode::Char(' '), KeyCode::Char('r')],
    );
    let Ok(Command::Run(config)) = received.try_recv() else {
        panic!("run was not requested");
    };
    assert_eq!(config.stages, Config::default().stages);
}

#[test]
fn setup_keys_work_during_the_path_check() {
    use graphite_meter_core::discovery::ThroughputTransport;
    let (commands, mut received) = mpsc::channel(8);
    let checking = || Snapshot {
        phase: Phase::Checking,
        ..Snapshot::default()
    };
    let mut ui = Ui::new(Config::default(), checking());

    press(&mut ui, &commands, &[KeyCode::Char('s')]);
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
    press(&mut ui, &commands, &[KeyCode::Esc]);

    ui.update(checking());
    select(&mut ui, Field::BidiStage);
    press(&mut ui, &commands, &[KeyCode::Char(' ')]);
    assert!(ui.config.stages.contains(&Stage::Bidirectional));
    press(&mut ui, &commands, &[KeyCode::Char('v')]);
    assert!(matches!(received.try_recv(), Ok(Command::Verify(_))));
    ui.config.throughput_transport = Some(ThroughputTransport::FetchStream);
    press(&mut ui, &commands, &[KeyCode::Char('a')]);
    assert_eq!(ui.config.throughput_transport, None);

    ui.update(checking());
    ui.rows.select(Some(0));
    press(&mut ui, &commands, &[KeyCode::Enter]);
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
        select(&mut ui, field);
        press(&mut ui, &commands, &[KeyCode::Enter]);
        let opened = ui.edit.as_ref().map(|edit| edit.text());
        assert!(
            opened.as_ref().is_some_and(|text| text.ends_with('s')),
            "{typed}: {opened:?}"
        );
        ui.edit = Some(Edit::new(field, typed.into()));
        press(&mut ui, &commands, &[KeyCode::Enter]);
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
                press(&mut ui, &commands, &[KeyCode::Esc]);
            }
        }
    }
    select(&mut ui, Field::DownloadStage);
    press(&mut ui, &commands, &[KeyCode::Char(' ')]);
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
    use graphite_meter_core::discovery::{LatencyTarget, LatencyTransport};
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
    let checking = screen(&mut ui, 100, 30);
    assert!(checking.contains("Near · Ready"));
    assert!(checking.contains("Checking selected servers"));
    ui.update(Snapshot {
        servers: vec![ready, failed],
        ..Snapshot::default()
    });
    let checked = screen(&mut ui, 100, 30);
    assert!(checked.contains("Far · Failed"));
    assert!(!checked.contains("Checking selected servers"));
}

#[test]
fn server_chooser_discards_on_escape_and_checks_again_on_enter() {
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
    press(&mut ui, &commands, &[KeyCode::Char('s')]);
    assert_eq!(ui.popup, Popup::Servers);
    assert_eq!(ui.config.servers, ["a", "b"], "the default selection starts checked");
    press(
        &mut ui,
        &commands,
        &[KeyCode::Down, KeyCode::Down, KeyCode::Char(' '), KeyCode::Esc],
    );
    assert_eq!(ui.popup, Popup::None);
    assert!(ui.config.servers.is_empty(), "esc applied the draft");
    assert!(ui.recheck.is_none());
    press(
        &mut ui,
        &commands,
        &[KeyCode::Char('s'), KeyCode::Char(' '), KeyCode::Enter],
    );
    assert_eq!(ui.config.servers, ["b"]);
    assert!(ui.recheck.is_some(), "the new selection is checked");
}

#[test]
fn a_run_that_never_starts_keeps_the_last_results() {
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
    let unstarted = |phase, error: Option<&str>| Snapshot {
        phase,
        error: error.map(Into::into),
        ..Snapshot::default()
    };
    press(&mut ui, &commands, &[KeyCode::Char('r')]);
    assert!(matches!(received.try_recv(), Ok(Command::Run(_))));
    ui.update(unstarted(Phase::Preparing, None));
    let reason = "Test could not start: Server could not be reached";
    ui.update(unstarted(Phase::Failed, Some(reason)));
    assert!(ui.live && ui.snapshot.phase == Phase::Complete && ui.snapshot.results.len() == 1);
    assert_eq!(ui.notice(), (reason, true));

    press(&mut ui, &commands, &[KeyCode::Esc, KeyCode::Char('r')]);
    assert!(matches!(received.try_recv(), Ok(Command::Run(_))));
    ui.update(unstarted(Phase::Preparing, None));
    ui.update(unstarted(Phase::Cancelled, None));
    assert!(!ui.live, "a run from setup that never starts returns there");
    assert_eq!(ui.notice(), ("Test stopped before it started.", false));
}

#[test]
fn details_open_for_the_whole_run_and_l_is_offered_for_several_servers() {
    let (commands, _received) = mpsc::channel(4);
    let mut ui = Ui::new(
        Config::default(),
        Snapshot {
            phase: Phase::Preparing,
            ..Snapshot::default()
        },
    );
    ui.live = true;
    press(&mut ui, &commands, &[KeyCode::Char('d')]);
    assert_eq!(ui.popup, Popup::Details);
    assert!(screen(&mut ui, 100, 30).contains("Waiting for the first server report"));
    press(&mut ui, &commands, &[KeyCode::Esc]);
    let checked = |id: &str| ServerSummary {
        id: id.into(),
        name: id.into(),
        error: Some("refused".into()),
        ..Default::default()
    };
    // As in Go, a server that left the run still counts among its servers.
    for (servers, participants, several) in [
        (vec!["a"], vec!["a"], false),
        (vec!["a", "b"], vec!["a", "b"], true),
        (vec!["a", "b"], vec!["a"], true),
    ] {
        ui.update(Snapshot {
            phase: Phase::Measuring,
            participants: participants.into_iter().map(Into::into).collect(),
            servers: servers.into_iter().map(checked).collect(),
            ..Snapshot::default()
        });
        let screen = screen(&mut ui, 100, 30);
        assert_eq!(screen.contains("l Latency server"), several);
        assert_eq!(screen.contains("l switches server"), several);
    }
}

#[test]
fn page_keys_scroll_the_panels_that_overflow() {
    use graphite_meter_core::discovery::{LatencyTarget, LatencyTransport};
    let (commands, _received) = mpsc::channel(4);
    let servers = (0..4)
        .map(|index| ServerSummary {
            id: format!("s{index}"),
            name: format!("Server {index}"),
            latency: Some(LatencyTarget {
                base_url: "http://127.0.0.1:1".into(),
                transport: LatencyTransport::WebSocket,
            }),
            ..ServerSummary::default()
        })
        .collect();
    let mut ui = Ui::new(
        Config::default(),
        Snapshot {
            servers,
            ..Snapshot::default()
        },
    );
    let top = screen(&mut ui, 80, 24);
    assert!(top.contains("PgDn more") && !top.contains("TLS verification"));
    press(&mut ui, &commands, &[KeyCode::End]);
    let end = screen(&mut ui, 80, 24);
    assert!(end.contains("TLS verification") && !end.contains("Catalogue default selection"));
    press(&mut ui, &commands, &[KeyCode::Home]);
    assert!(screen(&mut ui, 80, 24).contains("Catalogue default selection"));

    let result = |stage: Stage| StageResult {
        stage,
        elapsed: Duration::from_secs(1),
        down: stage.downloads().then(download_measurement),
        up: stage.uploads().then(download_measurement),
        server_latencies: vec![latency_result("s0", probes(&[500_000], 0))],
        ..Default::default()
    };
    let stages = [Stage::Latency, Stage::Download, Stage::Upload, Stage::Bidirectional];
    ui.update(Snapshot {
        phase: Phase::Complete,
        participants: vec!["s0".into()],
        latency_focus: Some("s0".into()),
        results: stages.map(result).into(),
        plan: stages.into(),
        ..Snapshot::default()
    });
    ui.live = true;
    let top = screen(&mut ui, 40, 20);
    assert!(top.contains("PgDn more"), "{top}");
    press(&mut ui, &commands, &[KeyCode::Down]);
    assert_ne!(screen(&mut ui, 40, 20), top, "the results scroll in the run view");
    press(&mut ui, &commands, &[KeyCode::Home]);
    assert_eq!(screen(&mut ui, 40, 20), top);
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
    ui.advanced = true;
    select(&mut ui, Field::Reset);
    press(&mut ui, &commands, &[KeyCode::Enter]);
    assert_eq!(ui.config, config, "reset asks first");
    press(&mut ui, &commands, &[KeyCode::Char('r')]);
    assert_eq!(ui.config, config, "another key keeps the settings");
    assert_eq!(ui.notice, "Settings kept.");
    press(&mut ui, &commands, &[KeyCode::Enter, KeyCode::Enter]);
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

    select(&mut ui, Field::DownloadStage);
    press(&mut ui, &commands, &[KeyCode::Right]);
    tokio::time::advance(RECHECK_DELAY).await;
    assert!(!ui.recheck(&commands), "a duration does not change the paths");

    select(&mut ui, Field::ThroughputTransport);
    for _ in 0..2 {
        press(&mut ui, &commands, &[KeyCode::Right]);
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
    press(&mut ui, &commands, &[KeyCode::Esc]);
    assert!(!ui.live);
    tokio::time::advance(RECHECK_DELAY).await;
    assert!(ui.recheck(&commands), "setup after a run checks the paths again");
    assert!(matches!(received.try_recv(), Ok(Command::Verify(_))));
}

#[tokio::test(start_paused = true)]
async fn approval_takes_priority_over_editing_and_keeps_long_browser_urls_reachable() {
    let browser_url = format!("https://meter.example/auth/cli?challenge={}TAIL", "x".repeat(300));
    let mut ui = Ui::new(
        Config::default(),
        Snapshot {
            phase: Phase::Preparing,
            auth: Some(prompt("782411", browser_url.clone())),
            ..Snapshot::default()
        },
    );
    ui.edit = Some(Edit::new(Field::Url, "original".into()));
    ui.help = true;
    ui.popup = Popup::Servers;
    assert!(screen(&mut ui, 40, 12).contains("Match this code: 782411"));
    tokio::time::advance(Duration::from_secs(30)).await;
    let waited = screen(&mut ui, 40, 12);
    assert!(waited.contains("waited 30 s · expires in 90 s"));
    assert!(waited.contains("Enter/Space/o open"));
    assert!(!waited.contains("TAIL"));

    let (commands, mut received) = mpsc::channel(4);
    ui.paste("ignored");
    assert_eq!(ui.edit.as_ref().unwrap().text(), "original");
    for code in [KeyCode::Char('o'), KeyCode::Enter, KeyCode::Char(' ')] {
        press(&mut ui, &commands, &[code]);
        assert!(matches!(received.try_recv(), Ok(Command::OpenBrowser)));
    }
    press(&mut ui, &commands, &[KeyCode::Down; 12]);
    let scrolled = screen(&mut ui, 40, 12);
    assert!(scrolled.contains("TAIL"));
    assert!(scrolled.contains("Match this code: 782411"));
    press(&mut ui, &commands, &[KeyCode::Esc]);
    assert!(matches!(received.try_recv(), Ok(Command::Cancel)));
    assert_eq!(ui.edit.as_ref().unwrap().text(), "original");

    ui.update(Snapshot {
        auth: Some(prompt("999999", browser_url)),
        ..Snapshot::default()
    });
    assert_eq!(ui.auth_scroll, 0);
}

#[test]
fn escaping_sign_in_returns_to_setup_with_the_cancel_notice() {
    let browser_url = "https://meter.example/auth/cli?challenge=x".to_owned();
    for (live, phase, ended) in [
        (false, Phase::Checking, Phase::Setup),
        (true, Phase::Preparing, Phase::Cancelled),
    ] {
        let (commands, mut received) = mpsc::channel(4);
        let mut ui = Ui::new(
            Config::default(),
            Snapshot {
                phase,
                auth: Some(prompt("782411", browser_url.clone())),
                ..Snapshot::default()
            },
        );
        ui.live = live;
        press(&mut ui, &commands, &[KeyCode::Esc]);
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
    use crate::model::{Point, ServerLatency};
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
    let rendered = screen(&mut ui, 40, 12);
    assert!(rendered.contains("Upload · 3.0 s"));
    assert!(rendered.contains("Upload 12.00 Mbit/s"));
    assert!(rendered.contains("Latency 25.0 ms"));
    assert!(rendered.contains("Download: ✓ 12.00 Mbit/s"));
    assert!(rendered.contains("d Details"));

    ui.help = true;
    let expanded = screen(&mut ui, 40, 12);
    assert!(expanded.contains("Tab/Shift-Tab"));
    assert!(expanded.contains("Ctrl-C stop"));
    assert_eq!(ui.popup, Popup::None);
    ui.help = false;
    ui.snapshot.phase = Phase::Complete;
    assert!(screen(&mut ui, 40, 12).contains("Enter Run again"));
}

#[test]
fn stacked_run_keeps_charts_and_signed_loaded_latency_visible() {
    use crate::model::{Point, ServerLatency};
    let (idle, loaded) = (probes(&[500_000], 0), probes(&[200_000], 0));
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
                server_latencies: vec![latency_result("dropped", loaded), latency_result("self", idle)],
                ..Default::default()
            },
            StageResult {
                stage: Stage::Download,
                elapsed: Duration::from_secs(1),
                down: Some(download_measurement()),
                server_latencies: vec![latency_result("self", loaded)],
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
    let rendered = screen(&mut ui, 80, 24);
    for text in ["Throughput", "Latency · ms", "−0.3 ms", "Probe timeouts", "120 s"] {
        assert!(rendered.contains(text), "missing {text}: {rendered}");
    }
}
