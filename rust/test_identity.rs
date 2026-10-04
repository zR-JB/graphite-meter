type Error = Box<dyn std::error::Error + Send + Sync>;

/// Generates a disposable P-256 identity as `(certificate PEM, key PEM)`.
/// Test keys are generated locally, never shipped in source.
pub fn generate_identity(host: &str) -> Result<(String, String), Error> {
    assert!(matches!(host, "localhost" | "provider.test"));
    let pem = self_signed(host, "CA:FALSE")?;
    let key_end = pem
        .find("-----END ")
        .and_then(|end| pem[end..].find('\n').map(|line| end + line + 1))
        .ok_or("test identity has no key")?;
    let (key, certificate) = pem.split_at(key_end);
    Ok((certificate.to_owned(), key.to_owned()))
}

/// A self-signed P-256 certificate for `host` with these basic constraints, as `openssl req` writes it to stdout:
/// the key's PEM block first, then the certificate's; nothing touches the filesystem.
pub fn self_signed(host: &str, constraints: &str) -> Result<String, Error> {
    let arguments = format!(
        "req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 1 -subj /CN=localhost \
         -addext subjectAltName=DNS:{host} -addext basicConstraints=critical,{constraints} -keyout /dev/stdout"
    );
    let output = std::process::Command::new("openssl")
        .args(arguments.split_whitespace())
        .output()?;
    if !output.status.success() {
        return Err(format!("test identity generation failed: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}
