//! QUIC variable-length integers (RFC 9000 §16), whole or split across chunks.
use bytes::{Buf, BufMut};

pub(crate) const MAX: u64 = (1 << 62) - 1;

/// Length of the encoding whose first byte is `first`.
pub(crate) fn size(first: u8) -> usize {
    1 << (first >> 6)
}

/// The integer at the start of `input` and its length, or `None` when it is incomplete.
/// Non-minimal encodings are valid.
pub(crate) fn decode(input: &[u8]) -> Option<(u64, usize)> {
    let bytes = input.get(..size(*input.first()?))?;
    let value = bytes[1..]
        .iter()
        .fold(u64::from(bytes[0] & 0x3f), |value, byte| value << 8 | u64::from(*byte));
    Some((value, bytes.len()))
}

pub(crate) fn len(value: u64) -> usize {
    match value {
        0..64 => 1,
        64..16_384 => 2,
        16_384..1_073_741_824 => 4,
        _ => 8,
    }
}

/// Appends the minimal encoding of `value`, which must not exceed [`MAX`].
pub(crate) fn put(value: u64, output: &mut impl BufMut) {
    debug_assert!(value <= MAX);
    let size = len(value);
    let mut bytes = value.to_be_bytes();
    bytes[8 - size] |= (size.trailing_zeros() as u8) << 6;
    output.put_slice(&bytes[8 - size..]);
}

/// One varint, read from chunks split anywhere.
#[derive(Default)]
pub(crate) struct Partial {
    bytes: [u8; 8],
    used: u8,
}

impl Partial {
    pub(crate) fn read(&mut self, input: &mut impl Buf) -> Option<u64> {
        loop {
            let used = usize::from(self.used);
            let need = if used == 0 { 1 } else { size(self.bytes[0]) };
            if used == need {
                self.used = 0;
                return decode(&self.bytes[..used]).map(|(value, _)| value);
            }
            let take = (need - used).min(input.remaining());
            if take == 0 {
                return None;
            }
            input.copy_to_slice(&mut self.bytes[used..used + take]);
            self.used += take as u8;
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.used == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc9000_samples_decode_whole_and_in_parts() {
        for (hex, value) in [
            ("c2197c5eff14e88c", 151_288_809_941_952_652),
            ("9d7f3e7d", 494_878_333),
            ("7bbd", 15_293),
            ("25", 37),
        ] {
            let bytes = crate::testing::hex(hex);
            assert_eq!(decode(&bytes), Some((value, bytes.len())));
            let mut encoded = Vec::new();
            put(value, &mut encoded);
            assert_eq!(encoded, bytes);
            let mut partial = Partial::default();
            let (last, first) = bytes.split_last().unwrap();
            for byte in first {
                assert_eq!(partial.read(&mut &[*byte][..]), None);
            }
            assert_eq!(partial.read(&mut &[*last][..]), Some(value));
            assert!(partial.is_empty());
        }
        assert_eq!(decode(&[0x40, 0x25]), Some((37, 2)), "non-minimal");
        assert_eq!(decode(&[0x9d, 0x7f, 0x3e]), None);
        assert_eq!(decode(&[]), None);
    }
}
