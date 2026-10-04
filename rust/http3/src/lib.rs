//! HTTP/3 and WebTransport over the noq QUIC fork, for both roles: sans-IO protocol modules and one
//! driver per connection that runs them.
mod budget;
mod capsule;
pub mod client;
mod code;
mod control;
mod driver;
mod error;
mod fields;
mod frame;
mod incoming;
mod message;
mod qpack;
pub mod server;
mod settings;
mod stream;
mod varint;
pub mod webtransport;

pub use {
    budget::Budget,
    code::{Code, WtCode},
    driver::CONNECTION_BYTES,
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
