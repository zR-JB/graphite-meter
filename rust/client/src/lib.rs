//! The Graphite Meter native terminal client.

pub mod config;
pub mod model;
pub mod net;

pub mod measure {
    pub mod aggregate;
    pub mod format;
    pub mod latency;
}

pub mod run {
    pub mod engine;
    pub mod prepare;
    pub mod select;
}

/// The version `--version` prints.
pub const VERSION: &str = match option_env!("GM_ENGINE_VERSION") {
    Some(version) => version,
    None => concat!(env!("CARGO_PKG_VERSION"), "-rust-dev"),
};
