//! Native client orchestration, separate from terminal rendering.

pub mod cli;
pub mod config;
pub mod crypto;
pub mod download;
pub mod latency;
pub mod model;
pub mod net;
pub mod quic;
pub mod runner;
pub mod selection;
pub mod stream_plan;
pub mod transport;
pub mod upload;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

mod theme;
mod tls;
pub mod webtransport;

pub mod ui;

pub mod controller;
