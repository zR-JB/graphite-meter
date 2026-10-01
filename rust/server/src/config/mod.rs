//! Resolved server configuration shared by every listener and request policy.
mod load;
mod validate;

pub(crate) use load::go_duration;
pub use load::load;

use crate::admission::Limits;
use graphite_meter_core::{
    catalog::ServerCatalog,
    origin::{Origin, target_origin},
};
use std::{collections::BTreeSet, error::Error, ops::Deref, time::Duration};

pub type ConfigError = Box<dyn Error + Send + Sync>;

/// Go's os.PathError text: the operation, the path and the C library's message, in lower case.
pub(crate) fn path_error(operation: &str, path: impl std::fmt::Display, error: &std::io::Error) -> String {
    let message = error.to_string().to_lowercase();
    format!(
        "{operation} {path}: {}",
        message.split(" (os error").next().unwrap_or_default()
    )
}

pub const ENGINE_VERSION: &str = match option_env!("GM_ENGINE_VERSION") {
    Some(version) => version,
    None => concat!(env!("CARGO_PKG_VERSION"), "-rust-dev"),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NativeKind {
    H1,
    H1Tls,
    H2,
    H3,
}

impl NativeKind {
    pub const ALL: [Self; 4] = [Self::H1, Self::H1Tls, Self::H2, Self::H3];
    /// The endpoint name, its address and origin settings, and its protocol.
    const fn row(self) -> [&'static str; 4] {
        match self {
            Self::H1 => ["http1-clear", "GM_H1_ADDR", "GM_H1_PUBLIC_ORIGIN", "http1"],
            Self::H1Tls => ["http1-tls", "GM_H1_TLS_ADDR", "GM_H1_TLS_PUBLIC_ORIGIN", "http1"],
            Self::H2 => ["http2", "GM_H2_ADDR", "GM_H2_PUBLIC_ORIGIN", "http2"],
            Self::H3 => ["http3", "GM_H3_ADDR", "GM_H3_PUBLIC_ORIGIN", "http3"],
        }
    }
    pub const fn name(self) -> &'static str {
        self.row()[0]
    }
    pub const fn address_env(self) -> &'static str {
        self.row()[1]
    }
    pub const fn origin_env(self) -> &'static str {
        self.row()[2]
    }
    pub const fn protocol(self) -> &'static str {
        self.row()[3]
    }
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

#[derive(Debug, Clone, Default)]
pub struct NativeListener {
    pub address: String,
    pub public_origin: String,
}

#[derive(Debug, Clone, Default)]
pub struct PublicOrigins {
    pub both: Vec<String>,
    pub throughput: Vec<String>,
    pub latency: Vec<String>,
}
impl PublicOrigins {
    pub fn lists(&self) -> [(&'static str, &[String]); 3] {
        [
            ("GM_PUBLIC_ORIGINS", &self.both),
            ("GM_PUBLIC_THROUGHPUT_ORIGINS", &self.throughput),
            ("GM_PUBLIC_LATENCY_ORIGINS", &self.latency),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    #[default]
    Off,
    Password,
    Oidc,
    Hybrid,
}
impl AuthMode {
    pub const ALL: [Self; 4] = [Self::Off, Self::Password, Self::Oidc, Self::Hybrid];
    /// Its GM_AUTH_MODE value.
    pub const fn name(self) -> &'static str {
        ["off", "password", "oidc", "hybrid"][self as usize]
    }
    pub fn password(self) -> bool {
        matches!(self, Self::Password | Self::Hybrid)
    }
    pub fn oidc(self) -> bool {
        matches!(self, Self::Oidc | Self::Hybrid)
    }
}

// Deliberately no Debug: resolved secrets must not reach diagnostic config dumps.
#[derive(Clone)]
pub struct AuthConfig {
    pub explicit: bool,
    pub mode: AuthMode,
    /// GM_AUTH_MODE names no mode. As Go's text setting, it loads, and validation refuses it after the flags apply.
    pub unknown_mode: bool,
    pub public_url: String,
    pub password_hash: String,
    pub password_hash_file: String,
    pub oidc_issuer: String,
    pub oidc_client_id: String,
    pub oidc_client_secret: String,
    pub oidc_secret_file: String,
    pub oidc_allowed_groups: Vec<String>,
    pub oidc_provider_name: String,
}
impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            explicit: false,
            mode: AuthMode::Off,
            unknown_mode: false,
            public_url: String::new(),
            password_hash: String::new(),
            password_hash_file: String::new(),
            oidc_issuer: String::new(),
            oidc_client_id: String::new(),
            oidc_client_secret: String::new(),
            oidc_secret_file: String::new(),
            oidc_allowed_groups: Vec::new(),
            oidc_provider_name: "Authelia".into(),
        }
    }
}

