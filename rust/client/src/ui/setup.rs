//! Go's setup list (setup.go): its settings, what each row shows, and what its keys change.
use super::{Command, MAX_TEXT, Popup, Ui};
use crate::{
    config::{Config, MAX_STREAMS},
    model::{ServerSummary, Stage},
    report::{plain, span},
    vocabulary::{self as words, CADENCES, MISSING, wire},
};
use graphite_meter_core::{
    catalog::MAX_SELECTED_SERVERS,
    discovery::{Capabilities, Protocol, ThroughputTarget, ThroughputTransport},
    duration::parse_go_duration,
    origin::{canonical_origin, catalog_origin, target_origin},
    text::terminal_character,
};
use ratatui::text::{Line, Span};
use std::{ops::RangeInclusive, time::Duration};
use tokio::sync::mpsc;

const STAGE_BOUND: RangeInclusive<Duration> = Duration::from_secs(1)..=Duration::from_secs(300);
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

/// Go's setupGroups: a heading and its rows; Advanced hides the rows after it.
#[rustfmt::skip]
pub(super) const GROUPS: [(&str, &[Setting]); 4] = [
    ("", &[Setting::Start]),
    ("Connection", &[Setting::Catalogue, Setting::Servers, Setting::Path(false), Setting::Protocol, Setting::Path(true)]),
    ("Stages", &[Setting::Stage(Stage::Latency), Setting::Stage(Stage::Download), Setting::Stage(Stage::Upload),
        Setting::Stage(Stage::Bidirectional), Setting::LoadedLatency]),
    ("", &[Setting::Advanced, Setting::Warmup, Setting::Cadence(false), Setting::Cadence(true), Setting::ForceStreams,
        Setting::Streams, Setting::Insecure, Setting::Reset]),
];

/// Go's setupRow: what a row shows and explains; an inert value is muted.
pub(super) struct Row {
    pub label: &'static str,
    pub value: Line<'static>,
    pub help: String,
    pub inert: bool,
}

impl Setting {
    /// Go's label: the row's name, which its notices repeat.
    fn label(self) -> &'static str {
        match self {
            Self::Start => "Start test",
            Self::Catalogue => "Catalogue URL",
            Self::Servers => "Test servers",
            Self::Path(false) => "Throughput path",
            Self::Protocol => "HTTP version",
            Self::Path(true) => "Latency path",
            Self::Stage(stage) => stage.name(),
            Self::LoadedLatency => "Loaded latency",
            Self::Advanced => "Advanced",
            Self::Warmup => "Warmup",
            Self::Cadence(false) => "Idle latency cadence",
            Self::Cadence(true) => "Loaded latency cadence",
            Self::ForceStreams => "Force exact stream count",
            Self::Streams => "Maximum H1 streams per direction",
            Self::Insecure => "Skip TLS verify",
            Self::Reset => "Reset settings",
        }
    }

    /// Go's help: what the row does and how its keys change it. A path's goes on to its choices.
    fn help(self) -> &'static str {
        match self {
            Self::Start => "Runs the checked stages in order. r starts from any row.",
            Self::Catalogue => "Origin that lists the test servers. enter types one.",
            Self::Servers => "Measured at once; their speeds add up. enter picks up to 4.",
            Self::Path(false) => "How transfers reach the server",
            Self::Protocol => "Where the path negotiates. ←/→ Automatic, HTTP/1.1, HTTP/2, HTTP/3.",
            Self::Path(true) => "How probes travel",
            Self::Stage(Stage::Latency) => "Idle round trips. ←/→ ±1 s (1 s–300 s), space on/off.",
            Self::Stage(Stage::Download) => "Server to client. ←/→ ±1 s (1 s–300 s), space on/off.",
            Self::Stage(Stage::Upload) => "Client to server, receiver-timed. ←/→ ±1 s (1 s–300 s), space on/off.",
            Self::Stage(Stage::Bidirectional) => "Download and upload at once. ←/→ ±1 s (1 s–300 s), space on/off.",
            Self::LoadedLatency => "Round trips during transfers: the latency load adds. space on/off.",
            Self::Advanced => "Warmup, probe cadence, streams and TLS. ←/→ shows or hides.",
            Self::Warmup => "Ramp-up before each window, at least ten round trips. ←/→ ±100 ms (0 ms–4 s).",
            Self::Cadence(false) => "Probe spacing when idle. ←/→ reply-driven, 80, 250, 600 ms.",
            Self::Cadence(true) => "Probe spacing during transfers. ←/→ reply-driven, 80, 250, 600 ms.",
            Self::ForceStreams => "Off: each path picks its count. On: the count below everywhere.",
            Self::Streams => "Upper bound on HTTP/1.1 paths. ←/→ ±1 (1–14).",
            Self::Insecure => "Accepts any certificate. Unsafe; sign-in is refused. space on/off.",
            Self::Reset => "Restores defaults; keeps the catalogue and servers.",
        }
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
#[derive(Clone, Debug, Default)]
pub(super) struct PathChoice {
    target: String,
    transport: String,
    label: String,
    note: String,
}

