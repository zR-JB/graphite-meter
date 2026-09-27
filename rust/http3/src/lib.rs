//! HTTP/3 and WebTransport for Graphite Meter's routes, shared by the server and the client.
// WebTransport's codec is called from the session layer, which comes next.
#![allow(dead_code)]

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
