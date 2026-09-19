use graphite_meter_server::password::{Hash, hash_password};

// Generated independently by Go x/crypto/argon2.IDKey with salt "0123456789abcdef".
const GO_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$gy5SuVm5Z7Vw7keB9se9p87QGcomaseB/S2U1OhTsM0";
const PASSWORD: &str = "correct horse battery staple";

#[test]
fn verifies_go_hash() {
    let hash = Hash::parse(GO_HASH).unwrap();
    assert!(hash.verify(PASSWORD));
    assert!(!hash.verify("wrong"));
}

#[test]
fn rejects_malformed_or_out_of_policy_hashes() {
    for encoded in [
        String::new(),
        GO_HASH.replace("argon2id", "argon2i"),
        GO_HASH.replace("v=19", "v=16"),
        GO_HASH.replace("m=19456", "m=4096"),
        GO_HASH.replace("m=19456", "m=4294967296"),
        GO_HASH.replace("m=19456", "m=+19456"),
        GO_HASH.replace("t=2", "t=1"),
        GO_HASH.replace("p=1", "p=2"),
        GO_HASH.replace("m=19456,t=2", "t=2,m=19456"),
        GO_HASH.replace("p=1", "p=1,x=1"),
        GO_HASH.replace("Zg$", "Zg=$"),
        GO_HASH.replace("Zg$", "Z$"),
        GO_HASH.replace("MDEy", "MD y"),
        GO_HASH.replace("MDEy", "!!!!"),
        format!("{GO_HASH}="),
        format!("{GO_HASH}$"),
    ] {
        assert!(Hash::parse(&encoded).is_err(), "accepted {encoded}");
    }
}

#[test]
fn retains_go_parser_permissiveness() {
    for encoded in [
        format!("\u{85} \t{GO_HASH}\r\n"),
        format!("prefix{GO_HASH}"),
        GO_HASH.replace("m=19456,t=2,p=1", "m=019456,t=02,p=01"),
        GO_HASH.replace("MDEy", "MD\r\nEy"),
        GO_HASH.replace("Zg$", "Zh$"),
        GO_HASH.replace("TsM0", "TsM1"),
    ] {
        assert!(Hash::parse(&encoded).unwrap().verify(PASSWORD));
    }
}

#[test]
fn round_trip_preserves_password_bytes_and_uses_fresh_salt() {
    let password = " !@#$%^&*()_+-=[]{}|;:',.<>/?~ tabs\tand unicode ü🔐\0 ";
    let first = hash_password(password).unwrap();
    let second = hash_password(password).unwrap();
    assert_ne!(first, second);
    assert!(first.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    assert!(Hash::parse(&first).unwrap().verify(password));
    assert!(!Hash::parse(&first).unwrap().verify(password.trim()));
    let maximum = "ü".repeat(512);
    assert!(
        Hash::parse(&hash_password(&maximum).unwrap())
            .unwrap()
            .verify(&maximum)
    );
}

#[test]
fn rejects_invalid_passwords_before_hashing() {
    let hash = Hash::parse(GO_HASH).unwrap();
    for password in [
        String::new(),
        "a".repeat(1025),
        "ü".repeat(513),
        "line\nbreak".into(),
        "line\rbreak".into(),
    ] {
        assert!(hash_password(&password).is_err());
        assert!(!hash.verify(&password));
    }
}
