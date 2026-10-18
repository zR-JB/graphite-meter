//! Prints the console after a fixed simulated run as JSON cells in both themes, for the README screenshots.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use graphite_meter_client::{
    config::Config,
    events::{Event, Probed},
    measure::{
        aggregate::Rate,
        latency::{Latency, Population, ProbeOutcome},
    },
    model::{Cadence, Dir, Outcome, ServerResult, Stage, StageResult, Throughput},
    net::ThroughputPath,
    run::{
        engine::{Member, StagePlan},
        prepare::{Paths, ServerPath},
    },
    style::{Background, Palette},
    text::Profile,
    tui::App,
};
use graphite_meter_proto::{
    catalog::{ServerEntry, ServerId},
    discovery::{IpFamily, Protocol, ThroughputTransport},
    origin::{BaseUrl, Origin},
};
use ratatui_core::{buffer::Buffer, layout::Rect, style::Color, style::Modifier};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const SIZE: Rect = Rect::new(0, 0, 120, 40);
const TICK: Duration = Duration::from_millis(250);
/// Bytes per second each way, and what the stages moved.
const RATE: f64 = 2.35e9 / 8.0;
const PEAK: Dir<f64> = Dir { down: 2.36e9 / 8.0, up: 2.38e9 / 8.0 };
const BYTES: Dir<u64> = Dir { down: 2_900_000_000, up: 3_000_000_000 };

/// `count` round trips in µs within `spread` of `median`, a few `tail` slower or a fifth of it faster, the same every
/// run: a linear congruential walk.
fn rtts(median: u64, spread: u64, tail: u64, count: usize) -> impl Iterator<Item = Duration> {
    let mut state = median;
    (0..count).map(move |_| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let micros = median + (state >> 33) % (2 * spread + 1) - spread;
        Duration::from_micros(match (state >> 20) % 97 {
            0 => micros + tail,
            1 => micros - tail / 5,
            _ => micros,
        })
    })
}

fn console(palette: Palette) -> Buffer {
    let now = Instant::now();
    let home = ServerId::parse("self").unwrap();
    let origin = Origin::parse("https://meter.home.arpa:7247").unwrap();
    let config = Config { url: Some(origin.clone()), ..Config::default() };
    let mut app = App::new(config.clone(), None, (Profile::TrueColor, palette), now);
    app.tick(now);
    let paths = Paths {
        throughput: ThroughputPath {
            origin: origin.clone(),
            transport: ThroughputTransport::FetchStream,
            protocol: Protocol::Http2,
        },
        control: Protocol::Http2,
        latency: None,
        stage_limit: Duration::from_secs(300),
        idle_rtt: Some(Duration::from_micros(400)),
        ip_family: IpFamily::V4,
    };
    let server = ServerPath {
        id: home.clone(),
        name: "Home".into(),
        location: String::new(),
        origin: origin.clone(),
        offered: None,
        path: Ok(paths),
    };
    let entry = ServerEntry {
        id: home.clone(),
        url: BaseUrl::Origin(origin),
        name: "Home".into(),
        location: String::new(),
        additional_origins: Vec::new(),
    };
    app.event(&Event::Prepared { servers: Arc::new([server]), catalog: Arc::new([entry]) }, now);
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), now);
    app.event(&Event::RunStarted { plan: config.plan(), focus: home.clone(), at: now }, now);

    let mut at = now;
    for (stage, median, spread, tail, replies) in [
        (Stage::Latency, 400, 40, 300, 10_852),
        (Stage::Download, 3_100, 300, 900, 40),
        (Stage::Upload, 3_200, 300, 900, 40),
    ] {
        let duration = config.duration(stage);
        let cadence = if stage == Stage::Latency { config.ping } else { Cadence::Every(TICK) };
        let members = vec![Member { server: home.clone(), warmup: config.warmup }];
        app.event(&Event::StageStarted(StagePlan { stage, members, duration, latency: Some(cadence) }), at);
        at += config.warmup;
        app.event(&Event::Measuring(stage), at);
        let ticks = (duration.as_millis() / TICK.as_millis()) as usize;
        let (mut latency, mut rtt) = (Latency::default(), rtts(median, spread, tail, replies));
        for tick in 1..=ticks {
            let into = TICK * tick as u32;
            let due = replies * tick / ticks - replies * (tick - 1) / ticks;
            let probes: Box<[Probed]> = (&mut rtt)
                .take(due)
                .enumerate()
                .map(|(index, rtt)| {
                    latency.record(ProbeOutcome::Reply { rtt, handling: Duration::ZERO });
                    Probed {
                        at: into - TICK + TICK * index as u32 / due as u32,
                        rtt: Some(rtt),
                    }
                })
                .collect();
            app.event(&Event::Probes { server: home.clone(), probes, latency: latency.summary() }, at + into);
            let wander = RATE * (1.0 + 0.003 * (tick % 5) as f64 - 0.006);
            let live = |moves: bool| moves.then_some(wander);
            let rates = Dir {
                down: live(stage == Stage::Download),
                up: live(stage == Stage::Upload),
            };
            if stage != Stage::Latency {
                app.event(&Event::Sample { at: into, rates }, at + into);
            }
        }
        at += duration;
        let moved = |dir: Stage, peak: f64, bytes: u64| {
            (stage == dir).then_some(Throughput { rate: Some(Rate { mean: RATE, peak }), bytes })
        };
        let throughput = Dir {
            down: moved(Stage::Download, PEAK.down, BYTES.down),
            up: moved(Stage::Upload, PEAK.up, BYTES.up),
        };
        let population = Population { summary: latency.summary(), complete: true };
        let servers = vec![ServerResult {
            server: home.clone(),
            left: false,
            throughput,
            latency: Some(population),
        }];
        let result = StageResult {
            stage,
            measured: duration,
            stopped: false,
            throughput,
            servers,
            failures: Vec::new(),
        };
        app.event(&Event::StageFinished(result), at);
    }
    let elapsed = Duration::from_millis(28_600);
    app.event(&Event::RunFinished { outcome: Outcome::Complete, error: None, elapsed }, now + elapsed);
    let mut buffer = Buffer::empty(SIZE);
    app.draw(&mut buffer, now + elapsed + Duration::from_secs(5));
    buffer
}

fn hex(color: Color, default: &str) -> String {
    match color {
        Color::Rgb(red, green, blue) => format!("#{red:02x}{green:02x}{blue:02x}"),
        _ => default.into(),
    }
}

fn cells(buffer: &Buffer, palette: Palette) -> Value {
    let rgb = |rgb: u32| format!("#{:06x}", rgb & 0xff_ff_ff);
    let (canvas, text) = (rgb(palette.canvas().rgb), rgb(palette.text.fg.map_or(0, |color| color.rgb)));
    let rows: Vec<Value> = buffer
        .content()
        .chunks(usize::from(SIZE.width))
        .map(|row| {
            let cells = row.iter().map(|cell| {
                let bold = cell.modifier.contains(Modifier::BOLD);
                json!([cell.symbol(), hex(cell.fg, &text), hex(cell.bg, &canvas), bold])
            });
            Value::Array(cells.collect())
        })
        .collect();
    json!({ "canvas": canvas, "rows": rows })
}

fn main() {
    let themes = [("dark", [0x0d, 0x10, 0x13]), ("light", [0xf1, 0xf2, 0xf5])].map(|(name, background)| {
        let palette = Palette::over(Background(Some(background)));
        (name.to_owned(), cells(&console(palette), palette))
    });
    println!("{}", Value::Object(themes.into_iter().collect()));
}
