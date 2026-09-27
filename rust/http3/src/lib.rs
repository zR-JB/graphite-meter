//! HTTP/3 and WebTransport for Graphite Meter's routes, shared by the server and the client.
// The codec's callers arrive with the connection layer.
#![allow(dead_code)]

mod capsule;
mod code;
mod fields;
mod frame;
mod qpack;
mod settings;
mod varint;

pub use code::{Code, WtCode};

#[cfg(any(test, fuzzing))]
#[doc(hidden)]
pub mod fuzz;
