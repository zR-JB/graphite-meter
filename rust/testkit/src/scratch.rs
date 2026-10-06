//! Temporary directories for tests that need files.
use std::{
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

/// A private directory under the system's temporary directory, removed on drop.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new() -> io::Result<Self> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let name = format!("graphite-meter-scratch-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        // A temporary directory that climbs with `..` is refused, so scratch files stay below it.
        let base = std::env::temp_dir().to_string_lossy().into_owned();
        if base.contains("..") {
            return Err(io::Error::other(format!("the temporary directory {base} must not contain ..")));
        }
        let path = Path::new(&base).join(name);
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
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
