//! HTTP/3 and WebTransport over the noq QUIC fork, both roles: sans-IO protocol modules and a driver per connection.
mod budget;
mod capsule;
pub mod client;
mod code;
mod driver;
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

pub use {
    budget::Budget,
    code::Code,
    driver::{CONNECTION_BYTES, CONNECTION_IDLE},
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
    pub fn frame(kind: impl crate::frame::Kind, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        crate::frame::put_header(kind, payload.len() as u64, &mut bytes);
        [bytes, payload.to_vec()].concat()
    }

    pub fn section(fields: &[(&str, &str)]) -> Vec<u8> {
        let mut section = Vec::new();
        let fields = fields
            .iter()
            .map(|(name, value)| (name.as_bytes(), value.as_bytes()));
        crate::qpack::encode(fields, &mut section);
        section
    }
}

#[cfg(test)]
mod tests {
    use crate::{capsule, driver::Role, frame, message::Message, qpack, settings};
    use bytes::Bytes;

    /// Every input of up to two bytes, alone and after a section prefix, ends each parser without a panic.
    #[test]
    fn parsers_survive_every_short_input() {
        let short = std::iter::once(Vec::new())
            .chain((0..=255).map(|byte| vec![byte]))
            .chain((0..=u16::MAX).map(|pair| pair.to_be_bytes().to_vec()));
        for input in short {
            let _ = qpack::decode(&input, |_, _| Ok(()));
            let _ = qpack::decode(&[&[0, 0][..], &input].concat(), |_, _| Ok(()));
            let (mut frames, mut bytes) = (
                frame::Reader::<frame::FrameType>::default(),
                Bytes::from(input.clone()),
            );
            while frames.next(&mut bytes).is_some() {}
            let _ = capsule::Reader::default().read(&mut Bytes::from(input.clone()));
            let _ = settings::Reader::new(input.len() as u64)
                .map(|mut reader| reader.read(&mut &input[..]));
            for peer in [Role::Client, Role::Server] {
                let mut message = Message::new(4096, peer);
                let mut bytes = Bytes::from(input.clone());
                while let Ok(Some(_)) = message.next(&mut bytes) {}
                let _ = message.finish();
            }
        }
    }
}
