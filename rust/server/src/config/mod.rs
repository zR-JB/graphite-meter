//! The server's settings: environment and flags through one table, validated into a plain `Config`.

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
use jiff::tz::{TimeZone, TimeZoneDatabase};
use std::{
    ffi::{OsStr, OsString},
    fmt,
    fs::File,
    io::{Read, Write},
    num::NonZero,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use zeroize::Zeroizing;

/// The engine version `version` prints and discovery reports.
pub const ENGINE_VERSION: &str = match option_env!("GM_ENGINE_VERSION") {
    Some(version) => version,
    None => concat!(env!("CARGO_PKG_VERSION"), "-dev"),
};

#[derive(Debug)]
pub enum Loaded {
    Help,
    Config(Box<Config>),
}

/// Loads the environment, then the flags, and validates; argument errors and usage go to `usage`.
pub fn load(
    env: impl Fn(&str) -> Option<OsString>,
    args: impl IntoIterator<Item = OsString>,
    usage: &mut dyn Write,
) -> Result<Loaded, String> {
    match settings::load(&env, args, usage)? {
        None => Ok(Loaded::Help),
        Some(settings) => {
            let log_zone = log_zone(env("TZ"))?;
            validate::config(settings, Outbound::from_lookup(&env), log_zone)
                .map(|config| Loaded::Config(Box::new(config)))
        }
    }
}

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
    /// The published catalog: `self` first, carrying the server's name and location.
    pub catalog: ServerCatalog,
    pub outbound: Outbound,
    pub log_zone: Option<TimeZone>,
}

/// `TZ` names a zone of the bundled database, so the host's zone files are never read; unset, empty or UTC is UTC.
fn log_zone(value: Option<OsString>) -> Result<Option<TimeZone>, String> {
    match value.as_deref().map(OsStr::to_str) {
        None | Some(Some("" | "UTC" | "Etc/UTC")) => Ok(None),
        Some(name) => name
            .and_then(|name| TimeZoneDatabase::bundled().get(name).ok())
            .filter(|zone| !zone.is_unknown())
            .map(Some)
            .ok_or_else(|| "TZ must be a time zone name such as Europe/Berlin".into()),
    }
}

/// How OIDC requests leave: the proxies and trust roots the settings' environment names.
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
    #[derive(PartialOrd, Ord)]
    pub enum ListenerKind {
        /// Its name in `GM_ADVERTISED_NATIVE_ENDPOINTS`.
        name.0: &'static str,
        /// The prefix of its `_ADDR` and `_PUBLIC_ORIGIN` settings.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub kind: ListenerKind,
    /// With its bound port once bound; HTTP/3 binds it for UDP and bootstrap TCP.
    pub address: ListenAddress,
    pub public_origin: Option<Origin>,
    pub advertised: bool,
}

/// `host:port`, an IPv6 host in brackets, or `:port` for every address.
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
        Some(Self {
            host,
            port: port.parse().ok()?,
        })
    }
}

impl fmt::Display for ListenAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}",
            self.host.as_deref().unwrap_or_default(),
            self.port
        )
    }
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
    pub operations: NonZero<usize>,
    pub operations_per_client: NonZero<usize>,
    pub sessions: NonZero<usize>,
    pub sessions_per_client: NonZero<usize>,
    pub connections: NonZero<usize>,
    pub connections_per_client: NonZero<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lifetimes {
    /// A request, WebSocket bus or `/wt/ping` session.
    pub operation: Duration,
    pub session: Duration,
    pub stage: Duration,
}

#[derive(Debug, Clone)]
pub struct Auth {
    pub public_origin: Origin,
    pub methods: Methods,
    /// The OIDC provider's name, which the sign-in page names in every mode.
    pub provider: String,
}

/// The enabled sign-in methods, at least one: the operator password hash source and OIDC.
#[derive(Debug, Clone)]
pub struct Methods {
    pub password: Option<Secret>,
    pub oidc: Option<Oidc>,
}

#[derive(Debug, Clone)]
pub struct Oidc {
    pub issuer: String,
    pub client_id: String,
    pub secret: Secret,
    pub allowed_groups: Groups,
}

/// The OIDC groups allowed to sign in, never none.
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

#[derive(Clone)]
pub enum Secret {
    Inline(Zeroizing<String>),
    File(PathBuf),
}

impl Secret {
    /// The trimmed secret; a file is read up to `limit` bytes and must hold one.
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

/// `could not {action}: {message}`, the system's message in lower case without its code.
pub(crate) fn could_not(action: &str, error: &std::io::Error) -> String {
    let message = error.to_string().to_lowercase();
    let message = message.split(" (os error").next().unwrap_or_default();
    format!("could not {action}: {message}")
}
