use graphite_meter_server::password::Hash;
use std::{
    io::Write,
    process::{Command, Stdio},
};

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
fn piped_password_command_emits_compatible_hash_without_echoing_secret() {
    let result = command("correct horse battery staple\ncorrect horse battery staple\n");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let encoded = String::from_utf8(result.stdout).expect("ASCII PHC hash");
    assert!(
        Hash::parse(encoded.trim())
            .expect("valid PHC hash")
            .verify("correct horse battery staple")
    );
    assert!(!String::from_utf8_lossy(&result.stderr).contains("correct horse battery staple"));
}

#[test]
fn mismatched_passwords_fail_without_emitting_a_hash() {
    let result = command("one\ntwo\n");
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("passwords do not match"));
}
