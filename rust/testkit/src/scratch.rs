//! Makes temporary directories for tests that need files.
use std::{
    hash::{BuildHasher, RandomState},
    io,
    path::{Path, PathBuf},
};

/// Owns a private directory under the system's temporary directory, removed on drop.
pub struct Scratch(PathBuf);

impl Scratch {
    /// Creates the directory exclusively under a random name, so no other process's directory is ever taken over.
    pub fn new() -> io::Result<Self> {
        loop {
            let suffix = RandomState::new().hash_one(std::process::id());
            let path = std::env::temp_dir().join(format!("graphite-meter-scratch-{suffix:016x}"));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Writes `contents` to `name`, creating its directories.
    pub fn file(&self, name: &str, contents: &str) -> io::Result<PathBuf> {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap_or(&self.0))?;
        std::fs::write(&path, contents)?;
        Ok(path)
    }

    pub fn dir(&self, name: &str) -> io::Result<PathBuf> {
        let path = self.0.join(name);
        std::fs::create_dir_all(&path)?;
        Ok(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
