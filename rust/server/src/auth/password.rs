//! The operator's password: Argon2id PHC hashes at one fixed cost, verification off the runtime, the attempt budgets
//! and the device cookies a sign-in leaves.

use super::rate::{Attempts, Ceiling};
use crate::peer::ClientKeys;
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{
    Engine as _, alphabet,
    engine::{
        DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig,
        general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD},
    },
};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

const MEMORY_KIB: u32 = 19 * 1024;
const PASSES: u32 = 2;
const LANES: u32 = 1;
const SALT_BYTES: usize = 16;
const KEY_BYTES: usize = 32;
const DEVICE_LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Unpadded standard base64 that tolerates nonzero trailing bits.
const RAW_STD: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::RequireNone)
        .with_decode_allow_trailing_bits(true),
);

/// The password rules: 1 to 1024 bytes without line breaks, not necessarily UTF-8.
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
    let mut salt = [0; SALT_BYTES];
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

/// A configured hash's salt and key.
#[derive(Clone, Copy)]
struct Hash {
    salt: [u8; SALT_BYTES],
    key: [u8; KEY_BYTES],
}

impl Hash {
    /// Checks a PHC string, with the messages operators see.
    fn parse(encoded: &str) -> Result<Self, String> {
        let fields: Vec<_> = encoded.trim().split('$').collect();
        if fields.len() != 6 || fields[1] != "argon2id" || fields[2] != "v=19" {
            return Err("password hash must be an Argon2id v=19 PHC string".into());
        }
        let params: Vec<_> = fields[3].split(',').collect();
        if params.len() != 3 {
            return Err("password hash has invalid Argon2 parameters".into());
        }
        for (param, (prefix, expected)) in params.iter().zip([("m=", MEMORY_KIB), ("t=", PASSES), ("p=", LANES)]) {
            let number = param
                .strip_prefix(prefix)
                .ok_or("password hash has invalid Argon2 parameters")?;
            if number.starts_with('+') || number.parse::<u32>().ok() != Some(expected) {
                return Err(format!("password hash must use m={MEMORY_KIB},t={PASSES},p={LANES}"));
            }
        }
        let decode = |text: &str, bytes: usize| RAW_STD.decode(text).ok().filter(|raw| raw.len() == bytes);
        let salt = decode(fields[4], SALT_BYTES).ok_or("password hash must use a 16-byte salt")?;
        let key = decode(fields[5], KEY_BYTES).ok_or("password hash must use a 32-byte output")?;
        Ok(Self {
            salt: salt.try_into().expect("the salt's length"),
            key: key.try_into().expect("the key's length"),
        })
    }

    fn verify(&self, password: &[u8]) -> bool {
        validate(password).is_ok() && derive(password, &self.salt).is_ok_and(|key| key.ct_eq(&self.key).into())
    }
}

/// Password sign-in: the hash, the key of device cookies, two verifier slots and the attempt budgets.
pub(super) struct Password {
    hash: Hash,
    device: Hmac<Sha256>,
    slots: Arc<Semaphore>,
    attempts: Attempts,
    wrong: Ceiling,
}

impl Password {
    /// The sign-in `encoded`, the configured hash, allows; device cookies are signed with it, so a new password
    /// forgets every device.
    pub fn new(encoded: &str) -> Result<Self, String> {
        Ok(Self {
            hash: Hash::parse(encoded)?,
            device: Hmac::new_from_slice(encoded.as_bytes()).expect("HMAC takes keys of any length"),
            slots: Arc::new(Semaphore::new(2)),
            attempts: Attempts::new("password-attempt", 5),
            wrong: Ceiling::new("password-attempt", 60),
        })
    }

    /// Records an attempt by `keys`, five a minute; a known `device` skips the 60 wrong passwords a minute all
    /// clients share.
    pub fn admit(&self, keys: Option<ClientKeys>, device: Option<&str>) -> bool {
        let known = device.is_some_and(|cookie| self.known(cookie));
        keys.is_some_and(|keys| self.attempts.allow(&keys, known, (!known).then_some(&self.wrong)))
    }

    /// Whether `password` matches, verified on a blocking thread that holds its slot; `None` while both slots are busy.
    pub async fn verify(&self, password: Zeroizing<String>) -> Option<bool> {
        let permit = self.slots.clone().try_acquire_owned().ok()?;
        let hash = self.hash;
        let verify = move || {
            let _permit = permit;
            hash.verify(password.as_bytes())
        };
        let verified = tokio::task::spawn_blocking(verify).await.ok()?;
        if !verified {
            self.wrong.note();
        }
        Some(verified)
    }

    /// A device cookie's value, its expiry in seconds and their HMAC, and that expiry.
    pub fn device(&self, now: SystemTime) -> (String, SystemTime) {
        let expires = now + DEVICE_LIFETIME;
        let seconds = expires
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_be_bytes();
        let tag = self.device.clone().chain_update(seconds).finalize().into_bytes();
        (URL_SAFE_NO_PAD.encode([&seconds[..], &tag[..]].concat()), expires)
    }

    fn known(&self, cookie: &str) -> bool {
        let mut raw = [0; 8 + 32];
        if URL_SAFE_NO_PAD.decode_slice(cookie, &mut raw).ok() != Some(raw.len()) {
            return false;
        }
        let (expires, tag) = raw.split_at(8);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let expires_at = u64::from_be_bytes(expires.try_into().expect("eight bytes"));
        now < expires_at && self.device.clone().chain_update(expires).verify_slice(tag).is_ok()
    }
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

    #[test]
    fn a_device_cookie_is_known_under_its_password_until_it_expires_and_passes_the_shared_ceiling() {
        let go = "$argon2id$v=19$m=19456,t=2,p=1$OT2po7nOdP+21BKX5CuZQw$9kVgfSWvlFy31939zUCVY62fHIuSqC8RwL67EpQ8qy8";
        let password = Password::new(go).unwrap();
        let (cookie, expires) = password.device(SystemTime::now());
        assert!(expires > SystemTime::now() + DEVICE_LIFETIME - Duration::from_secs(1));
        assert!(password.known(&cookie));
        assert!(
            !Password::new(&go.replace("9kVg", "8kVg")).unwrap().known(&cookie),
            "another password's device"
        );
        let (old, _) = password.device(SystemTime::now() - DEVICE_LIFETIME - Duration::from_secs(1));
        assert!(!password.known(&old));
        let forged = format!("{}A{}", &cookie[..10], &cookie[11..]);
        assert!(!password.known(&forged) || forged == cookie);
        let keys = || Some(ClientKeys::address("192.0.2.1".parse().unwrap()));
        (0..60).for_each(|_| password.wrong.note());
        assert!(!password.admit(keys(), None) && !password.admit(keys(), Some(&old)));
        assert!(password.admit(keys(), Some(&cookie)));
    }
}
