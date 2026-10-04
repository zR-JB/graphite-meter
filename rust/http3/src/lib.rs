//! HTTP/3 and WebTransport over the noq QUIC fork, for both roles.
// The IO layer that uses the protocol modules follows them.
#![allow(dead_code)]

mod capsule;
mod code;
mod control;
mod fields;
mod frame;
mod message;
mod qpack;
mod settings;
mod varint;

pub use code::{Code, WtCode};

#[cfg(test)]
fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
        .collect()
}
