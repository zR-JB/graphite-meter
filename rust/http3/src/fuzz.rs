//! Fuzz target bodies; `cargo test` replays every committed corpus input through them.
use crate::frame;
use bytes::Bytes;

/// Frames read in arbitrary chunks equal the frames read in one piece.
pub fn frames(data: &[u8]) {
    let Some((&seed, data)) = data.split_first() else {
        return;
    };
    let whole = read_frames(data, |_| data.len());
    let chunked = read_frames(data, |index| (index * usize::from(seed)) % 23 + 1);
    assert_eq!(whole, chunked);
}

type Frames = (Vec<(u64, u64, Vec<u8>)>, bool);

fn read_frames(data: &[u8], chunk: impl Fn(usize) -> usize) -> Frames {
    let mut reader = frame::Reader::default();
    let mut frames: Vec<(u64, u64, Vec<u8>)> = Vec::new();
    let mut rest = Bytes::copy_from_slice(data);
    for index in 0.. {
        if rest.is_empty() {
            break;
        }
        let mut input = rest.split_to(chunk(index).min(rest.len()));
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
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    #[test]
    fn corpora_replay() {
        replay("frame", super::frames);
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
