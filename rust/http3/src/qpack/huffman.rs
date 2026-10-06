//! The HPACK Huffman code (RFC 7541 Appendix B). It is canonical, so code lengths define it.
use super::Corrupt;

/// Code length per symbol, 16 symbols per row; the last is EOS.
#[rustfmt::skip]
const LENGTHS: [u8; 257] = [
    13, 23, 28, 28, 28, 28, 28, 28, 28, 24, 30, 28, 28, 30, 28, 28,
    28, 28, 28, 28, 28, 28, 30, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    6, 10, 10, 12, 13, 6, 8, 11, 10, 10, 8, 11, 8, 6, 6, 6,
    5, 5, 5, 6, 6, 6, 6, 6, 6, 6, 7, 8, 15, 6, 12, 10,
    13, 6, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
    7, 7, 7, 7, 7, 7, 7, 7, 8, 7, 8, 13, 19, 13, 14, 6,
    15, 5, 6, 5, 6, 5, 6, 6, 6, 5, 7, 7, 6, 6, 6, 5,
    6, 7, 6, 5, 5, 6, 7, 7, 7, 7, 7, 15, 11, 14, 13, 28,
    20, 22, 20, 20, 22, 22, 22, 23, 22, 23, 23, 23, 23, 23, 24, 23,
    24, 24, 22, 23, 24, 23, 23, 23, 23, 21, 22, 23, 22, 23, 23, 24,
    22, 21, 20, 22, 22, 23, 23, 21, 23, 22, 22, 24, 21, 22, 23, 23,
    21, 21, 22, 21, 23, 22, 23, 23, 20, 22, 22, 22, 23, 22, 22, 23,
    26, 26, 20, 19, 22, 23, 22, 25, 26, 26, 26, 27, 27, 26, 24, 25,
    19, 21, 26, 27, 27, 26, 27, 24, 21, 21, 26, 26, 28, 27, 27, 27,
    20, 24, 20, 21, 22, 21, 21, 23, 22, 22, 25, 25, 24, 24, 26, 23,
    26, 27, 26, 26, 27, 27, 27, 27, 27, 28, 27, 27, 27, 27, 27, 26,
    30,
];
const EOS: u16 = 256;

struct Code {
    codes: [u32; 257],
    /// Per length: the first code, how many codes there are and where their symbols start.
    first: [u32; 31],
    count: [u32; 31],
    start: [usize; 31],
    symbols: [u16; 257],
}

const CODE: Code = canonical();

const fn canonical() -> Code {
    let mut count = [0; 31];
    let mut symbol = 0;
    while symbol < 257 {
        count[LENGTHS[symbol] as usize] += 1;
        symbol += 1;
    }
    let (mut first, mut start) = ([0; 31], [0; 31]);
    let (mut code, mut index, mut length) = (0, 0, 1);
    while length <= 30 {
        first[length] = code;
        start[length] = index;
        code = (code + count[length]) << 1;
        index += count[length] as usize;
        length += 1;
    }
    let (mut next, mut fill) = (first, start);
    let (mut codes, mut symbols) = ([0; 257], [0; 257]);
    symbol = 0;
    while symbol < 257 {
        let length = LENGTHS[symbol] as usize;
        codes[symbol] = next[length];
        next[length] += 1;
        symbols[fill[length]] = symbol as u16;
        fill[length] += 1;
        symbol += 1;
    }
    // A complete code, with EOS as the all-ones 30-bit code: every bit string decodes or pads.
    assert!(code == 1 << 31 && codes[EOS as usize] == (1 << 30) - 1);
    Code { codes, first, count, start, symbols }
}

pub(crate) fn encoded_len(input: &[u8]) -> usize {
    input
        .iter()
        .map(|&byte| usize::from(LENGTHS[usize::from(byte)]))
        .sum::<usize>()
        .div_ceil(8)
}

pub(crate) fn encode(input: &[u8], output: &mut Vec<u8>) {
    let (mut bits, mut pending) = (0_u64, 0);
    for &byte in input {
        let length = u32::from(LENGTHS[usize::from(byte)]);
        bits = bits << length | u64::from(CODE.codes[usize::from(byte)]);
        pending += length;
        while pending >= 8 {
            pending -= 8;
            output.push((bits >> pending) as u8);
        }
        bits &= (1 << pending) - 1;
    }
    if pending > 0 {
        // Pad with the most significant bits of EOS.
        output.push((bits << (8 - pending)) as u8 | 0xff >> pending);
    }
}

pub(crate) fn decode(input: &[u8], output: &mut Vec<u8>) -> Result<(), Corrupt> {
    let (mut code, mut length) = (0_u32, 0);
    for &byte in input {
        for shift in (0..8).rev() {
            code = code << 1 | u32::from(byte >> shift & 1);
            length += 1;
            let index = code.wrapping_sub(CODE.first[length]);
            if index < CODE.count[length] {
                let symbol = CODE.symbols[CODE.start[length] + index as usize];
                if symbol == EOS {
                    return Err(Corrupt);
                }
                output.push(symbol as u8);
                (code, length) = (0, 0);
            }
        }
    }
    // Padding is a prefix of EOS: at most seven one bits.
    if length > 7 || code != (1 << length) - 1 {
        return Err(Corrupt);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::hex;

    #[test]
    fn rfc7541_examples() {
        for (plain, coded) in [
            ("www.example.com", "f1e3c2e5f23a6ba0ab90f4ff"),
            ("no-cache", "a8eb10649cbf"),
            ("custom-key", "25a849e95ba97d7f"),
            ("custom-value", "25a849e95bb8e8b4bf"),
            ("302", "6402"),
            ("private", "aec3771a4b"),
            ("Mon, 21 Oct 2013 20:13:21 GMT", "d07abe941054d444a8200595040b8166e082a62d1bff"),
            ("https://www.example.com", "9d29ad171863c78f0b97c8e9ae82ae43d3"),
            ("gzip", "9bd9ab"),
            (
                "foo=ASDJKHQKBZXOQWEOPIUAXQWEOIU; max-age=3600; version=1",
                "94e7821dd7f2e6c7b335dfdfcd5b3960d5af27087f3672c1ab270fb5291f9587316065c003ed4ee5b1063d5007",
            ),
        ] {
            let mut encoded = Vec::new();
            encode(plain.as_bytes(), &mut encoded);
            assert_eq!(encoded, hex(coded), "{plain}");
            assert_eq!(encoded_len(plain.as_bytes()), encoded.len());
            let mut decoded = Vec::new();
            decode(&encoded, &mut decoded).unwrap();
            assert_eq!(decoded, plain.as_bytes());
        }
    }

    #[test]
    fn padding_and_eos_are_checked() {
        let mut output = Vec::new();
        for invalid in [&[0xff, 0xff, 0xff, 0xff][..], &[0x00], &[0xf8, 0x01], &[0xff, 0xfe]] {
            assert_eq!(decode(invalid, &mut output), Err(Corrupt), "{invalid:02x?}");
        }
        // "0" is the five-bit code 00000; three one bits of padding complete the byte.
        output.clear();
        assert_eq!(decode(&[0x07], &mut output), Ok(()));
        assert_eq!(output, b"0");
    }
}