#[derive(Clone)]
pub struct Config {
    pub server_catalog: ServerCatalog,
    pub native: [NativeListener; 4],
    /// None means all enabled native listeners; an empty set advertises none.
    pub advertised_native: Option<BTreeSet<NativeKind>>,
    pub public: PublicOrigins,
    pub tls_cert: String,
    pub tls_key: String,
    pub server_name: String,
    pub server_location: String,
    pub engine_version: String,
    pub result_history_default: bool,
    pub verbose: bool,
    pub trusted_proxies: Vec<ipnet::IpNet>,
    pub limits: Limits,
    pub max_buffer_bytes: usize,
    pub max_connections: usize,
    pub max_connections_per_client: usize,
    pub max_operation_duration: Duration,
    pub max_session_duration: Duration,
    pub max_stage_duration: Duration,
    pub auth: AuthConfig,
}
impl Default for Config {
    fn default() -> Self {
        let mut native = std::array::from_fn(|_| NativeListener::default());
        native[NativeKind::H1 as usize].address = ":7246".into();
        Self {
            server_catalog: ServerCatalog::singleton(),
            native,
            advertised_native: None,
            public: PublicOrigins::default(),
            tls_cert: String::new(),
            tls_key: String::new(),
            server_name: "graphite-meter".into(),
            server_location: String::new(),
            engine_version: ENGINE_VERSION.into(),
            result_history_default: false,
            verbose: false,
            trusted_proxies: Vec::new(),
            limits: Limits::default(),
            max_buffer_bytes: 8 * 1024 * 1024 * 1024,
            max_connections: 4096,
            max_connections_per_client: 64,
            max_operation_duration: Duration::from_secs(300),
            max_session_duration: Duration::from_secs(7200),
            max_stage_duration: graphite_meter_core::discovery::DEFAULT_STAGE_LIMIT,
            auth: AuthConfig::default(),
        }
    }
}

/// A configuration `Config::validate` accepted. The runtime, its listeners and discovery take only this, so a
/// configuration is validated once, where it is loaded.
#[derive(Clone)]
pub struct ValidatedConfig(Config);

impl Deref for ValidatedConfig {
    type Target = Config;

    fn deref(&self) -> &Config {
        &self.0
    }
}

impl Config {
    /// This configuration, once `validate` accepts it, with a canonical authentication origin.
    pub fn validated(mut self) -> Result<ValidatedConfig, ConfigError> {
        self.validate()?;
        if self.auth.mode != AuthMode::Off {
            // Go keeps the spelling, and so refuses every sign-in whose browser writes the origin as browsers do: its
            // host in lower case, and its port without leading zeros, or none where it is the default.
            let origin = target_origin(&self.auth.public_url)?.ok_or("GM_AUTH_PUBLIC_URL names no origin")?;
            let port = Some(origin.port_number().to_string());
            self.auth.public_url = Origin { port, ..origin }.key();
        }
        Ok(ValidatedConfig(self))
    }

    pub fn published_catalog(&self) -> ServerCatalog {
        let mut catalog = if self.server_catalog.servers.is_empty() {
            ServerCatalog::singleton()
        } else {
            self.server_catalog.clone()
        };
        catalog.servers[0].name.clone_from(&self.server_name);
        catalog.servers[0].location.clone_from(&self.server_location);
        catalog
    }
    pub fn listener(&self, kind: NativeKind) -> &NativeListener {
        &self.native[kind as usize]
    }
    pub fn native_advertised(&self, kind: NativeKind) -> bool {
        !self.listener(kind).address.is_empty() && self.advertised_native.as_ref().is_none_or(|set| set.contains(&kind))
    }
}
