//! Native client orchestration, separate from terminal rendering.
#![forbid(unsafe_code)]

pub mod cli;
pub mod config;
pub mod crypto;
pub mod download;
pub mod failure;
pub mod latency;
pub mod model;
pub mod net;
pub mod quic;
pub mod report;
pub mod runner;
pub mod selection;
pub mod transport;
pub mod upload;
pub mod vocabulary;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

mod theme;
mod tls;
pub mod webtransport;

pub mod ui;

pub mod controller;

#[cfg(test)]
#[path = "../../test_identity.rs"]
mod test_identity;
