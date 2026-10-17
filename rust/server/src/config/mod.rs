//! Reads the server's settings from environment and flags through one table, and validates them into a plain `Config`.

mod catalog;
mod settings;
mod validate;

use graphite_meter_net::{Proxy, Trust, Verify};
use graphite_meter_proto::{
    catalog::ServerCatalog,
    discovery::Protocol,
    origin::{BaseUrl, Origin, Scheme},
};
use ipnet::IpNet;
use std::{
    ffi::OsString,
    fmt,
    fs::File,
    io::{Read, Write},
    num::NonZero,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use zeroize::Zeroizing;

/// `version` prints this engine version, and discovery reports it.
pub const ENGINE_VERSION: &str = match option_env!("GM_ENGINE_VERSION") {
    Some(version) => version,
    None => concat!(env!("CARGO_PKG_VERSION"), "-dev"),
};

/// The command line asked for one of these.
#[derive(Debug)]
pub enum Loaded {
    /// `-h` or `-help` sent the usage to the usage writer.
    Help,
    Config(Box<Config>),
}

/// Loads the environment, then the flags, and validates. Flag errors and usage go to `usage`.
pub fn load(
    env: impl Fn(&str) -> Option<OsString>,
    args: impl IntoIterator<Item = OsString>,
    usage: &mut dyn Write,
) -> Result<Loaded, String> {
    match settings::load(&env, args, usage)? {
        None => Ok(Loaded::Help),
        Some(settings) => {
            validate::config(settings, Outbound::from_lookup(&env)).map(|config| Loaded::Config(Box::new(config)))
        }
    }
}

/// Config holds a validated configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// The enabled listeners come in `ListenerKind` order.
    pub listeners: Vec<Listener>,
    /// Certificate and key paths are present when a TLS listener is enabled.
    pub tls: Option<TlsFiles>,
    pub public: PublicOrigins,
    pub result_history_default: bool,
    pub verbose: bool,
    pub trusted_proxies: Vec<IpNet>,
    pub limits: Limits,
    pub max_buffer_bytes: usize,
    pub lifetimes: Lifetimes,
    pub auth: Option<Auth>,
    /// The published catalog lists `self` first, carrying the server's name and location.
    pub catalog: ServerCatalog,
    pub outbound: Outbound,
}

/// OIDC requests leave through the proxies and roots named by the same environment as the settings.
#[derive(Debug, Clone)]
pub struct Outbound {
    pub proxy: Proxy,
    pub verify: Verify,
}

impl Outbound {
    fn from_lookup(env: &impl Fn(&str) -> Option<OsString>) -> Self {
        let proxy = Proxy::from_lookup(|name| env(name).and_then(|value| value.into_string().ok()));
        Self {
            proxy,
            verify: Verify::Trusted(Arc::new(Trust::from_lookup(env))),
        }
    }
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

graphite_meter_proto::table! {
    /// Native listeners are listed in discovery's order.
    #[derive(PartialOrd, Ord)]
    pub enum ListenerKind {
        /// `GM_ADVERTISED_NATIVE_ENDPOINTS` knows it by this name.
        name.0: &'static str,
        /// Its `_ADDR` and `_PUBLIC_ORIGIN` settings start with this prefix.
        env.1: &'static str,
        scheme.2: Scheme,
        protocol.3: Protocol,
    } {
        H1 => ("http1-clear", "GM_H1", Scheme::Http, Protocol::Http1),
        H1Tls => ("http1-tls", "GM_H1_TLS", Scheme::Https, Protocol::Http1),
        H2 => ("http2", "GM_H2", Scheme::Https, Protocol::Http2),
        H3 => ("http3", "GM_H3", Scheme::Https, Protocol::Http3),
    }
}

/// Describes one enabled listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub kind: ListenerKind,
    /// Carries its bound port once bound. HTTP/3 binds it for UDP and bootstrap TCP.
    pub address: ListenAddress,
    pub public_origin: Option<Origin>,
    /// Discovery offers it as a native target.
    pub advertised: bool,
}

