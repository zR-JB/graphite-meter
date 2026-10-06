//! The PKCE challenge of a sign-in approval and the comparison code both pages show (`api/discovery.md`).

use base64::{
    Engine as _, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig, general_purpose::URL_SAFE_NO_PAD},
};
use ring::digest::{SHA256, digest};

/// Unpadded base64url, decoded as Go's `RawURLEncoding` decodes it.
const RAW_URL: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::RequireNone)
        .with_decode_allow_trailing_bits(true),
);

/// The S256 challenge of `verifier`: its SHA-256 digest in unpadded base64url.
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest(&SHA256, verifier.as_bytes()))
}

/// The digest's first five bytes in unpadded RFC 4648 base32; `None` for a challenge that is no S256 digest.
pub fn verification_code(challenge: &str) -> Option<String> {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut digest = [0; 32];
    if challenge.len() > 64 || RAW_URL.decode_slice(challenge, &mut digest).ok()? != digest.len() {
        return None;
    }
    let bits = digest[..5].iter().fold(0, |bits, &byte| bits << 8 | u64::from(byte));
    let code = (0..8)
        .rev()
        .map(|group| char::from(ALPHABET[(bits >> (5 * group) & 31) as usize]));
    Some(code.collect())
}
