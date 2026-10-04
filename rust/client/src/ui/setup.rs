//! Go's setup list (setup.go): its settings, what each row shows, and what its keys change.
use super::{Command, MAX_TEXT, PathState, Popup, Ui};
use crate::{
    config::{Config, MAX_STREAMS, STAGE_BOUND},
    model::{ServerSummary, Stage},
    report::{cell, plain, span},
    vocabulary::{self as words, CADENCES, MISSING, wire},
};
use graphite_meter_core::{
    catalog::MAX_SELECTED_SERVERS,
    discovery::{
        Capabilities, LatencyTarget,
        Protocol::{self, Http1, Http2, Http3},
        ThroughputTarget, ThroughputTransport,
    },
    duration::{go_duration, parse_go_duration, short_duration},
    origin::{canonical_origin, catalog_origin, target_origin},
    text::terminal_character,
};
use ratatui_core::text::{Line, Span};
use std::{ops::RangeInclusive, time::Duration};
use tokio::sync::mpsc;

const WARMUP_BOUND: RangeInclusive<Duration> = Duration::ZERO..=Duration::from_secs(4);

/// A setup row. `Path(true)` and `Cadence(true)` are the latency path and the loaded cadence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Setting {
    Start,
    Catalogue,
    Servers,
    Path(bool),
    Protocol,
    Stage(Stage),
    LoadedLatency,
    Advanced,
    Warmup,
    Cadence(bool),
    ForceStreams,
    Streams,
    Insecure,
    Reset,
}

/// Go's setupGroups in order: the heading of the group a row starts, the row, its label, which its notices
/// repeat, and its help; a path's help goes on to its choices, a stage's to its keys. Advanced hides the rows after it.
#[rustfmt::skip]
pub(super) const ROWS: [(Option<&str>, Setting, &str, &str); 19] = [
    (Some(""), Setting::Start, "Start test", "Runs the checked stages in order. r starts from any row."),
    (Some("Connection"), Setting::Catalogue, "Catalogue URL", "Origin that lists the test servers. enter types one."),
    (None, Setting::Servers, "Test servers", "Measured at once; their speeds add up. enter picks up to 4."),
    (None, Setting::Path(false), "Throughput path", "How transfers reach the server"),
    (None, Setting::Protocol, "HTTP version", "Where the path negotiates. ←/→ Automatic, HTTP/1.1, HTTP/2, HTTP/3."),
    (None, Setting::Path(true), "Latency path", "How probes travel"),
    (Some("Stages"), Setting::Stage(Stage::Latency), "Latency", "Idle round trips."),
    (None, Setting::Stage(Stage::Download), "Download", "Server to client."),
    (None, Setting::Stage(Stage::Upload), "Upload", "Client to server, receiver-timed."),
    (None, Setting::Stage(Stage::Bidirectional), "Bidirectional", "Download and upload at once."),
    (None, Setting::LoadedLatency, "Loaded latency",
        "Round trips during transfers: the latency load adds. space on/off."),
    (Some(""), Setting::Advanced, "Advanced", "Warmup, probe cadence, streams and TLS. ←/→ shows or hides."),
    (None, Setting::Warmup, "Warmup",
        "Ramp-up before each window, at least ten round trips. ←/→ ±100 ms (0 ms–4 s)."),
    (None, Setting::Cadence(false), "Idle latency cadence",
        "Probe spacing when idle. ←/→ reply-driven, 80, 250, 600 ms."),
    (None, Setting::Cadence(true), "Loaded latency cadence",
        "Probe spacing during transfers. ←/→ reply-driven, 80, 250, 600 ms."),
    (None, Setting::ForceStreams, "Force exact stream count",
        "Off: each path picks its count. On: the count below everywhere."),
    (None, Setting::Streams, "Maximum H1 streams per direction", "Upper bound on HTTP/1.1 paths. ←/→ ±1 (1–14)."),
    (None, Setting::Insecure, "Skip TLS verify", "Accepts any certificate. Unsafe; sign-in is refused. space on/off."),
    (None, Setting::Reset, "Reset settings", "Restores defaults; keeps the catalogue and servers."),
];

/// Go's setupRow: what a row shows and explains; an inert value is muted.
pub(super) struct Row {
    pub label: &'static str,
    pub value: Line<'static>,
    pub help: String,
    pub inert: bool,
}

