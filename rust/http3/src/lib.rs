//! HTTP/3 and WebTransport for Graphite Meter's routes, shared by the server and the client.
// The codec's callers arrive with the connection layer.
#![allow(dead_code)]

mod frame;
mod varint;

#[cfg(any(test, fuzzing))]
#[doc(hidden)]
pub mod fuzz;
