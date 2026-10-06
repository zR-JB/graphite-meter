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
mod testing {
    pub fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
            .collect()
    }

    /// A frame of `kind`; capsules are laid out the same way.
    pub fn frame(kind: u64, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        crate::frame::put_header(kind, payload.len() as u64, &mut bytes);
        [bytes, payload.to_vec()].concat()
    }

    /// `fields` as our QPACK encodes them.
    pub fn section(fields: &[(&str, &str)]) -> Vec<u8> {
        let mut section = Vec::new();
        let fields = fields.iter().map(|(name, value)| (name.as_bytes(), value.as_bytes()));
        crate::qpack::encode(fields, &mut section);
        section
    }
}