impl Setting {
    /// The row's label and help in `ROWS`.
    fn text(self) -> (&'static str, &'static str) {
        let row = ROWS.iter().find(|row| row.1 == self).expect("every setting is a row");
        (row.2, row.3)
    }

    fn label(self) -> &'static str {
        self.text().0
    }

    /// Go's span bound: a duration row's range and step.
    fn bound(self) -> Option<(RangeInclusive<Duration>, Duration)> {
        match self {
            Self::Stage(_) => Some((STAGE_BOUND, Duration::from_secs(1))),
            Self::Warmup => Some((WARMUP_BOUND, Duration::from_millis(100))),
            _ => None,
        }
    }

    pub(super) fn flag(self, config: &Config) -> Option<bool> {
        match self {
            Self::Stage(stage) => Some(config.stages.contains(&stage)),
            Self::LoadedLatency => Some(config.loaded_latency),
            Self::Insecure => Some(config.insecure),
            _ => None,
        }
    }

    /// Go's enterVerb: what Enter does on the row.
    pub(super) fn enter_verb(self) -> &'static str {
        match self {
            Self::Servers | Self::Advanced => "open",
            Self::Reset => "reset",
            Self::Catalogue | Self::Streams | Self::Stage(_) | Self::Warmup => "edit",
            Self::LoadedLatency | Self::Insecure => "on/off",
            _ => "next",
        }
    }

    /// Whether ←/→ change the row: Go's rows that cycle, span or flag.
    pub(super) fn adjusts(self) -> bool {
        !matches!(self, Self::Start | Self::Catalogue | Self::Servers | Self::Reset)
    }
}

/// The duration a stage or the warmup row holds.
fn duration(config: &mut Config, setting: Setting) -> &mut Duration {
    match setting {
        Setting::Stage(Stage::Latency) => &mut config.latency_duration,
        Setting::Stage(Stage::Download) => &mut config.download_duration,
        Setting::Stage(Stage::Upload) => &mut config.upload_duration,
        Setting::Stage(Stage::Bidirectional) => &mut config.bidirectional_duration,
        _ => &mut config.warmup,
    }
}

/// Go's pathChoice: an origin and transport as Go's settings name them, where "auto" is automatic.
#[derive(Clone, Debug)]
pub(super) struct PathChoice {
    target: String,
    transport: String,
    label: String,
    note: String,
}

impl PathChoice {
    fn new(target: &str, transport: &str, label: String, note: String) -> Self {
        let (target, transport) = (target.into(), transport.into());
        Self { target, transport, label, note }
    }

    fn selects(&self, (target, transport): (&str, &str)) -> bool {
        (self.target == target || same_origin(&self.target, target)) && self.transport == transport
    }
}

fn same_origin(a: &str, b: &str) -> bool {
    matches!((canonical_origin(a), canonical_origin(b)), (Ok(a), Ok(b)) if a == b)
}

/// A path setting's origin and transport as Go's settings hold them.
fn path(config: &Config, latency: bool) -> (String, String) {
    let (origin, transport) = match latency {
        true => (&config.latency_origin, wire(config.latency_transport)),
        false => (&config.throughput_origin, wire(config.throughput_transport)),
    };
    (origin.clone().unwrap_or_else(|| "auto".into()), transport)
}

/// Go's shortOrigin: a target on the catalogue's host is its port.
fn short_origin(base: &str, target: &str) -> String {
    let host = |origin: &str| target_origin(origin).ok().flatten();
    let Some(parsed) = host(target) else { return target.to_owned() };
    match &parsed.port {
        Some(port) if host(base).is_some_and(|base| base.host.eq_ignore_ascii_case(&parsed.host)) => format!(":{port}"),
        _ => parsed.authority(),
    }
}

/// Go's discoveredTargets: the advertised paths but datagram throughput, as choices without notes.
fn discovered(offered: &Capabilities, latency: bool) -> Vec<PathChoice> {
    let new = |url: &str, transport: String, label| PathChoice::new(url, &transport, label, String::new());
    if latency {
        let choice = |path: &LatencyTarget| new(&path.base_url, wire(Some(path.transport)), words::latency_path(path));
        return offered.latency.iter().map(choice).collect();
    }
    let datagrams = ThroughputTransport::WebTransportDatagram;
    let choice =
        |path: &ThroughputTarget| new(&path.base_url, wire(Some(path.transport)), words::throughput_path(path));
    let paths = offered.throughput.iter().filter(|path| path.transport != datagrams);
    paths.map(choice).collect()
}

