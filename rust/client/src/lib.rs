//! The Graphite Meter native terminal client.

pub mod config;
pub mod model;

pub mod measure {
    pub mod aggregate;
    pub mod format;
    pub mod latency;
}

/// The version `--version` prints.
pub const VERSION: &str = match option_env!("GM_ENGINE_VERSION") {
    Some(version) => version,
    None => concat!(env!("CARGO_PKG_VERSION"), "-rust-dev"),
};
