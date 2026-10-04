//! QUIC variable-length integers (RFC 9000 §16).
use bytes::BufMut;

pub(crate) const MAX: u64 = (1 << 62) - 1;

/// Length of the encoding that starts with `first`.
pub(crate) fn size(first: u8) -> usize {
    1 << (first >> 6)
}

/// Decodes the integer at the start of `input`, or `None` when it is incomplete.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc9000_sample_encodings() {
        for (hex, value) in [
            ("c2197c5eff14e88c", 151_288_809_941_952_652),
            ("9d7f3e7d", 494_878_333),
            ("7bbd", 15_293),
            ("25", 37),
        ] {
            let bytes = crate::hex(hex);
            assert_eq!(decode(&bytes), Some((value, bytes.len())));
            let mut encoded = Vec::new();
            put(value, &mut encoded);
            assert_eq!(encoded, bytes);
        }
        assert_eq!(decode(&[0x40, 0x25]), Some((37, 2)));
        assert_eq!(decode(&[0x9d, 0x7f, 0x3e]), None);
        assert_eq!(decode(&[]), None);
    }
}
