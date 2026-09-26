/// Generates a disposable localhost P-256 identity as `(certificate PEM, key PEM)`.
/// Test keys are generated locally, never shipped in source.
pub fn generate_identity() -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
    // Both PEM blocks go to stdout, key first; nothing touches the filesystem.
    let output = std::process::Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=DNS:localhost",
            "-addext",
            "basicConstraints=critical,CA:FALSE",
            "-keyout",
            "/dev/stdout",
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "test identity generation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let pem = String::from_utf8(output.stdout)?;
    let start = pem
        .find("-----BEGIN CERTIFICATE-----")
        .ok_or("test identity has no certificate")?;
    let (key, certificate) = pem.split_at(start);
    Ok((certificate.to_owned(), key.to_owned()))
}
