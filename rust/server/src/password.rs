//! Fixed-cost Argon2id password hashes compatible with the Go server.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::{
    Engine, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use subtle::ConstantTimeEq;

const MEMORY: u32 = 19 * 1024;
const TIME: u32 = 2;
const THREADS: u32 = 1;
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::RequireNone)
        .with_decode_allow_trailing_bits(true),
);

/// A validated hash; deliberately does not implement Debug to avoid secret logging.
#[derive(Clone)]
pub struct Hash {
    salt: [u8; 16],
    expected: [u8; 32],
}

impl Hash {
    /// Validate the complete format and fixed cost before any expensive hashing.
    pub fn parse(encoded: &str) -> Result<Self, &'static str> {
        let parts: Vec<_> = encoded.trim().split('$').collect();
        if parts.len() != 6 || parts[1] != "argon2id" || parts[2] != "v=19" {
            return Err("password hash must be an Argon2id v=19 PHC string");
        }
        let params: Vec<_> = parts[3].split(',').collect();
        if params.len() != 3 {
            return Err("password hash has invalid Argon2 parameters");
        }
        for (param, (prefix, expected)) in
            params
                .iter()
                .zip([("m=", MEMORY), ("t=", TIME), ("p=", THREADS)])
        {
            let value = param
                .strip_prefix(prefix)
                .ok_or("password hash has invalid Argon2 parameters")?;
            if value.is_empty()
                || !value.bytes().all(|byte| byte.is_ascii_digit())
                || value.parse::<u32>().ok() != Some(expected)
            {
                return Err("password hash must use m=19456,t=2,p=1");
            }
        }
        Ok(Self {
            salt: decode(parts[4]).ok_or("password hash must use a 16-byte salt")?,
            expected: decode(parts[5]).ok_or("password hash must use a 32-byte output")?,
        })
    }

    pub fn verify(&self, password: &str) -> bool {
        if validate_password(password).is_err() {
            return false;
        }
        match derive(password, &self.salt) {
            Ok(actual) => bool::from(actual.ct_eq(&self.expected)),
            Err(_) => false,
        }
    }
}

pub fn hash_password(password: &str) -> Result<String, &'static str> {
    validate_password(password)?;
    let mut salt = [0; 16];
    rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut salt)
        .map_err(|_| "failed to generate password salt")?;
    let key = derive(password, &salt)?;
    Ok(format!(
        "$argon2id$v=19$m={MEMORY},t={TIME},p={THREADS}${}${}",
        BASE64.encode(salt),
        BASE64.encode(key)
    ))
}

pub(crate) fn validate_password(password: &str) -> Result<(), &'static str> {
    if password.is_empty() || password.len() > 1024 {
        return Err("password must contain 1 to 1024 bytes");
    }
    if password.contains(['\r', '\n']) {
        return Err("password must not contain line breaks");
    }
    Ok(())
}

fn derive(password: &str, salt: &[u8; 16]) -> Result<[u8; 32], &'static str> {
    let params = Params::new(MEMORY, TIME, THREADS, Some(32))
        .map_err(|_| "invalid password hashing parameters")?;
    let mut key = [0; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|_| "password hashing failed")?;
    Ok(key)
}

fn decode<const N: usize>(encoded: &str) -> Option<[u8; N]> {
    // Go's RawStdEncoding ignores CR/LF and permits noncanonical trailing bits.
    let encoded: Vec<_> = encoded
        .bytes()
        .filter(|byte| !matches!(byte, b'\r' | b'\n'))
        .collect();
    BASE64.decode(encoded).ok()?.try_into().ok()
}
