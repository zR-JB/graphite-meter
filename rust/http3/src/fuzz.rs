//! Fuzz target bodies; `cargo test` replays every committed corpus input through them.
use crate::{fields, frame, qpack, settings};
use bytes::Bytes;

/// Frames read in arbitrary chunks equal the frames read in one piece.
pub fn frames(data: &[u8]) {
    let Some((&seed, data)) = data.split_first() else {
        return;
    };
    let read = |chunk: &dyn Fn(usize) -> usize| {
        let mut reader = frame::Reader::default();
        let mut frames: Vec<(u64, u64, Vec<u8>)> = Vec::new();
        for mut input in chunks(data, chunk) {
            while let Some(piece) = reader.next(&mut input) {
                match piece {
                    frame::Piece::Header { kind, length } => frames.push((kind, length, Vec::new())),
                    frame::Piece::Payload(payload) => {
                        let (_, length, received) = frames.last_mut().expect("payload follows a header");
                        received.extend_from_slice(&payload);
                        assert!(received.len() as u64 <= *length);
                    }
                }
            }
            assert!(input.is_empty());
        }
        (frames, reader.at_boundary())
    };
    assert_eq!(
        read(&|_| data.len()),
        read(&|index| (index * usize::from(seed)) % 23 + 1)
    );
}

/// SETTINGS read in arbitrary chunks give the same peer or error; the dialect decision is total.
pub fn settings(data: &[u8]) {
    let Some((&seed, payload)) = data.split_first() else {
        return;
    };
    let read = |chunk: &dyn Fn(usize) -> usize| {
        let mut reader = settings::Reader::new(payload.len() as u64)?;
        let mut peer = reader.read(&mut &payload[..0])?;
        for input in chunks(payload, chunk) {
            peer = reader.read(&mut &input[..])?;
        }
        Ok(peer.expect("the whole payload was read"))
    };
    let whole: Result<settings::Peer, crate::Code> = read(&|_| payload.len());
    assert_eq!(whole, read(&|index| (index * usize::from(seed)) % 7 + 1));
    if let Ok(peer) = whole {
        assert_eq!(peer.webtransport(false), None);
        let _ = peer.webtransport(true);
    }
}

/// QPACK decoding stays within the field limit, and re-encoding decoded lines decodes to the same lines.
pub fn qpack(data: &[u8]) {
    let lines = |section: &[u8]| {
        let mut lines = Vec::new();
        qpack::decode(section, |name, value| {
            lines.push((name.to_vec(), value.to_vec()));
            Ok(())
        })
        .map(|()| lines)
    };
    if let Ok(decoded) = lines(data) {
        let mut encoded = Vec::new();
        qpack::encode(
            decoded.iter().map(|(name, value)| (&name[..], &value[..])),
            &mut encoded,
        );
        assert_eq!(lines(&encoded), Ok(decoded));
    }
    for limit in [0, 128, 4096] {
        if let Ok(head) = fields::decode_request(data, limit) {
            assert!(head.size <= limit);
        }
        if let Ok(head) = fields::decode_response(data, limit) {
            assert!(head.size <= limit);
        }
        let _ = fields::check_trailers(data, limit);
    }
}

/// Huffman encoding round-trips, and a valid encoding is the unique encoding of what it decodes to.
pub fn huffman(data: &[u8]) {
    let mut encoded = Vec::new();
    qpack::huffman::encode(data, &mut encoded);
    assert_eq!(encoded.len(), qpack::huffman::encoded_len(data));
    let mut decoded = Vec::new();
    qpack::huffman::decode(&encoded, &mut decoded).expect("our encoding decodes");
    assert_eq!(decoded, data);
    decoded.clear();
    if qpack::huffman::decode(data, &mut decoded).is_ok() {
        encoded.clear();
        qpack::huffman::encode(&decoded, &mut encoded);
        assert_eq!(encoded, data);
    }
}

/// Splits `data` into chunks whose lengths `chunk` picks by index; each at least one byte.
fn chunks<'a>(data: &'a [u8], chunk: &'a dyn Fn(usize) -> usize) -> impl Iterator<Item = Bytes> + 'a {
    let mut rest = Bytes::copy_from_slice(data);
    (0..).map_while(move |index| (!rest.is_empty()).then(|| rest.split_to(chunk(index).clamp(1, rest.len()))))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    #[test]
    fn corpora_replay() {
        replay("frame", super::frames);
        replay("settings", super::settings);
        replay("qpack", super::qpack);
        replay("huffman", super::huffman);
    }

    fn replay(target: &str, body: fn(&[u8])) {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/corpus").join(target);
        let entries = fs::read_dir(&directory).unwrap_or_else(|error| panic!("{directory:?}: {error}"));
        let mut replayed = 0;
        for entry in entries {
            body(&fs::read(entry.unwrap().path()).unwrap());
            replayed += 1;
        }
        assert!(replayed > 0, "{target} has no corpus");
    }
}
