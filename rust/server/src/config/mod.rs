//! The server's settings: environment and flags through one table, validated into a plain `Config`.

mod catalog;
mod settings;
mod validate;

use graphite_meter_proto::{
    catalog::ServerCatalog,
    discovery::Protocol,
    origin::{BaseUrl, Origin, Scheme},
};
use ipnet::IpNet;
use std::{ffi::OsString, fmt, io::Write, path::PathBuf, time::Duration};

/// The engine version `version` prints and discovery reports.
pub const ENGINE_VERSION: &str = match option_env!("GM_ENGINE_VERSION") {
    Some(version) => version,
    None => concat!(env!("CARGO_PKG_VERSION"), "-rust-dev"),
};

/// What the command line asked for.
#[derive(Debug)]
pub enum Loaded {
    /// `-h` or `-help`: the usage went to the usage writer.
    Help,
    Config(Box<Config>),
}

/// Loads the environment, then the flags over it, and validates the result. Flag errors and the usage go to `usage`;
/// the returned message is Go's.
pub fn load(
    env: impl Fn(&str) -> Option<OsString>,
    args: impl IntoIterator<Item = OsString>,
    usage: &mut dyn Write,
) -> Result<Loaded, String> {
    match settings::load(&env, args, usage)? {
        None => Ok(Loaded::Help),
        Some(settings) => validate::config(settings).map(|config| Loaded::Config(Box::new(config))),
    }
}

/// A validated configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// The enabled listeners, in `ListenerKind` order.
    pub listeners: Vec<Listener>,
    /// Certificate and key paths, present when a TLS listener is enabled.
    pub tls: Option<TlsFiles>,
    pub public: PublicOrigins,
    pub result_history_default: bool,
    pub verbose: bool,
    pub trusted_proxies: Vec<IpNet>,
    pub limits: Limits,
    pub max_buffer_bytes: usize,
    pub lifetimes: Lifetimes,
    pub auth: Option<Auth>,
    /// The published catalogue: `self` first, carrying the server's name and location.
    pub catalog: ServerCatalog,
}

impl Config {
    pub fn name(&self) -> &str {
        &self.catalog.servers[0].name
    }

    pub fn location(&self) -> &str {
        &self.catalog.servers[0].location
    }

    pub fn listener(&self, kind: ListenerKind) -> Option<&Listener> {
        self.listeners.iter().find(|listener| listener.kind == kind)
    }
}

/// A native listener, in discovery's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ListenerKind {
    H1,
    H1Tls,
    H2,
    H3,
}

impl ListenerKind {
    pub const ALL: [Self; 4] = [Self::H1, Self::H1Tls, Self::H2, Self::H3];

    /// Its endpoint name, setting prefix, scheme and protocol.
    const fn row(self) -> (&'static str, &'static str, Scheme, Protocol) {
        match self {
            Self::H1 => ("http1-clear", "GM_H1", Scheme::Http, Protocol::Http1),
            Self::H1Tls => ("http1-tls", "GM_H1_TLS", Scheme::Https, Protocol::Http1),
            Self::H2 => ("http2", "GM_H2", Scheme::Https, Protocol::Http2),
            Self::H3 => ("http3", "GM_H3", Scheme::Https, Protocol::Http3),
        }
    }

    /// Its name in `GM_ADVERTISED_NATIVE_ENDPOINTS`.
    pub const fn name(self) -> &'static str {
        self.row().0
    }

    /// The prefix of its `_ADDR` and `_PUBLIC_ORIGIN` settings.
    pub const fn env(self) -> &'static str {
        self.row().1
    }

    pub const fn scheme(self) -> Scheme {
        self.row().2
    }

    pub const fn protocol(self) -> Protocol {
        self.row().3
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

/// An enabled listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub kind: ListenerKind,
    /// The listen address, such as `:7246`, with the port it bound once bound; HTTP/3 binds it for UDP and its
    /// bootstrap TCP.
    pub address: String,
    pub public_origin: Option<Origin>,
    /// Discovery offers it as a native target.
    pub advertised: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// Negotiated origins discovery advertises; `BaseUrl::Served` is `self`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublicOrigins {
    pub both: Vec<BaseUrl>,
    pub throughput: Vec<BaseUrl>,
    pub latency: Vec<BaseUrl>,
}

/// Concurrency limits, each positive and per-client ones within their totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub operations: usize,
    pub operations_per_client: usize,
    pub sessions: usize,
    pub sessions_per_client: usize,
    pub connections: usize,
    pub connections_per_client: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lifetimes {
    /// A request, WebSocket bus or `/wt/ping` session.
    pub operation: Duration,
    /// A WebTransport transfer session.
    pub session: Duration,
    /// The longest stage clients may plan.
    pub stage: Duration,
}

/// Authentication settings, present when a mode is enabled.
#[derive(Debug, Clone)]
pub struct Auth {
    /// The canonical HTTPS origin of the UI.
    pub public_origin: Origin,
    pub methods: Methods,
}

#[derive(Debug, Clone)]
pub enum Methods {
    Password(Secret),
    Oidc(Oidc),
    Hybrid(Secret, Oidc),
}

impl Methods {
    /// The operator password hash source, when password sign-in is enabled.
    pub fn password(&self) -> Option<&Secret> {
        match self {
            Self::Password(secret) | Self::Hybrid(secret, _) => Some(secret),
            Self::Oidc(_) => None,
        }
    }

    pub fn oidc(&self) -> Option<&Oidc> {
        match self {
            Self::Oidc(oidc) | Self::Hybrid(_, oidc) => Some(oidc),
            Self::Password(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Oidc {
    pub issuer: String,
    pub client_id: String,
    pub secret: Secret,
    pub allowed_groups: Vec<String>,
    pub provider_name: String,
}

/// A secret given inline in the environment or as a file to read.
#[derive(Clone)]
pub enum Secret {
    Inline(String),
    File(PathBuf),
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inline(_) => formatter.write_str("Inline(..)"),
            Self::File(path) => formatter.debug_tuple("File").field(path).finish(),
        }
    }
}

/// Go's `os.PathError` text: the operation, the path and the system's message in lower case.
pub(crate) fn path_error(operation: &str, path: &str, error: &std::io::Error) -> String {
    let message = error.to_string().to_lowercase();
    let message = message.split(" (os error").next().unwrap_or_default();
    format!("{operation} {path}: {message}")
}
