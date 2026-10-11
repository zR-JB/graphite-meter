//! The path choices the checked servers offer, and how ready each selected server is.
use super::{App, FRESH};
use crate::{
    config::PathChoice,
    events::Check,
    report::vocabulary as words,
    run::prepare::{Paths, ServerPath},
};
use graphite_meter_proto::{
    catalog::ServerId,
    discovery::{Capabilities, LatencyTarget, LatencyTransport, Protocol, ThroughputTarget, ThroughputTransport},
    origin::Origin,
    reason::FailureReason,
};
use std::time::Instant;

/// A selected server's paths as setup shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Readiness {
    Ready,
    Checking,
    Stale,
    Failed,
    SignIn,
}

impl Readiness {
    pub(super) fn label(self) -> &'static str {
        ["Ready", "Checking", "Recheck needed", "Failed", "Sign in"][self as usize]
    }
}

impl App {
    /// Whether the paths are being checked, or will be once the settings rest.
    pub(super) fn checking(&self) -> bool {
        self.recheck.is_some() || self.view.check == Check::Checking
    }

    pub(super) fn readiness(&self, server: &ServerPath) -> Readiness {
        let outdated = |(_, at): &(_, Instant)| self.now.saturating_duration_since(*at) > FRESH;
        let stale = self.checked.as_ref().is_none_or(outdated);
        match &server.path {
            _ if self.checking() => Readiness::Checking,
            Err(failure) if failure.reason == FailureReason::SignInRequired => Readiness::SignIn,
            Err(_) => Readiness::Failed,
            Ok(_) if stale => Readiness::Stale,
            Ok(_) => Readiness::Ready,
        }
    }

    /// The selected servers whose paths a run can take.
    pub(super) fn ready(&self) -> Vec<ServerId> {
        let usable = |server: &&ServerPath| matches!(self.readiness(server), Readiness::Ready | Readiness::Stale);
        let servers = self.view.servers.iter().filter(usable);
        servers.map(|server| server.id.clone()).collect()
    }

    pub(super) fn can_use_available(&self) -> bool {
        let ready = self.ready().len();
        !self.checking() && ready > 0 && ready < self.view.servers.len()
    }

    /// Whether the last check left no server to run with.
    pub(super) fn could_not_start(&self) -> bool {
        matches!(self.view.check, Check::Failed(_)) || self.view.check == Check::Ready && self.ready().is_empty()
    }

    /// Whether the last check's paths serve the settings as they are now.
    pub(super) fn fresh(&self) -> bool {
        let fresh =
            |(key, at): &(_, Instant)| *key == self.config.key() && self.now.saturating_duration_since(*at) <= FRESH;
        self.checked.as_ref().is_some_and(fresh)
    }

    /// The single selected server and what its preflight offered.
    fn single(&self) -> Option<(&ServerPath, &Capabilities)> {
        match &self.view.servers[..] {
            [server] => Some((server, server.offered.as_ref()?)),
            _ => None,
        }
    }

    /// A path row's choices: automatic, then the single server's offers or the transports servers may share.
    fn choices<T: Copy + PartialEq>(&self, kind: &Kind<T>) -> Vec<Choice<T>> {
        let automatic = |note| Choice::new(None, None, "Automatic".into(), note);
        let Some((server, offered)) = self.single() else {
            let mut choices = vec![automatic("each server".into())];
            for transport in kind.shared {
                let offers = |server: &ServerPath| {
                    let offered = server.offered.as_ref();
                    let paths = |offered| (kind.offered)(offered, &server.origin);
                    offered.is_some_and(|offered| paths(offered).iter().any(|path| path.1 == transport))
                };
                let lacking = self.view.servers.iter().filter(|server| !offers(server));
                let names: Vec<_> = lacking.map(|server| server.name.as_str()).collect();
                let note = match names.is_empty() {
                    true => "every server".into(),
                    false => format!("unavailable on {}", names.join(", ")),
                };
                choices.push(Choice::new(None, Some(transport), (kind.label)(transport).into(), note));
            }
            return choices;
        };
        let checked = server.path.as_ref().ok().and_then(kind.checked);
        let note = checked.map(|origin| format!("→ {}", self.short(origin)));
        let mut choices = vec![automatic(note.unwrap_or_default())];
        for (origin, transport, version) in (kind.offered)(offered, &server.origin) {
            let chosen = (Some(&origin), Some(transport));
            if !choices.iter().any(|choice| choice.selects(chosen)) {
                let label = words::connection((kind.label)(transport), version, &origin);
                let note = self.short(&origin);
                choices.push(Choice::new(Some(origin), Some(transport), label, note));
            }
        }
        choices
    }

