//! Native client orchestration, separate from terminal rendering.

pub mod cli;
pub mod config;
pub mod download;
pub mod latency;
pub mod model;
pub mod net;
pub mod quic;
pub mod selection;
pub mod transport;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

mod tls;
pub mod webtransport;

pub mod ui;
