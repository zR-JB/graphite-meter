//! The operator's Argon2id password hash at Go's fixed cost.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};

const MEMORY_KIB: u32 = 19 * 1024;
const PASSES: u32 = 2;
const LANES: u32 = 1;
const KEY_BYTES: usize = 32;

/// Go's password rules: 1 to 1024 bytes without line breaks, not necessarily UTF-8.
pub fn validate(password: &[u8]) -> Result<(), &'static str> {
    if password.is_empty() || password.len() > 1024 {
        return Err("password must contain 1 to 1024 bytes");
    }
    if password.contains(&b'\r') || password.contains(&b'\n') {
        return Err("password must not contain line breaks");
    }
    Ok(())
}

/// The PHC string of `password` under a fresh 16-byte salt.
pub fn hash(password: &[u8]) -> Result<String, &'static str> {
    validate(password)?;
    let mut salt = [0; 16];
    getrandom::fill(&mut salt).map_err(|_| "password salt unavailable")?;
    let key = derive(password, &salt)?;
    let (salt, key) = (STANDARD_NO_PAD.encode(salt), STANDARD_NO_PAD.encode(key));
    Ok(format!("$argon2id$v=19$m={MEMORY_KIB},t={PASSES},p={LANES}${salt}${key}"))
}

fn derive(password: &[u8], salt: &[u8]) -> Result<[u8; KEY_BYTES], &'static str> {
    let params = Params::new(MEMORY_KIB, PASSES, LANES, Some(KEY_BYTES)).map_err(|_| "invalid Argon2 parameters")?;
    let mut key = [0; KEY_BYTES];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password, salt, &mut key)
        .map_err(|_| "password hashing failed")?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(phc: &str) -> (Vec<u8>, Vec<u8>) {
        let fields: Vec<_> = phc.split('$').collect();
        assert_eq!(fields[..4], ["", "argon2id", "v=19", "m=19456,t=2,p=1"]);
        (STANDARD_NO_PAD.decode(fields[4]).unwrap(), STANDARD_NO_PAD.decode(fields[5]).unwrap())
    }

    #[test]
    fn hashes_match_the_go_servers() {
        let go = "$argon2id$v=19$m=19456,t=2,p=1$OT2po7nOdP+21BKX5CuZQw$9kVgfSWvlFy31939zUCVY62fHIuSqC8RwL67EpQ8qy8";
        let (salt, expected) = key(go);
        assert_eq!(derive(b"correct horse", &salt).unwrap().to_vec(), expected);
        let phc = hash(b"correct horse").unwrap();
        let (salt, expected) = key(&phc);
        assert_eq!((salt.len(), derive(b"correct horse", &salt).unwrap().to_vec()), (16, expected));
        assert_ne!(hash(b"correct horse").unwrap(), phc, "every hash has its own salt");
        assert_eq!(hash(b""), Err("password must contain 1 to 1024 bytes"));
        assert_eq!(hash(&[b'x'; 1025]), Err("password must contain 1 to 1024 bytes"));
        assert_eq!(hash(b"a\nb"), Err("password must not contain line breaks"));
        assert!(hash(&[0xff, 0xfe]).is_ok(), "bytes need not be UTF-8");
    }
}
