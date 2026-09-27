//! HTTP/3 and WebTransport for Graphite Meter's routes, shared by the server and the client.
// The fuzz harness builds the codec without the connection layer that calls it.
#![cfg_attr(not(feature = "io"), allow(dead_code))]

mod capsule;
#[cfg(feature = "io")]
mod charge;
#[cfg(feature = "io")]
pub mod client;
mod code;
#[cfg(feature = "io")]
mod connection;
#[cfg(feature = "io")]
mod error;
mod fields;
mod frame;
mod message;
mod qpack;
#[cfg(feature = "io")]
pub mod server;
mod settings;
#[cfg(feature = "io")]
mod stream;
mod varint;
#[cfg(feature = "io")]
pub mod webtransport;

pub use code::{Code, WtCode};
#[cfg(feature = "io")]
pub use {
    charge::Budget,
    connection::CONNECTION_BYTES,
    error::Error,
    stream::{RecvHalf, RequestStream, SendHalf},
};

#[cfg(any(test, fuzzing))]
#[doc(hidden)]
pub mod fuzz;