impl PathChoice {
    fn automatic(transport: &str, label: String, note: String) -> Self {
        let (target, transport) = ("auto".into(), transport.into());
        Self {
            target,
            transport,
            label,
            note,
        }
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
    let Some(parsed) = host(target) else {
        return target.to_owned();
    };
    match &parsed.port {
        Some(port) if host(base).is_some_and(|base| base.host.eq_ignore_ascii_case(&parsed.host)) => format!(":{port}"),
        _ => parsed.authority(),
    }
}

/// Go's discoveredTargets: the advertised paths but datagram throughput, as choices without notes.
fn discovered(offered: &Capabilities, latency: bool) -> Vec<PathChoice> {
    let choice = |target: &String, transport, label| PathChoice {
        target: target.clone(),
        transport,
        label,
        ..PathChoice::default()
    };
    if latency {
        let targets = offered.latency.iter();
        let choices = targets.map(|target| {
            choice(
                &target.base_url,
                wire(Some(target.transport)),
                words::latency_path(target),
            )
        });
        return choices.collect();
    }
    let targets = offered.throughput.iter().filter_map(|target| match target.transport {
        ThroughputTransport::WebTransportDatagram => None,
        kind => Some(choice(
            &target.base_url,
            wire(Some(kind)),
            words::throughput_path(target),
        )),
    });
    targets.collect()
}

impl Ui {
    /// Go's rows: every group's rows, up to Advanced while it is hidden.
    pub(super) fn rows(&self) -> Vec<Setting> {
        let mut rows = Vec::new();
        for setting in GROUPS.iter().flat_map(|(_, group)| group.iter()) {
            rows.push(*setting);
            if *setting == Setting::Advanced && !self.advanced {
                break;
            }
        }
        rows
    }

    pub(super) fn current(&self) -> Setting {
        let rows = self.rows();
        rows[self.row.min(rows.len() - 1)]
    }

    /// Go's checkbox.
    pub(super) fn checkbox(&self, on: bool) -> Span<'static> {
        match on {
            true => span("●", self.theme.accent),
            false => span("○", self.theme.muted),
        }
    }

