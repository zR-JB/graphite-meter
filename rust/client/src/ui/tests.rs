use super::setup::Field;
use super::*;
use crate::model::Stage;
use graphite_meter_core::discovery::{LatencyTransport, Protocol, ThroughputTransport};

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
                down_bytes: 0,
                up_bytes: 0,
                down_bps: None,
                up_bps: None,
                latency: LatencySummary::default(),
                complete: true,
                server_latencies: vec![ServerLatencyResult {
                    id: "self".into(),
                    summary: LatencySummary {
                        distribution: Some(Distribution {
                            min: rtt,
                            max: rtt,
                            mean: rtt,
                            p10: rtt,
                            p50: rtt,
                            p90: rtt,
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
    assert!(rendered.contains("1.50 ms"));

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
    assert!(!rendered.contains("1.50 ms"));
}

#[test]
fn rejected_run_command_keeps_the_setup_visible() {
    let (commands, mut receiver) = mpsc::channel(1);
    let mut ui = Ui::new(Config::default(), Snapshot::default());
    commands.try_send(Command::Cancel).unwrap();

    ui.key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        &commands,
    );
    assert!(!ui.live);
    assert!(!ui.awaiting);
    assert_eq!(ui.notice().0, "Controller is busy; try again.");

    assert!(matches!(receiver.try_recv(), Ok(Command::Cancel)));
    ui.key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        &commands,
    );
    assert!(ui.live);
    assert!(ui.awaiting);
    assert!(matches!(receiver.try_recv(), Ok(Command::Run(_))));
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
    assert_eq!(ui.notice().0, "Run continues.");
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
fn setup_shows_checked_paths_and_marks_changed_settings_stale() {
    use crate::model::ServerSummary;
    use graphite_meter_core::discovery::{LatencyTarget, ThroughputTarget};
    use ratatui::{Terminal, backend::TestBackend};

    let config = Config::default();
    let server = ServerSummary {
        id: "self".into(),
        name: "Local peer".into(),
        origin: "https://meter.example".into(),
        throughput: Some(ThroughputTarget {
            base_url: "https://meter.example".into(),
            transport: ThroughputTransport::FetchStream,
            protocol: Protocol::Http2,
        }),
        latency: Some(LatencyTarget {
            base_url: "https://meter.example".into(),
            transport: LatencyTransport::WebTransport,
        }),
        ..ServerSummary::default()
    };
    let mut ui = Ui::new(
        config,
        Snapshot {
            servers: vec![server],
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
    assert!(rendered(&terminal).contains("↓ Fetch stream · HTTP/2 · TLS"));
    assert!(rendered(&terminal).contains("RTT WebTransport · HTTP/3 · TLS"));

    let mut narrow = Terminal::new(TestBackend::new(80, 24)).unwrap();
    narrow.draw(|frame| ui.draw(frame)).unwrap();
    assert!(rendered(&narrow).contains("↓ Fetch stream · HTTP/2 · TLS"));

    ui.config.throughput_protocol = Some(Protocol::Http1);
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    assert!(rendered(&terminal).contains("Settings changed · verify again"));
    assert!(!rendered(&terminal).contains("↓ Fetch stream"));

    let (commands, _receiver) = mpsc::channel(1);
    ui.send(Command::Verify(ui.config.clone()), &commands);
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    assert!(rendered(&terminal).contains("Checking selected servers"));
    assert!(!rendered(&terminal).contains("↓ Fetch stream"));
}

#[test]
fn approval_takes_priority_over_editing_and_keeps_long_browser_urls_reachable() {
    use crate::model::AuthPrompt;
    use ratatui::{Terminal, backend::TestBackend};

    let mut ui = Ui::new(
        Config::default(),
        Snapshot {
            phase: Phase::Preparing,
            auth: Some(AuthPrompt {
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
    ui.chooser = true;
    let mut terminal = Terminal::new(TestBackend::new(45, 12)).unwrap();
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
    assert!(rendered(&terminal).contains("Confirmation code: 782411"));
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
    assert!(rendered(&terminal).contains("Confirmation code: 782411"));
    ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands);
    assert!(matches!(received.try_recv(), Ok(Command::Cancel)));
    assert_eq!(ui.edit.as_ref().unwrap().text(), "original");

    let browser_url = ui.snapshot.auth.as_ref().unwrap().browser_url.clone();
    ui.update(Snapshot {
        auth: Some(AuthPrompt {
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
fn editing_is_unicode_safe_and_bounded() {
    let mut edit = Edit::new(Field::Url, "a🦀b".into());
    edit.key(KeyCode::Left);
    edit.key(KeyCode::Backspace);
    assert_eq!(edit.text(), "ab");
    edit.insert("\x1b\n界");
    assert_eq!(edit.text(), "a界b");
    edit.insert(&"x".repeat(MAX_TEXT * 2));
    assert_eq!(edit.chars.len(), MAX_TEXT);
}
#[test]
fn editing_keeps_the_whole_value_visible_when_it_fits() {
    use ratatui::{Terminal, backend::TestBackend};

    let url = "https://meter.example/some/moderately/long/path";
    let mut ui = Ui::new(Config::default(), Snapshot::default());
    ui.edit = Some(Edit::new(Field::Url, url.into()));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains(url));

    let edit = Edit::new(Field::Url, "界".repeat(30));
    let (before, cursor, after) = edit.viewport(20);
    assert!(before.width() + cursor.width().unwrap_or(0) + after.width() <= 20);
    assert!(before.ends_with("界"));
}
#[test]
fn sections_cycle_through_setup_and_live_in_both_directions() {
    let mut ui = Ui::new(Config::default(), Snapshot::default());
    for page in 1..PAGES.len() {
        ui.change_section(1);
        assert_eq!(ui.page, page);
        assert!(!ui.live);
    }
    ui.change_section(1);
    assert!(ui.live);
    ui.change_section(1);
    assert_eq!(ui.page, 0);
    assert!(!ui.live);
    ui.change_section(-1);
    assert!(ui.live);
}
#[test]
fn current_setup_notice_is_visible_after_a_failed_run() {
    let mut ui = Ui::new(Config::default(), Snapshot::default());
    let failed = Snapshot {
        error: Some("old transfer error".into()),
        ..Snapshot::default()
    };
    ui.update(failed);
    assert_eq!(ui.notice(), ("old transfer error", true));

    ui.notice = "Transport paths set to automatic.".into();
    assert_eq!(ui.notice(), ("Transport paths set to automatic.", false));

    let next = Snapshot {
        error: Some("new transfer error".into()),
        ..Snapshot::default()
    };
    ui.update(next);
    assert_eq!(ui.notice(), ("new transfer error", true));
}
#[test]
fn details_show_both_server_contributions_and_close_with_escape() {
    use crate::model::{ServerContribution, ServerSummary, StageResult};
    use ratatui::{Terminal, backend::TestBackend};

    let snapshot = Snapshot {
        phase: Phase::Complete,
        results: vec![StageResult {
            stage: Stage::Download,
            elapsed: Duration::from_secs(1),
            down_bytes: 1_500_000,
            up_bytes: 0,
            down_bps: Some(12_000_000.0),
            up_bps: None,
            latency: Default::default(),
            complete: false,
            server_latencies: Vec::new(),
            server_results: vec![
                ServerContribution {
                    id: "near".into(),
                    down_bytes: 1_500_000,
                    up_bytes: 0,
                    down_bps: Some(12_000_000.0),
                    up_bps: None,
                    error: None,
                },
                ServerContribution {
                    id: "far".into(),
                    down_bytes: 0,
                    up_bytes: 0,
                    down_bps: None,
                    up_bps: None,
                    error: Some("peer disconnected".into()),
                },
            ],
        }],
        servers: vec![
            ServerSummary {
                id: "near".into(),
                name: "Near".into(),
                ..ServerSummary::default()
            },
            ServerSummary {
                id: "far".into(),
                name: "Far".into(),
                ..ServerSummary::default()
            },
        ],
        ..Snapshot::default()
    };
    let mut ui = Ui::new(Config::default(), snapshot);
    ui.live = true;
    let (commands, _) = mpsc::channel(1);
    ui.key(
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        &commands,
    );
    assert!(ui.details);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Near"));
    assert!(rendered.contains("Far"));
    assert!(rendered.contains("peer disconnected"));
    assert!(rendered.contains("12.00 Mbit/s"));
    ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands);
    assert!(!ui.details);
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
            down_bytes: 1_500_000,
            up_bytes: 0,
            down_bps: Some(12_000_000.0),
            up_bps: None,
            latency: Default::default(),
            complete: true,
            server_latencies: Vec::new(),
            server_results: Vec::new(),
        }],
        ..Snapshot::default()
    };
    let mut ui = Ui::new(Config::default(), snapshot);
    ui.live = true;
    let mut terminal = Terminal::new(TestBackend::new(45, 12)).unwrap();
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Upload · 3.0s"));
    assert!(rendered.contains("↑ 12.00 Mbit/s"));
    assert!(rendered.contains("RTT 25.00 ms"));
    assert!(rendered.contains("Download: ↓ 12.00 Mbit/s"));
    assert!(rendered.contains("d details"));

    ui.snapshot.phase = Phase::Complete;
    terminal.draw(|frame| ui.draw(frame)).unwrap();
    let completed = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(completed.contains("r rerun"));
}
