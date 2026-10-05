//! The Graphite Meter native terminal client.

pub mod config;
pub mod controller;
pub mod events;
pub mod model;
pub mod net;

pub mod measure {
    pub mod aggregate;
    pub mod format;
    pub mod latency;
}

pub mod run {
    pub mod coordinator;
    pub mod engine;
    pub mod participant;
    pub mod prepare;
    pub mod probe;
    pub mod select;
    pub mod upload;
}

/// The version `--version` prints.
pub const VERSION: &str = match option_env!("GM_ENGINE_VERSION") {
    Some(version) => version,
    None => concat!(env!("CARGO_PKG_VERSION"), "-rust-dev"),
};
