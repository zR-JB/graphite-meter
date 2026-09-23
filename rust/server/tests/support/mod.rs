use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Disposable identity: test keys are generated locally, never shipped in source.
pub struct Identity(PathBuf);

impl Identity {
    pub fn generate() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut nonce = [0_u8; 16];
        crypto_provider()
            .secure_random
            .fill(&mut nonce)
            .map_err(|_| "test identity randomness unavailable")?;
        let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = std::env::temp_dir().join(format!("graphite-meter-rust-test-{suffix}"));
        fs::create_dir(&path)?;
        let identity = Self(path);
        let output = Command::new("openssl")
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
            ])
            .arg(identity.0.join("identity.key"))
            .arg("-out")
            .arg(identity.0.join("identity.pem"))
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "test identity generation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(identity)
    }

    pub fn directory(&self) -> &Path {
        &self.0
    }
}

#[cfg(feature = "crypto-aws-lc")]
fn crypto_provider() -> rustls::crypto::CryptoProvider {
    rustls::crypto::aws_lc_rs::default_provider()
}

#[cfg(not(feature = "crypto-aws-lc"))]
fn crypto_provider() -> rustls::crypto::CryptoProvider {
    rustls::crypto::ring::default_provider()
}

impl Drop for Identity {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
