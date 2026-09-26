use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use graphite_meter_server::password::{Hash, hash_password};
use std::{
    io::Write,
    process::{Command, Stdio},
};

/// Generated independently by Go x/crypto/argon2.IDKey with salt "0123456789abcdef":
/// the fixture holds the password, then its PHC hash.
fn go_vector() -> (String, String) {
    let fixture = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/go-argon2id.txt"
    ))
    .expect("Go Argon2id vector");
    let (password, hash) = fixture.trim_end().split_once('\n').expect("two lines");
    (password.into(), hash.into())
}

fn random_password() -> String {
    let mut bytes = [0; 18];
    getrandom::fill(&mut bytes).expect("test randomness");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn command(input: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_graphite-meter-server"))
        .arg("hash-password")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start Rust server password command");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input.as_bytes())
        .expect("supply password twice");
    child.wait_with_output().expect("password command result")
}

#[test]
fn verifies_go_hash() {
    let (password, encoded) = go_vector();
    let hash = Hash::parse(&encoded).unwrap();
    assert!(hash.verify(&password));
    assert!(!hash.verify(&password.to_uppercase()));
    assert!(!hash.verify(&random_password()));
}

#[test]
fn rejects_malformed_or_out_of_policy_hashes() {
    let (_, hash) = go_vector();
    for encoded in [
        String::new(),
        hash.replace("argon2id", "argon2i"),
        hash.replace("v=19", "v=16"),
        hash.replace("m=19456", "m=4096"),
        hash.replace("m=19456", "m=4294967296"),
        hash.replace("m=19456", "m=+19456"),
        hash.replace("t=2", "t=1"),
        hash.replace("p=1", "p=2"),
        hash.replace("m=19456,t=2", "t=2,m=19456"),
        hash.replace("p=1", "p=1,x=1"),
        hash.replace("Zg$", "Zg=$"),
        hash.replace("Zg$", "Z$"),
        hash.replace("MDEy", "MD y"),
        hash.replace("MDEy", "!!!!"),
        format!("{hash}="),
        format!("{hash}$"),
    ] {
        assert!(Hash::parse(&encoded).is_err(), "accepted {encoded}");
    }
}

#[test]
fn retains_go_parser_permissiveness() {
    let (password, hash) = go_vector();
    for encoded in [
        format!("\u{85} \t{hash}\r\n"),
        format!("prefix{hash}"),
        hash.replace("m=19456,t=2,p=1", "m=019456,t=02,p=01"),
        hash.replace("MDEy", "MD\r\nEy"),
        hash.replace("Zg$", "Zh$"),
        hash.replace("TsM0", "TsM1"),
    ] {
        assert!(Hash::parse(&encoded).unwrap().verify(&password));
    }
}

#[test]
fn round_trip_preserves_password_bytes_and_uses_fresh_salt() {
    // Unusual bytes, including surrounding spaces, around a fresh random core.
    let password = String::from(" !@#$%^&*()_+-=[]{}|;:',.<>/?~ tabs\tand unicode ü🔐\0 ")
        + &random_password()
        + " ";
    let first = hash_password(&password).unwrap();
    let second = hash_password(&password).unwrap();
    assert_ne!(first, second);
    assert!(first.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    assert!(Hash::parse(&first).unwrap().verify(&password));
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
    let (_, encoded) = go_vector();
    let hash = Hash::parse(&encoded).unwrap();
    for password in [
        String::new(),
        "a".repeat(1025),
        "ü".repeat(513),
        random_password() + "\n" + &random_password(),
        random_password() + "\r" + &random_password(),
    ] {
        assert!(hash_password(&password).is_err());
        assert!(!hash.verify(&password));
    }
}

#[test]
fn piped_password_command_emits_compatible_hash_without_echoing_secret() {
    let password = random_password();
    let result = command(&format!("{password}\n{password}\n"));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let encoded = String::from_utf8(result.stdout).expect("ASCII PHC hash");
    assert!(
        Hash::parse(encoded.trim())
            .expect("valid PHC hash")
            .verify(&password)
    );
    assert!(!String::from_utf8_lossy(&result.stderr).contains(&password));
}

#[test]
fn mismatched_passwords_fail_without_emitting_a_hash() {
    let result = command("one\ntwo\n");
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("passwords do not match"));
}