    /// A path row's value; its help gains the count of choices.
    pub(super) fn path_value<T: Copy + PartialEq>(&self, kind: &Kind<T>, help: &mut String) -> String {
        let (choices, chosen) = (self.choices(kind), (kind.chosen)(&self.config.paths));
        *help = format!("{help}. ←/→ picks one of {}.", choices.len());
        match choices.iter().find(|choice| choice.selects(chosen)) {
            Some(choice) if choice.note.is_empty() => choice.label.clone(),
            Some(choice) => format!("{} · {}", choice.label, choice.note),
            None => {
                *help = format!("Not offered by the checked server. {help}");
                let origin = chosen.0.map_or_else(|| "automatic origin".into(), Origin::to_string);
                format!("{} · {origin}", chosen.1.map_or("auto", kind.label))
            }
        }
    }

    /// Steps a path row to the next choice, or to the first when the chosen one is not offered: its label.
    pub(super) fn cycle_path<T: Copy + PartialEq>(&mut self, kind: &Kind<T>, step: isize) -> String {
        let choices = self.choices(kind);
        let chosen = (kind.chosen)(&self.config.paths);
        let at = choices.iter().position(|choice| choice.selects(chosen));
        let next = at.map_or(0, |at| (at as isize + step).rem_euclid(choices.len() as isize) as usize);
        let choice = &choices[next];
        (kind.choose)(&mut self.config.paths, choice.origin.clone(), choice.transport);
        choice.label.clone()
    }

    /// The HTTP version of the chosen throughput path when it does not negotiate one.
    pub(super) fn fixed_protocol(&self) -> Option<Protocol> {
        let (server, offered) = self.single()?;
        let (Some(origin), Some(transport)) = (THROUGHPUT.chosen)(&self.config.paths) else {
            return None;
        };
        let mut paths = (THROUGHPUT.offered)(offered, &server.origin).into_iter();
        let (.., version) = paths.find(|path| path.0 == *origin && path.1 == transport)?;
        (version != Protocol::Negotiated).then_some(version)
    }

    /// An origin by its authority, or by its port alone on the catalogue's host.
    fn short(&self, origin: &Origin) -> String {
        if self.config.url.as_ref().is_some_and(|url| url.host == origin.host)
            && origin.port != origin.scheme.default_port()
        {
            return format!(":{}", origin.port);
        }
        let text = origin.to_string();
        let authority = text.split_once("://").map(|(_, authority)| authority.to_owned());
        authority.unwrap_or(text)
    }
}

/// A path a preflight offered: its origin, transport and HTTP version.
type Offer<T> = (Origin, T, Protocol);

/// A path row's transport kind: nameable transports, labels, settings slot, offered paths by origin, checked origin.
pub(super) struct Kind<T> {
    shared: [T; 2],
    label: fn(T) -> &'static str,
    chosen: fn(&PathChoice) -> (Option<&Origin>, Option<T>),
    choose: fn(&mut PathChoice, Option<Origin>, Option<T>),
    offered: fn(&Capabilities, &Origin) -> Vec<Offer<T>>,
    checked: fn(&Paths) -> Option<&Origin>,
}

pub(super) const THROUGHPUT: Kind<ThroughputTransport> = Kind {
    shared: [ThroughputTransport::FetchStream, ThroughputTransport::WebTransport],
    label: words::throughput_transport,
    chosen: |paths| (paths.throughput_origin.as_ref(), paths.throughput_transport),
    choose: |paths, origin, transport| (paths.throughput_origin, paths.throughput_transport) = (origin, transport),
    offered: |offered, served| {
        let streams = offered.throughput.iter();
        let streams = streams.filter(|target| target.transport != ThroughputTransport::WebTransportDatagram);
        let offer =
            |target: &ThroughputTarget| (target.base_url.resolve(served).clone(), target.transport, target.protocol);
        streams.map(offer).collect()
    },
    checked: |paths| Some(&paths.throughput.origin),
};

pub(super) const LATENCY: Kind<LatencyTransport> = Kind {
    shared: [LatencyTransport::WebSocket, LatencyTransport::WebTransport],
    label: words::latency_transport,
    chosen: |paths| (paths.latency_origin.as_ref(), paths.latency_transport),
    choose: |paths, origin, transport| (paths.latency_origin, paths.latency_transport) = (origin, transport),
    offered: |offered, served| {
        let offer = |target: &LatencyTarget| {
            let origin = target.base_url.resolve(served).clone();
            (origin, target.transport, words::latency_protocol(target.transport))
        };
        offered.latency.iter().map(offer).collect()
    },
    checked: |paths| paths.latency.as_ref().map(|path| &path.origin),
};

/// A path row's choice: an origin and a transport, each automatic when none.
struct Choice<T> {
    origin: Option<Origin>,
    transport: Option<T>,
    label: String,
    note: String,
}

impl<T: PartialEq> Choice<T> {
    fn new(origin: Option<Origin>, transport: Option<T>, label: String, note: String) -> Self {
        Self { origin, transport, label, note }
    }

    fn selects(&self, (origin, transport): (Option<&Origin>, Option<T>)) -> bool {
        self.origin.as_ref() == origin && self.transport == transport
    }
}
