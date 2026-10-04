//! HTTP/3 and WebTransport for Graphite Meter's routes, shared by the server and the client.

mod capsule;
mod charge;
pub mod client;
mod code;
mod connection;
mod control;
mod error;
mod fields;
mod frame;
mod message;
mod qpack;
pub mod server;
mod settings;
mod stream;
mod varint;
pub mod webtransport;

pub use code::{Code, WtCode};
pub use {
    charge::Budget,
    connection::CONNECTION_BYTES,
    error::Error,
    stream::{RecvHalf, RequestStream, SendHalf},
};

#[cfg(test)]
fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
        .collect()
}
