use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[path = "../../../test_identity.rs"]
mod test_identity;

/// Disposable identity files in Cargo's scratch directory for integration tests.
pub struct Identity(PathBuf);

impl Identity {
    pub fn generate() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let (certificate, key) = test_identity::generate_identity().expect("test identity");
        let identity = Self(Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "identity-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        fs::create_dir_all(&identity.0).expect("test identity directory");
        fs::write(identity.0.join("identity.pem"), certificate).expect("test certificate");
        fs::write(identity.0.join("identity.key"), key).expect("test key");
        identity
    }

    pub fn directory(&self) -> &Path {
        &self.0
    }
}

impl Drop for Identity {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
