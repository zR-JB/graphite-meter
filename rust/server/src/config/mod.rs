//! Resolved server configuration shared by every listener and request policy.
mod load;
mod validate;

use crate::admission::Limits;
use graphite_meter_core::catalog::ServerCatalog;
use std::{collections::BTreeSet, error::Error, time::Duration};

pub type ConfigError = Box<dyn Error + Send + Sync>;

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
    pub const fn name(self) -> &'static str {
        match self {
            Self::H1 => "http1-clear",
            Self::H1Tls => "http1-tls",
            Self::H2 => "http2",
            Self::H3 => "http3",
        }
    }
    pub const fn address_env(self) -> &'static str {
        match self {
            Self::H1 => "GM_H1_ADDR",
            Self::H1Tls => "GM_H1_TLS_ADDR",
            Self::H2 => "GM_H2_ADDR",
            Self::H3 => "GM_H3_ADDR",
        }
    }
    pub const fn origin_env(self) -> &'static str {
        match self {
            Self::H1 => "GM_H1_PUBLIC_ORIGIN",
            Self::H1Tls => "GM_H1_TLS_PUBLIC_ORIGIN",
            Self::H2 => "GM_H2_PUBLIC_ORIGIN",
            Self::H3 => "GM_H3_PUBLIC_ORIGIN",
        }
    }
    pub const fn protocol(self) -> &'static str {
        match self {
            Self::H1 | Self::H1Tls => "http1",
            Self::H2 => "http2",
            Self::H3 => "http3",
        }
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
    pub max_connections: usize,
    pub max_connections_per_client: usize,
    pub max_operation_duration: Duration,
    pub max_session_duration: Duration,
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
            max_connections: 512,
            max_connections_per_client: 64,
            max_operation_duration: Duration::from_secs(300),
            max_session_duration: Duration::from_secs(7200),
            auth: AuthConfig::default(),
        }
    }
}
impl Config {
    pub fn listener(&self, kind: NativeKind) -> &NativeListener {
        &self.native[kind as usize]
    }
    pub fn native_advertised(&self, kind: NativeKind) -> bool {
        !self.listener(kind).address.is_empty()
            && self
                .advertised_native
                .as_ref()
                .is_none_or(|set| set.contains(&kind))
    }
}