    /// Go's setting.row: the row's label, value, help and whether its value is inert.
    pub(super) fn row(&self, setting: Setting) -> Row {
        let config = &self.config;
        let (mut label, mut help) = (setting.label(), setting.help().to_owned());
        let checkbox = |on| Line::from(self.checkbox(on));
        let (value, inert) = match setting {
            Setting::Start | Setting::Reset => (Line::default(), false),
            Setting::Advanced => (Line::from(if self.advanced { "▾ shown" } else { "▸ hidden" }), false),
            Setting::Catalogue => (Line::from(config.url.clone()), false),
            Setting::Servers => {
                let names: Vec<_> = self.checked().iter().map(|server| server.name.as_str()).collect();
                let mut value = if names.is_empty() {
                    MISSING.into()
                } else {
                    names.join(", ")
                };
                let summary = self.readiness_summary();
                if !summary.is_empty() {
                    value = format!("{value} · {summary}");
                }
                (Line::from(value), !self.can_choose_servers())
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
        Row {
            label,
            value,
            help,
            inert,
        }
    }

    /// Go's singleDiscovery: the one checked server and the paths it advertised.
    fn single_discovery(&self) -> Option<(&ServerSummary, &Capabilities)> {
        match self.checked().as_slice() {
            [server] => Some((*server, server.offered.as_ref()?)),
            _ => None,
        }
    }

    /// Go's pathChoices: Automatic, then each advertised path of the one checked server, or
    /// each transport the selected servers share.
    pub(super) fn path_choices(&self, latency: bool) -> Vec<PathChoice> {
        let Some((server, offered)) = self.single_discovery() else {
            return self.shared_paths(latency);
        };
        let resolved = match latency {
            true => server.latency.as_ref().map(|target| &target.base_url),
            false => server.throughput.as_ref().map(|target| &target.base_url),
        };
        let note = resolved.map(|origin| format!("→ {}", short_origin(&self.config.url, origin)));
        let mut choices = vec![PathChoice::automatic(
            "auto",
            "Automatic".into(),
            note.unwrap_or_default(),
        )];
        for mut choice in discovered(offered, latency) {
            if !choices
                .iter()
                .any(|known| known.selects((&choice.target, &choice.transport)))
            {
                choice.note = short_origin(&self.config.url, &choice.target);
                choices.push(choice);
            }
        }
        choices
    }

    /// Go's sharedPaths: Automatic and each transport, noting the servers that lack it.
    fn shared_paths(&self, latency: bool) -> Vec<PathChoice> {
        let kinds = if latency {
            ["websocket", "webtransport"]
        } else {
            ["fetch-stream", "webtransport"]
        };
        let mut choices = vec![PathChoice::automatic("auto", "Automatic".into(), "each server".into())];
        for kind in kinds {
            let lacking = self.checked().into_iter().filter(|server| {
                let paths = server.offered.as_ref().map(|offered| discovered(offered, latency));
                !paths.is_some_and(|paths| paths.iter().any(|path| path.transport == kind))
            });
            let names: Vec<_> = lacking.map(|server| server.name.as_str()).collect();
            let note = match names.is_empty() {
                true => "every server".into(),
                false => format!("unavailable on {}", names.join(", ")),
            };
            choices.push(PathChoice::automatic(kind, words::transport(kind, latency), note));
        }
        choices
    }

    /// Go's protocolView's fixed version: the HTTP version of the one path the settings pick,
    /// when that path does not negotiate one.
    fn fixed_protocol(&self) -> Option<Protocol> {
        let (target, transport) = path(&self.config, false);
        let (_, offered) = self.single_discovery()?;
        let picked = offered
            .throughput
            .iter()
            .find(|offered| wire(Some(offered.transport)) == transport && same_origin(&offered.base_url, &target));
        picked
            .map(|target| target.protocol)
            .filter(|protocol| *protocol != Protocol::Negotiated)
    }

    /// Go's activate: Enter or space on a row.
    pub(super) fn activate(&mut self, setting: Setting, commands: &mpsc::Sender<Command>) {
        let before = self.config.clone();
        match setting {
            Setting::Start => return self.start(commands),
            Setting::Servers => return self.open_servers(),
            Setting::Reset if !self.reset_prompt => {
                self.reset_prompt = true;
                self.notice = "Press enter again to reset every setting; any other key keeps them.".into();
            }
            Setting::Reset => {
                let (url, servers) = (self.config.url.clone(), self.config.servers.clone());
                (self.config, self.reset_prompt) = (
                    Config {
                        url,
                        servers,
                        ..Config::default()
                    },
                    false,
                );
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
        self.recheck_if_changed(&before);
    }

    /// Go's adjust: ←/→ steps a duration or a choice and switches a flag on or off.
    pub(super) fn adjust(&mut self, setting: Setting, step: isize) {
        let before = self.config.clone();
        if let Some((bound, unit)) = setting.bound() {
            let value = duration(&mut self.config, setting);
            let moved = if step > 0 {
                value.saturating_add(unit)
            } else {
                value.saturating_sub(unit)
            };
            *value = moved.clamp(*bound.start(), *bound.end());
            self.notice = format!("{} {}.", setting.label(), words::setting(*value));
        } else if setting.flag(&self.config).is_some() {
            self.set_flag(setting, step > 0);
        } else {
            self.cycle(setting, step);
        }
        self.recheck_if_changed(&before);
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
                let transport = serde_json::Value::from(choice.transport.as_str());
                let config = &mut self.config;
                if latency {
                    (config.latency_origin, config.latency_transport) =
                        (origin, serde_json::from_value(transport).ok());
                } else {
                    config.throughput_transport = serde_json::from_value(transport).ok();
                    config.throughput_origin = origin;
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
                let protocols = [
                    None,
                    Some(Protocol::Http1),
                    Some(Protocol::Http2),
                    Some(Protocol::Http3),
                ];
                let at = protocols
                    .iter()
                    .position(|protocol| *protocol == self.config.throughput_protocol);
                self.config.throughput_protocol = protocols[next(at, protocols.len())];
                self.notice = format!("HTTP version: {}.", words::protocol(self.config.throughput_protocol));
            }
            Setting::Cadence(loaded) => {
                let config = &mut self.config;
                let interval = if loaded {
                    &mut config.loaded_ping_interval
                } else {
                    &mut config.ping_interval
                };
                let at = CADENCES.iter().position(|(.., preset)| preset == interval);
                *interval = CADENCES[next(at, CADENCES.len())].2;
                self.notice = format!("{}: {}.", setting.label(), words::cadence(*interval));
            }
            Setting::ForceStreams => {
                self.config.streams = if self.config.streams > 0 {
                    0
                } else {
                    self.config.auto_streams
                };
                self.notice = format!("Stream count: {}.", words::streams(&self.config, None));
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
            protocol: Protocol::Http1,
        };
        self.notice = format!("Stream count: {}.", words::streams(&self.config, Some(&h1)));
    }

    /// Go's recheckIfPathsChanged.
    pub(super) fn recheck_if_changed(&mut self, before: &Config) {
        if before.preparation_key() != self.config.preparation_key() {
            self.recheck_soon();
        }
    }

    /// Go's beginEdit.
    pub(super) fn begin_edit(&mut self, setting: Setting, value: String) {
        self.edit = Some(Edit::new(setting, &value));
        self.notice = "Enter applies, esc cancels.".into();
    }

    /// Go's commitEdit: a duration row reads a Go duration, where a bare number is seconds, and
    /// the catalogue and stream rows parse theirs.
    pub(super) fn commit_edit(&mut self, setting: Setting, raw: &str) -> Result<(), String> {
        let raw = raw.trim();
        if setting == Setting::Streams {
            let count = raw.parse().ok().filter(|count| (1..=MAX_STREAMS).contains(count));
            self.set_streams(count.ok_or(format!("streams must be a whole number from 1 to {MAX_STREAMS}"))?);
            return Ok(());
        }
        let Some((bound, _)) = setting.bound() else {
            return self.set_catalogue(raw);
        };
        let raw = raw
            .parse::<f64>()
            .map_or_else(|_| raw.to_owned(), |seconds| format!("{seconds}s"));
        let nanos = parse_go_duration(&raw).ok().and_then(|nanos| u64::try_from(nanos).ok());
        let nanos = nanos.ok_or("use a duration like 800ms, 4s, or 1m; a bare number is seconds")?;
        let value = Duration::from_nanos(nanos);
        if !bound.contains(&value) {
            let seconds = |bound: &Duration| format!("{} s", bound.as_secs_f64());
            let (min, max) = (seconds(bound.start()), seconds(bound.end()));
            return Err(format!("{} must be from {min} to {max}", setting.label()));
        }
        *duration(&mut self.config, setting) = value;
        self.notice = format!("{} {}.", setting.label(), words::setting(value));
        Ok(())
    }

    /// Go's catalogueRow.parse: a missing scheme is http for a loopback host and https otherwise.
    fn set_catalogue(&mut self, raw: &str) -> Result<(), String> {
        let raw = match raw.contains("://") {
            true => raw.to_owned(),
            false => format!("{}{raw}", default_scheme(raw)),
        };
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
            self.open_chooser = true;
            self.notice = "Test servers open when the path check finishes.".into();
        } else if self.prepared.is_empty() {
            self.open_chooser = true;
            self.notice = "Loading servers…".into();
            self.recheck_soon();
        } else if !self.can_choose_servers() {
            self.notice = "This catalogue offers one server.".into();
        } else {
            (self.popup, self.server_row) = (Popup::Servers, 0);
            let checked = self.checked().iter().map(|server| server.id.clone()).collect();
            let ids = if self.config.servers.is_empty() {
                checked
            } else {
                self.config.servers.clone()
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
            self.popup = Popup::None;
            self.notice = "Server selection unchanged.".into();
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
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let loopback = host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if loopback || host.eq_ignore_ascii_case("localhost") {
        "http://"
    } else {
        "https://"
    }
}

/// Go's time.Duration.String for the durations setup holds, which stay under an hour.
pub(super) fn go_duration(duration: Duration) -> String {
    let nanos = duration.as_nanos();
    let decimal = |value: u128, scale: u128| {
        let fraction = format!("{:0width$}", value % scale, width = scale.ilog10() as usize);
        let number = format!("{}.{fraction}", value / scale);
        number.trim_end_matches('0').trim_end_matches('.').to_owned()
    };
    match nanos {
        0 => "0s".into(),
        1..1_000 => format!("{nanos}ns"),
        1_000..1_000_000 => format!("{}µs", decimal(nanos, 1_000)),
        1_000_000..1_000_000_000 => format!("{}ms", decimal(nanos, 1_000_000)),
        60_000_000_000.. => format!(
            "{}m{}s",
            nanos / 60_000_000_000,
            decimal(nanos % 60_000_000_000, 1_000_000_000)
        ),
        _ => format!("{}s", decimal(nanos, 1_000_000_000)),
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
        let (chars, cursor, error) = (Vec::new(), 0, String::new());
        let mut edit = Self {
            setting,
            chars,
            cursor,
            error,
        };
        edit.insert(value);
        edit
    }

    pub(super) fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// Typed or pasted text; tabs and line breaks become spaces, as textinput sanitizes them.
    pub(super) fn insert(&mut self, text: &str) {
        let spaced = text
            .chars()
            .map(|c| if matches!(c, '\t' | '\n' | '\r') { ' ' } else { c });
        let shown = spaced.filter(|character| terminal_character(*character));
        for character in shown.take(MAX_TEXT.saturating_sub(self.chars.len())) {
            self.chars.insert(self.cursor, character);
            self.cursor += 1;
        }
    }

    /// textinput's default keymap; any other key types its text.
    pub(super) fn key(&mut self, name: &str, text: Option<char>) {
        let (length, space) = (self.chars.len(), |at: usize| self.chars[at].is_whitespace());
        let mut word_end = self.cursor;
        while word_end < length && space(word_end) {
            word_end += 1;
        }
        while word_end < length && !space(word_end) {
            word_end += 1;
        }
        let mut word_start = self.cursor;
        while word_start > 0 && space(word_start - 1) {
            word_start -= 1;
        }
        while word_start > 0 && !space(word_start - 1) {
            word_start -= 1;
        }
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
    pub(super) fn view(&self, ui: &Ui) -> Line<'static> {
        let (before, rest) = self.chars.split_at(self.cursor);
        let under = rest.first().map_or(" ".into(), char::to_string);
        let after: String = rest.iter().skip(1).collect();
        let before: String = before.iter().collect();
        let spans = [
            (before, ui.theme.value),
            (under, ui.theme.cursor),
            (after, ui.theme.value),
        ];
        let spans = spans.into_iter().filter(|(text, _)| !text.is_empty());
        Line::from(spans.map(|(text, style)| span(text, style)).collect::<Vec<_>>())
    }
}
