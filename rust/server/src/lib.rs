//! Experimental Graphite Meter server.
#![forbid(unsafe_code)]

/// A failure while the server starts or runs, as opposed to a `config::ConfigError` in what it was given.
pub type ServerError = Box<dyn std::error::Error + Send + Sync>;

#[cfg(test)]
#[path = "../../test_identity.rs"]
mod test_identity;
#[cfg(test)]
#[path = "../../test_link.rs"]
mod test_link;

pub mod admission;
pub mod assets;
pub mod auth;
mod budget;
pub mod catalog;
pub mod client_address;
pub mod config;
pub mod connections;
pub mod cors;
pub mod discovery;
pub mod http;
pub mod log;
mod meter;
pub mod password;
pub mod ping;
pub mod preflight;
mod probe;
mod quic_shard;
pub mod runtime;
mod timeouts;
pub mod tls;
pub mod upload;
pub mod websocket;