/// Reads `host:port`, an IPv6 host in brackets, or `:port` for every address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenAddress {
    pub host: Option<String>,
    pub port: u16,
}

impl ListenAddress {
    fn parse(text: &str) -> Option<Self> {
        let (host, port) = text.rsplit_once(':')?;
        let port = Some(port).filter(|port| port.bytes().all(|byte| byte.is_ascii_digit()))?;
        let bracketed = host.starts_with('[') && host.ends_with(']');
        let host = match host {
            "" => None,
            host if host.contains(':') && !bracketed => return None,
            host => Some(host.to_owned()),
        };
        Some(Self { host, port: port.parse().ok()? })
    }
}

impl fmt::Display for ListenAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.host.as_deref().unwrap_or_default(), self.port)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// Discovery advertises these negotiated origins. `BaseUrl::Served` is `self`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublicOrigins {
    pub both: Vec<BaseUrl>,
    pub throughput: Vec<BaseUrl>,
    pub latency: Vec<BaseUrl>,
}

/// Each concurrency limit is positive, and per-client ones stay within their totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub operations: NonZero<usize>,
    pub operations_per_client: NonZero<usize>,
    pub sessions: NonZero<usize>,
    pub sessions_per_client: NonZero<usize>,
    pub connections: NonZero<usize>,
    pub connections_per_client: NonZero<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lifetimes {
    /// A request, WebSocket bus or `/wt/ping` session lives this long.
    pub operation: Duration,
    /// A WebTransport transfer session lives this long.
    pub session: Duration,
    /// Clients may plan a stage this long at most.
    pub stage: Duration,
}

/// Authentication settings are present when a mode is enabled.
#[derive(Debug, Clone)]
pub struct Auth {
    /// The UI lives at this canonical HTTPS origin.
    pub public_origin: Origin,
    pub methods: Methods,
    /// The sign-in page names this OIDC provider in every mode.
    pub provider: String,
}

#[derive(Debug, Clone)]
pub enum Methods {
    Password(Secret),
    Oidc(Oidc),
    Hybrid(Secret, Oidc),
}

impl Methods {
    /// Gives the operator password hash source when password sign-in is enabled.
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
    pub allowed_groups: Groups,
}

/// The OIDC groups allowed to sign in are never none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Groups(Vec<String>);

impl Groups {
    pub fn new(groups: Vec<String>) -> Option<Self> {
        (!groups.is_empty()).then_some(Self(groups))
    }

    pub fn contains(&self, group: &str) -> bool {
        self.0.iter().any(|allowed| allowed == group)
    }

    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

/// A secret comes inline in the environment or as a file to read.
#[derive(Clone)]
pub enum Secret {
    Inline(Zeroizing<String>),
    File(PathBuf),
}

impl Secret {
    /// Reads the trimmed secret. A file is read up to `limit` bytes and must hold one.
    pub fn read(&self, limit: u64) -> Result<Zeroizing<String>, String> {
        let path = match self {
            Self::Inline(secret) => return Ok(Zeroizing::new(secret.trim().to_owned())),
            Self::File(path) => path,
        };
        let failed = |action, error| could_not(&format!("{action} {}", path.display()), &error);
        let mut bytes = Zeroizing::new(Vec::new());
        let file = File::open(path).map_err(|error| failed("open", error))?;
        file.take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| failed("read", error))?;
        if bytes.len() as u64 > limit {
            return Err(format!("secret file exceeds {limit} bytes"));
        }
        let secret = Zeroizing::new(String::from_utf8_lossy(&bytes).trim().to_owned());
        if secret.is_empty() {
            return Err("secret is empty".into());
        }
        Ok(secret)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inline(_) => formatter.write_str("Inline(..)"),
            Self::File(path) => formatter.debug_tuple("File").field(path).finish(),
        }
    }
}

/// Formats `could not {action}: {message}` with the system's message in lower case without its code.
pub(crate) fn could_not(action: &str, error: &std::io::Error) -> String {
    let message = error.to_string().to_lowercase();
    let message = message.split(" (os error").next().unwrap_or_default();
    format!("could not {action}: {message}")
}
