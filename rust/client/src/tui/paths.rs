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
    discovery::{Capabilities, LatencyTransport, Protocol, ThroughputTransport},
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
        let stale = self
            .checked
            .as_ref()
            .is_none_or(|(_, at)| self.now.saturating_duration_since(*at) > FRESH);
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
        self.view
            .servers
            .iter()
            .filter(usable)
            .map(|server| server.id.clone())
            .collect()
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
    fn choices<T: Kind>(&self) -> Vec<Choice<T>> {
        let automatic = |note| Choice {
            origin: None,
            transport: None,
            label: "Automatic".into(),
            note,
        };
        let Some((server, offered)) = self.single() else {
            let mut choices = vec![automatic("each server".into())];
            for kind in T::SHARED {
                let offers = |server: &ServerPath| {
                    let offered = server.offered.as_ref();
                    offered.is_some_and(|offered| T::offered(offered, &server.origin).iter().any(|path| path.1 == kind))
                };
                let lacking = self.view.servers.iter().filter(|server| !offers(server));
                let names: Vec<_> = lacking.map(|server| server.name.as_str()).collect();
                let note = match names.is_empty() {
                    true => "every server".into(),
                    false => format!("unavailable on {}", names.join(", ")),
                };
                choices.push(Choice {
                    origin: None,
                    transport: Some(kind),
                    label: kind.label().into(),
                    note,
                });
            }
            return choices;
        };
        let checked = server.path.as_ref().ok().and_then(T::checked);
        let mut choices = vec![automatic(
            checked
                .map(|origin| format!("→ {}", self.short(origin)))
                .unwrap_or_default(),
        )];
        for (origin, transport, version) in T::offered(offered, &server.origin) {
            if !choices
                .iter()
                .any(|choice| choice.selects((Some(&origin), Some(transport))))
            {
                let (label, note) = (words::connection(transport.label(), version, &origin), self.short(&origin));
                choices.push(Choice {
                    origin: Some(origin),
                    transport: Some(transport),
                    label,
                    note,
                });
            }
        }
        choices
    }

    /// A path row's value; its help gains the count of choices.
    pub(super) fn path_value<T: Kind>(&self, help: &mut String) -> String {
        let (choices, chosen) = (self.choices::<T>(), T::chosen(&self.config.paths));
        *help = format!("{help}. ←/→ picks one of {}.", choices.len());
        match choices.iter().find(|choice| choice.selects(chosen)) {
            Some(choice) if choice.note.is_empty() => choice.label.clone(),
            Some(choice) => format!("{} · {}", choice.label, choice.note),
            None => {
                *help = format!("Not offered by the checked server. {help}");
                let origin = chosen.0.map_or_else(|| "automatic origin".into(), Origin::to_string);
                format!("{} · {origin}", chosen.1.map_or("auto", T::label))
            }
        }
    }

    /// Steps a path row to the next choice, or to the first when the chosen one is not offered: its label.
    pub(super) fn cycle_path<T: Kind>(&mut self, step: isize) -> String {
        let choices = self.choices::<T>();
        let at = choices
            .iter()
            .position(|choice| choice.selects(T::chosen(&self.config.paths)));
        let next = at.map_or(0, |at| (at as isize + step).rem_euclid(choices.len() as isize) as usize);
        let choice = &choices[next];
        T::choose(&mut self.config.paths, choice.origin.clone(), choice.transport);
        choice.label.clone()
    }

    /// The HTTP version of the chosen throughput path when it does not negotiate one.
    pub(super) fn fixed_protocol(&self) -> Option<Protocol> {
        let (server, offered) = self.single()?;
        let (Some(origin), Some(transport)) = ThroughputTransport::chosen(&self.config.paths) else {
            return None;
        };
        let mut paths = ThroughputTransport::offered(offered, &server.origin).into_iter();
        let (.., version) = paths.find(|path| path.0 == *origin && path.1 == transport)?;
        (version != Protocol::Negotiated).then_some(version)
    }

    /// An origin by its authority, or by its port alone on the catalogue's host.
    fn short(&self, origin: &Origin) -> String {
        if origin.host == self.config.url.host && origin.port != origin.scheme.default_port() {
            return format!(":{}", origin.port);
        }
        let text = origin.to_string();
        text.split_once("://")
            .map_or(text.clone(), |(_, authority)| authority.to_owned())
    }
}

/// A path row's kind of transport.
pub(super) trait Kind: Copy + PartialEq {
    /// The transports a choice for every server may name.
    const SHARED: [Self; 2];
    fn label(self) -> &'static str;
    fn chosen(paths: &PathChoice) -> (Option<&Origin>, Option<Self>);
    fn choose(paths: &mut PathChoice, origin: Option<Origin>, transport: Option<Self>);
    /// The paths of this kind a preflight offered, against the origin that served it, with their HTTP versions.
    fn offered(offered: &Capabilities, served: &Origin) -> Vec<(Origin, Self, Protocol)>;
    fn checked(paths: &Paths) -> Option<&Origin>;
}

impl Kind for ThroughputTransport {
    const SHARED: [Self; 2] = [Self::FetchStream, Self::WebTransport];

    fn label(self) -> &'static str {
        words::throughput_transport(self)
    }

    fn chosen(paths: &PathChoice) -> (Option<&Origin>, Option<Self>) {
        (paths.throughput_origin.as_ref(), paths.throughput_transport)
    }

    fn choose(paths: &mut PathChoice, origin: Option<Origin>, transport: Option<Self>) {
        (paths.throughput_origin, paths.throughput_transport) = (origin, transport);
    }

    fn offered(offered: &Capabilities, served: &Origin) -> Vec<(Origin, Self, Protocol)> {
        let streams = offered
            .throughput
            .iter()
            .filter(|target| target.transport != Self::WebTransportDatagram);
        streams
            .map(|target| (target.base_url.resolve(served).clone(), target.transport, target.protocol))
            .collect()
    }

    fn checked(paths: &Paths) -> Option<&Origin> {
        Some(&paths.throughput.origin)
    }
}

impl Kind for LatencyTransport {
    const SHARED: [Self; 2] = [Self::WebSocket, Self::WebTransport];

    fn label(self) -> &'static str {
        words::latency_transport(self)
    }

    fn chosen(paths: &PathChoice) -> (Option<&Origin>, Option<Self>) {
        (paths.latency_origin.as_ref(), paths.latency_transport)
    }

    fn choose(paths: &mut PathChoice, origin: Option<Origin>, transport: Option<Self>) {
        (paths.latency_origin, paths.latency_transport) = (origin, transport);
    }

    fn offered(offered: &Capabilities, served: &Origin) -> Vec<(Origin, Self, Protocol)> {
        let version = words::latency_protocol;
        let paths = offered.latency.iter();
        paths
            .map(|target| (target.base_url.resolve(served).clone(), target.transport, version(target.transport)))
            .collect()
    }

    fn checked(paths: &Paths) -> Option<&Origin> {
        paths.latency.as_ref().map(|path| &path.origin)
    }
}

/// A path row's choice: an origin and a transport, each automatic when none.
struct Choice<T> {
    origin: Option<Origin>,
    transport: Option<T>,
    label: String,
    note: String,
}

impl<T: Kind> Choice<T> {
    fn selects(&self, (origin, transport): (Option<&Origin>, Option<T>)) -> bool {
        self.origin.as_ref() == origin && self.transport == transport
    }
}