impl Ui {
    /// Go's rows: every row, up to Advanced while it is hidden.
    pub(super) fn rows(&self) -> Vec<Setting> {
        let hidden = ROWS.iter().position(|row| row.1 == Setting::Advanced && !self.advanced);
        let shown = hidden.map_or(ROWS.len(), |advanced| advanced + 1);
        ROWS[..shown].iter().map(|row| row.1).collect()
    }

    pub(super) fn current(&self) -> Setting {
        let rows = self.rows();
        rows[self.row.min(rows.len() - 1)]
    }

    /// Go's checkbox.
    pub(super) fn checkbox(&self, on: bool) -> Span<'static> {
        if on { span("●", self.theme.accent) } else { span("○", self.theme.muted) }
    }

    /// Go's setting.row: the row's label, value, help and whether its value is inert.
    pub(super) fn row(&self, setting: Setting) -> Row {
        let config = &self.config;
        let (mut label, help) = setting.text();
        let (mut help, checkbox) = (help.to_owned(), |on| Line::from(self.checkbox(on)));
        let (value, inert) = match setting {
            Setting::Start | Setting::Reset => (Line::default(), false),
            Setting::Advanced => (Line::from(if self.advanced { "▾ shown" } else { "▸ hidden" }), false),
            Setting::Catalogue => (Line::from(config.url.clone()), false),
            Setting::Servers => {
                // Go's selectedServerNames and readinessSummary.
                let names: Vec<_> = self.checked().map(|server| server.name.as_str()).collect();
                let names = names.join(", ");
                let (rows, ready) = (self.checked().count(), self.ready_servers().count());
                let summary = match () {
                    _ if rows == 0 => String::new(),
                    _ if self.prepare() == PathState::Checking => " · checking".into(),
                    _ if ready == rows => " · ready".into(),
                    _ => format!(" · {ready} of {rows} ready"),
                };
                let names = if names.is_empty() { MISSING } else { &names };
                (Line::from(format!("{names}{summary}")), !self.can_choose_servers())
            }
            Setting::Path(latency) => {
                let (choices, (target, transport)) = (self.path_choices(latency), path(config, latency));
                help = format!("{help}. ←/→ picks one of {}.", choices.len());
                let value = match choices.iter().find(|choice| choice.selects((&target, &transport))) {
                    Some(choice) if choice.note.is_empty() => choice.label.clone(),
                    Some(choice) => format!("{} · {}", choice.label, choice.note),
                    None => {
                        help = format!("Not offered by the checked server. {help}");
                        let target = if target == "auto" { "automatic origin" } else { &target };
                        format!("{} · {target}", words::transport(&transport, latency))
                    }
                };
                (Line::from(value), false)
            }
            Setting::Protocol => {
                let fixed = self.fixed_protocol();
                if fixed.is_some() {
                    help = "Fixed by this path; pick another path to change it.".into();
                }
                let version = words::protocol(fixed.or(config.throughput_protocol));
                (Line::from(version), fixed.is_some())
            }
            Setting::Stage(stage) => {
                help = format!("{help} ←/→ step it (1 s–24 h; a server may allow less), space on/off.");
                let on = config.stages.contains(&stage);
                let duration = Span::raw(format!(" {}", words::setting(config.duration(stage))));
                (Line::from(vec![self.checkbox(on), duration]), !on)
            }
            Setting::LoadedLatency => (checkbox(config.loaded_latency), false),
            Setting::Warmup => (Line::from(words::setting(config.warmup)), false),
            Setting::Cadence(false) => (Line::from(words::cadence(config.ping_interval)), false),
            Setting::Cadence(true) => (Line::from(words::cadence(config.loaded_ping_interval)), false),
            Setting::ForceStreams => (checkbox(config.streams > 0), false),
            Setting::Streams => {
                if config.streams > 0 {
                    label = "Streams per server and direction";
                    help = "Exact streams per server and direction. ←/→ ±1 (1–14).".into();
                }
                (Line::from(self.stream_count().to_string()), false)
            }
            Setting::Insecure => (checkbox(config.insecure), false),
        };
        Row { label, value, help, inert }
    }

    /// Go's singleDiscovery: the one checked server and the paths it advertised.
    fn single_discovery(&self) -> Option<(&ServerSummary, &Capabilities)> {
        let mut checked = self.checked();
        match (checked.next(), checked.next()) {
            (Some(server), None) => Some((server, server.offered.as_ref()?)),
            _ => None,
        }
    }

    /// Go's pathChoices: Automatic, then the one checked server's paths, or the transports the servers share.
    pub(super) fn path_choices(&self, latency: bool) -> Vec<PathChoice> {
        let Some((server, offered)) = self.single_discovery() else {
            return self.shared_paths(latency);
        };
        let resolved = match latency {
            true => server.latency.as_ref().map(|target| &target.base_url),
            false => server.throughput.as_ref().map(|target| &target.base_url),
        };
        let note = resolved.map(|origin| format!("→ {}", short_origin(&self.config.url, origin)));
        let mut choices = vec![PathChoice::new("auto", "auto", "Automatic".into(), note.unwrap_or_default())];
        for mut path in discovered(offered, latency) {
            let target = (path.target.as_str(), path.transport.as_str());
            if !choices.iter().any(|known| known.selects(target)) {
                path.note = short_origin(&self.config.url, &path.target);
                choices.push(path);
            }
        }
        choices
    }

    /// Go's sharedPaths: Automatic and each transport, noting the servers that lack it.
    fn shared_paths(&self, latency: bool) -> Vec<PathChoice> {
        let kinds = [if latency { "websocket" } else { "fetch-stream" }, "webtransport"];
        let mut choices = vec![PathChoice::new("auto", "auto", "Automatic".into(), "each server".into())];
        for kind in kinds {
            let lacking = self.checked().filter(|server| {
                let paths = server.offered.as_ref().map(|offered| discovered(offered, latency));
                !paths.is_some_and(|paths| paths.iter().any(|path| path.transport == kind))
            });
            let names: Vec<_> = lacking.map(|server| server.name.as_str()).collect();
            let note = match names.is_empty() {
                true => "every server".into(),
                false => format!("unavailable on {}", names.join(", ")),
            };
            choices.push(PathChoice::new("auto", kind, words::transport(kind, latency), note));
        }
        choices
    }

    /// Go's protocolView's fixed version: the HTTP version of the picked path when it does not negotiate one.
    fn fixed_protocol(&self) -> Option<Protocol> {
        let (target, transport) = path(&self.config, false);
        let (_, offered) = self.single_discovery()?;
        let mut paths = offered.throughput.iter();
        let picked =
            paths.find(|path| wire(Some(path.transport)) == transport && same_origin(&path.base_url, &target))?;
        Some(picked.protocol).filter(|protocol| *protocol != Protocol::Negotiated)
    }

    /// Go's activate: Enter or space on a row.
    pub(super) fn activate(&mut self, setting: Setting, commands: &mpsc::Sender<Command>) {
        match setting {
            Setting::Start => self.start(commands),
            Setting::Servers => self.open_servers(),
            Setting::Reset if !self.reset_prompt => {
                self.reset_prompt = true;
                self.notice = "Press enter again to reset every setting; any other key keeps them.".into();
            }
            Setting::Reset => {
                let kept = std::mem::take(&mut self.config);
                (self.config.url, self.config.servers, self.reset_prompt) = (kept.url, kept.servers, false);
                self.notice = "Settings reset to defaults.".into();
            }
            Setting::Stage(_) | Setting::Warmup => {
                let value = go_duration(*duration(&mut self.config, setting));
                self.begin_edit(setting, value);
            }
            Setting::Catalogue | Setting::Streams => self.begin_edit(setting, plain(&self.row(setting).value)),
            _ => match setting.flag(&self.config) {
                Some(on) => self.set_flag(setting, !on),
                None => self.cycle(setting, 1),
            },
        }
    }

    /// Go's adjust: ←/→ steps a duration or a choice and switches a flag on or off.
    pub(super) fn adjust(&mut self, setting: Setting, step: isize) {
        if let Some((bound, mut unit)) = setting.bound() {
            let value = duration(&mut self.config, setting);
            if matches!(setting, Setting::Stage(_)) {
                let at = value.saturating_sub(Duration::from_nanos(u64::from(step < 0)));
                unit = Duration::from_secs(match at.as_secs() {
                    0..60 => 1,
                    60..600 => 10,
                    600..3600 => 60,
                    _ => 300,
                });
            }
            let moved = if step > 0 { *value + unit } else { value.saturating_sub(unit) };
            *value = moved.clamp(*bound.start(), *bound.end());
            self.notice = format!("{} {}.", setting.label(), words::setting(*value));
        } else if setting.flag(&self.config).is_some() {
            self.set_flag(setting, step > 0);
        } else {
            self.cycle(setting, step);
        }
    }

    /// Go's setFlag with its notice.
    pub(super) fn set_flag(&mut self, setting: Setting, on: bool) {
        match setting {
            Setting::Stage(stage) => {
                self.config.stages.retain(|existing| *existing != stage);
                if on {
                    self.config.stages.push(stage);
                    self.config.stages.sort_unstable();
                }
            }
            Setting::LoadedLatency => self.config.loaded_latency = on,
            _ => self.config.insecure = on,
        }
        self.notice = format!("{} {}.", setting.label(), if on { "on" } else { "off" });
    }

    /// Go's cycle: the rows that step through choices; a choice not offered steps to the first.
    fn cycle(&mut self, setting: Setting, step: isize) {
        let next = |at: Option<usize>, length: usize| {
            (at.map_or(-1, |at| at as isize) + step).rem_euclid(length as isize) as usize
        };
        match setting {
            Setting::Advanced => self.advanced = !self.advanced,
            Setting::Path(latency) => {
                let (choices, (target, transport)) = (self.path_choices(latency), path(&self.config, latency));
                let at = choices.iter().position(|choice| choice.selects((&target, &transport)));
                let choice = &choices[at.map_or(0, |at| next(Some(at), choices.len()))];
                let origin = Some(choice.target.clone()).filter(|target| target != "auto");
                let json = serde_json::Value::from(choice.transport.as_str());
                let config = &mut self.config;
                if latency {
                    (config.latency_origin, config.latency_transport) = (origin, serde_json::from_value(json).ok());
                } else {
                    (config.throughput_origin, config.throughput_transport) =
                        (origin, serde_json::from_value(json).ok());
                    if self.fixed_protocol().is_some() {
                        self.config.throughput_protocol = None;
                    }
                }
                self.notice = format!("{}: {}.", setting.label(), choice.label);
            }
            Setting::Protocol => {
                if let Some(fixed) = self.fixed_protocol() {
                    self.notice = format!("This path serves {} only.", words::protocol(Some(fixed)));
                    return;
                }
                let (protocols, config) = ([None, Some(Http1), Some(Http2), Some(Http3)], &mut self.config);
                let current = config.throughput_protocol;
                config.throughput_protocol = protocols[next(protocols.iter().position(|at| *at == current), 4)];
                self.notice = format!("HTTP version: {}.", words::protocol(config.throughput_protocol));
            }
            Setting::Cadence(loaded) => {
                let interval = match loaded {
                    true => &mut self.config.loaded_ping_interval,
                    false => &mut self.config.ping_interval,
                };
                let at = CADENCES.iter().position(|(.., preset)| preset == interval);
                *interval = CADENCES[next(at, CADENCES.len())].2;
                self.notice = format!("{}: {}.", setting.label(), words::cadence(*interval));
            }
            Setting::ForceStreams => {
                let config = &mut self.config;
                config.streams = if config.streams > 0 { 0 } else { config.auto_streams };
                self.notice = format!("Stream count: {}.", words::streams(config, None));
            }
            Setting::Streams => self.set_streams(self.stream_count().saturating_add_signed(step).clamp(1, MAX_STREAMS)),
            _ => {}
        }
    }

    /// Go's streamCount: the forced count, or the automatic maximum.
    fn stream_count(&self) -> usize {
        match self.config.streams {
            0 => self.config.auto_streams,
            forced => forced,
        }
    }

    /// Sets Go's streamCount, with a notice of the streams an HTTP/1.1 fetch path opens.
    fn set_streams(&mut self, count: usize) {
        match self.config.streams {
            0 => self.config.auto_streams = count,
            _ => self.config.streams = count,
        }
        let h1 = ThroughputTarget {
            base_url: String::new(),
            transport: ThroughputTransport::FetchStream,
            protocol: Http1,
        };
        self.notice = format!("Stream count: {}.", words::streams(&self.config, Some(&h1)));
    }

    /// Go's beginEdit.
    pub(super) fn begin_edit(&mut self, setting: Setting, value: String) {
        self.edit = Some(Edit::new(setting, &value));
        self.notice = "Enter applies, esc cancels.".into();
    }

    /// Go's commitEdit: durations read Go's syntax, a bare number as seconds; the other rows parse theirs.
    pub(super) fn commit_edit(&mut self, setting: Setting, raw: &str) -> Result<(), String> {
        let raw = raw.trim();
        if setting == Setting::Streams {
            let count = raw.parse().ok().filter(|count| (1..=MAX_STREAMS).contains(count));
            self.set_streams(count.ok_or(format!("streams must be a whole number from 1 to {MAX_STREAMS}"))?);
            return Ok(());
        }
        let Some((bound, _)) = setting.bound() else { return self.set_catalogue(raw) };
        let raw = raw
            .parse::<f64>()
            .map_or_else(|_| raw.to_owned(), |seconds| format!("{seconds}s"));
        let nanos =
            parse_go_duration(&raw).map_err(|_| "use a duration like 800ms, 4s, or 1m; a bare number is seconds");
        // A negative duration is out of range, as Go's DurationBound.Check reads it.
        let value = u64::try_from(nanos?).map_or(Duration::MAX, Duration::from_nanos);
        if !bound.contains(&value) {
            let (min, max) = (short_duration(*bound.start()), short_duration(*bound.end()));
            return Err(format!("{} must be from {min} to {max}", setting.label()));
        }
        *duration(&mut self.config, setting) = value;
        self.notice = format!("{} {}.", setting.label(), words::setting(value));
        Ok(())
    }

    /// Go's catalogueRow.parse: a missing scheme is http for a loopback host and https otherwise.
    fn set_catalogue(&mut self, raw: &str) -> Result<(), String> {
        let scheme = if raw.contains("://") { "" } else { default_scheme(raw) };
        let raw = format!("{scheme}{raw}");
        let canonical =
            catalog_origin(&raw).map_err(|_| "use an http:// or https:// origin, for example https://meter.example")?;
        if canonical != self.config.url {
            self.config.servers.clear();
        }
        self.notice = format!("Catalogue {canonical}.");
        self.config.url = canonical;
        Ok(())
    }

    /// Go's openServerChooser.
    pub(super) fn open_servers(&mut self) {
        if self.checking() {
            (self.open_chooser, self.notice) = (true, "Test servers open when the path check finishes.".into());
        } else if self.prepared.is_empty() {
            (self.open_chooser, self.notice) = (true, "Loading servers…".into());
            self.recheck_soon();
        } else if !self.can_choose_servers() {
            self.notice = "This catalogue offers one server.".into();
        } else {
            (self.popup, self.server_row) = (Popup::Servers, 0);
            let ids: Vec<_> = match self.config.servers.is_empty() {
                true => self.checked().map(|server| server.id.clone()).collect(),
                false => self.config.servers.clone(),
            };
            let prepared = self.prepared.iter().map(|server| &server.id);
            self.draft = prepared.filter(|id| ids.contains(id)).cloned().collect();
            self.notice = format!("Choose up to {MAX_SELECTED_SERVERS}. Their speeds are combined.");
        }
    }

    /// Go's handleServerChooserKey.
    pub(super) fn chooser_key(&mut self, name: &str) {
        use super::keys::{APPLY, DISCARD, ROWS, TOGGLE_SERVER, delta};
        if DISCARD.matches(name) {
            (self.popup, self.notice) = (Popup::None, "Server selection unchanged.".into());
        } else if ROWS.matches(name) {
            let last = self.prepared.len().saturating_sub(1);
            self.server_row = self.server_row.saturating_add_signed(delta(name)).min(last);
        } else if TOGGLE_SERVER.matches(name) {
            let Some(id) = self.prepared.get(self.server_row).map(|server| server.id.clone()) else {
                return;
            };
            if let Some(at) = self.draft.iter().position(|chosen| *chosen == id) {
                self.draft.remove(at);
            } else if self.draft.len() < MAX_SELECTED_SERVERS {
                self.draft.push(id);
            } else {
                self.notice = "At most four servers share one test.".into();
            }
        } else if APPLY.matches(name) {
            (self.config.servers, self.popup) = (self.draft.clone(), Popup::None);
            self.notice = "Checking the selected servers…".into();
            self.recheck_soon();
        }
    }
}

