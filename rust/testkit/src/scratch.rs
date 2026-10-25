//! Temporary directories for tests that need files.
use std::{
    hash::{BuildHasher, RandomState},
    io,
    path::{Path, PathBuf},
};

/// A private directory under the system's temporary directory, removed on drop.
pub struct Scratch(PathBuf);

impl Scratch {
    /// Created exclusively under a random name, so no other process's directory is ever taken over.
    pub fn new() -> io::Result<Self> {
        // A temporary directory that climbs with `..` is refused, so scratch files stay below it.
        let base = std::env::temp_dir().to_string_lossy().into_owned();
        if base.contains("..") {
            return Err(io::Error::other(format!(
                "the temporary directory {base} must not contain .."
            )));
        }
        loop {
            let suffix = RandomState::new().hash_one(std::process::id());
            let path = Path::new(&base).join(format!("graphite-meter-scratch-{suffix:016x}"));
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

    /// `name` below the scratch directory; a name that climbs out with `..` is refused.
    fn inside(&self, name: &str) -> io::Result<PathBuf> {
        if name.contains("..") {
            return Err(io::Error::other(format!(
                "scratch name {name} must not contain .."
            )));
        }
        Ok(self.0.join(name))
    }

    pub fn file(&self, name: &str, contents: &str) -> io::Result<PathBuf> {
        let path = self.inside(name)?;
        std::fs::create_dir_all(path.parent().unwrap_or(&self.0))?;
        std::fs::write(&path, contents)?;
        Ok(path)
    }

    /// `file` with the Unix permission bits `mode`, whatever the umask.
    #[cfg(unix)]
    pub fn file_with_mode(&self, name: &str, contents: &str, mode: u32) -> io::Result<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let path = self.inside(name)?;
        std::fs::create_dir_all(path.parent().unwrap_or(&self.0))?;
        std::fs::write(&path, contents)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
        Ok(path)
    }

    pub fn dir(&self, name: &str) -> io::Result<PathBuf> {
        let path = self.inside(name)?;
        std::fs::create_dir_all(&path)?;
        Ok(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
