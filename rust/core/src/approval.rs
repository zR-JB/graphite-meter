use base64::{
    Engine as _, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig, general_purpose::URL_SAFE_NO_PAD},
};
use ring::digest::{SHA256, digest};

const RAW_URL: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::RequireNone)
        .with_decode_allow_trailing_bits(true),
);

pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes()))
}

pub fn verification_code(challenge: &str) -> Option<String> {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    if challenge.len() > 64 {
        return None;
    }
    let normalized: Vec<_> = challenge.bytes().filter(|byte| !b"\r\n".contains(byte)).collect();
    let mut digest = [0; 32];
    if RAW_URL.decode_slice(normalized, &mut digest).ok()? != 32 {
        return None;
    }
    let bits = digest[..5].iter().fold(0, |bits, &byte| (bits << 8) | u64::from(byte));
    let digits = (0..8).rev().map(|i| (bits >> (5 * i)) & 31);
    Some(digits.map(|digit| ALPHABET[digit as usize] as char).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_code_is_base32_of_the_challenge_digest() {
        assert_eq!(verification_code(&challenge("verifier")).unwrap(), "RDE6VZUO");
    }
}