/// Go's defaultScheme: http for a loopback host, https otherwise.
fn default_scheme(raw: &str) -> &'static str {
    let authority = raw.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = match host.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|byte| byte.is_ascii_digit()) => name,
        _ => host,
    };
    let bracketed = host.strip_prefix('[').and_then(|host| host.strip_suffix(']'));
    let host = bracketed.unwrap_or(host);
    let loopback = host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    match loopback || host.eq_ignore_ascii_case("localhost") {
        true => "http://",
        false => "https://",
    }
}

/// Go's editState: a textinput's value and cursor, and the error its last Enter found.
pub(super) struct Edit {
    pub setting: Setting,
    chars: Vec<char>,
    cursor: usize,
    pub error: String,
}

impl Edit {
    fn new(setting: Setting, value: &str) -> Self {
        let mut edit = Self { setting, chars: Vec::new(), cursor: 0, error: String::new() };
        edit.insert(value);
        edit
    }

    pub(super) fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// Typed or pasted text; tabs and line breaks become spaces, as textinput sanitizes them.
    pub(super) fn insert(&mut self, text: &str) {
        let spaced = |c| if matches!(c, '\t' | '\n' | '\r') { ' ' } else { c };
        let shown = text.chars().map(spaced).filter(|c| terminal_character(*c));
        for character in shown.take(MAX_TEXT.saturating_sub(self.chars.len())) {
            self.chars.insert(self.cursor, character);
            self.cursor += 1;
        }
    }

