//! Embeds third-party notices. `embed` writes `legal.rs` at build time, defining `static NOTICES: Notices` for runtime.

#[cfg(feature = "build")]
mod build;

#[cfg(feature = "build")]
pub use build::{checkout, embed, inside, out_dir};

use std::{
    io::{self, Write},
    sync::OnceLock,
};

/// A server serves the browser's notice at this path from the report's suffix.
pub const BROWSER_NOTICE: &str = "legal/THIRD_PARTY_NOTICES.txt";

/// Holds the notices a build embedded.
pub struct Notices {
    /// Holds the zlib-compressed report and its length.
    payload: Option<(&'static [u8], usize)>,
    /// The browser's notice starts here in the report.
    browser: Option<usize>,
    /// The build's plain marker is a development build's, a reviewed build's with its report's SHA-256, or nothing.
    marker: &'static str,
    report: OnceLock<Vec<u8>>,
}

impl Notices {
    /// The generated `legal.rs` calls this.
    #[doc(hidden)]
    pub const fn new(payload: Option<(&'static [u8], usize)>, browser: Option<usize>, marker: &'static str) -> Self {
        Self { payload, browser, marker, report: OnceLock::new() }
    }

    /// Keeps the build's plain marker, which release verification reads, in the executable.
    pub fn keep(&self) {
        std::hint::black_box(self.marker);
    }

    /// Inflates the report on first use. A build without notices gives `None`.
    pub fn report(&self) -> Option<&[u8]> {
        let (compressed, length) = self.payload?;
        let report = self.report.get_or_init(|| {
            miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(compressed, length)
                .ok()
                .filter(|report| report.len() == length)
                .expect("the build embeds a complete report")
        });
        Some(report)
    }

    /// Returns the browser's notice in a server build that embeds the browser app.
    pub fn browser(&self) -> Option<&[u8]> {
        let start = self.browser?;
        Some(&self.report()?[start..])
    }
}

/// Writes `report` and returns the exit status, 141 into a closed pipe on Unix, as a process killed by SIGPIPE, else 0.
pub fn print(mut out: impl Write, report: &[u8]) -> io::Result<u8> {
    match out.write_all(report).and_then(|()| out.flush()) {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(if cfg!(unix) { 141 } else { 0 }),
        written => written.map(|()| 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printing_into_a_closed_pipe_ends_quietly() {
        let (reader, writer) = io::pipe().unwrap();
        drop(reader);
        let status = print(writer, &[b'x'; 1 << 16]).unwrap();
        assert_eq!(status, if cfg!(unix) { 141 } else { 0 });
        let mut out = Vec::new();
        assert_eq!(print(&mut out, b"notices").unwrap(), 0);
        assert_eq!(out, b"notices");
    }
}