    /// textinput's default keymap; any other key types its text.
    pub(super) fn key(&mut self, name: &str, text: Option<char>) {
        let length = self.chars.len();
        let (before, after) = self.chars.split_at(self.cursor);
        // A word and the spaces before it, on each side of the cursor.
        let spaces = after.iter().take_while(|c| c.is_whitespace()).count();
        let word_end = self.cursor + spaces + after[spaces..].iter().take_while(|c| !c.is_whitespace()).count();
        let spaces = before.iter().rev().take_while(|c| c.is_whitespace()).count();
        let word = before[..before.len() - spaces].iter().rev();
        let word_start = self.cursor - spaces - word.take_while(|c| !c.is_whitespace()).count();
        match name {
            "right" | "ctrl+f" => self.cursor = (self.cursor + 1).min(length),
            "left" | "ctrl+b" => self.cursor = self.cursor.saturating_sub(1),
            "alt+right" | "ctrl+right" | "alt+f" => self.cursor = word_end,
            "alt+left" | "ctrl+left" | "alt+b" => self.cursor = word_start,
            "alt+backspace" | "ctrl+w" | "ctrl+backspace" => {
                self.chars.drain(word_start..self.cursor);
                self.cursor = word_start;
            }
            "alt+delete" | "alt+d" | "ctrl+delete" => {
                self.chars.drain(self.cursor..word_end);
            }
            "ctrl+k" => self.chars.truncate(self.cursor),
            "ctrl+u" => {
                self.chars.drain(..self.cursor);
                self.cursor = 0;
            }
            "backspace" | "ctrl+h" if self.cursor > 0 => {
                self.cursor -= 1;
                self.chars.remove(self.cursor);
            }
            "delete" | "ctrl+d" if self.cursor < length => {
                self.chars.remove(self.cursor);
            }
            "home" | "ctrl+a" => self.cursor = 0,
            "end" | "ctrl+e" => self.cursor = length,
            _ => self.insert(&text.map(String::from).unwrap_or_default()),
        }
    }

    /// textinput.View: the value, the cursor reversed over the character under it.
    pub(super) fn view(&self, ui: &Ui, width: usize) -> Line<'static> {
        let width = width.max(1);
        let (before, rest) = self.chars.split_at(self.cursor);
        let under = rest.first().copied().filter(|ch| cell(*ch) <= width).unwrap_or(' ');
        let (mut start, mut room) = (before.len(), width - cell(under));
        while start > 0 && cell(before[start - 1]) <= room {
            start -= 1;
            room -= cell(before[start]);
        }
        let before: String = before[start..].iter().collect();
        let mut after = String::new();
        for ch in rest.iter().skip(1) {
            if cell(*ch) > room {
                break;
            }
            room -= cell(*ch);
            after.push(*ch);
        }
        let under = under.to_string();
        let theme = &ui.theme;
        let spans = [(before, theme.value), (under, theme.cursor), (after, theme.value)];
        let spans = spans.into_iter().filter(|(text, _)| !text.is_empty());
        Line::from(spans.map(|(text, style)| span(text, style)).collect::<Vec<_>>())
    }
}
