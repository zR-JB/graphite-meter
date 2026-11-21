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

pub struct Notices {
    /// Concatenated RFC 1952 gzip member(s) for the full report and its uncompressed byte length.
    payload: Option<(&'static [u8], usize)>,
    /// Byte offset of the browser notice gzip member inside `payload` and its uncompressed byte length.
    browser: Option<(usize, usize)>,
    /// The plain marker: a development build's, a reviewed build's with its report's and profile's SHA-256s, or none.
    marker: &'static str,
    report: OnceLock<Vec<u8>>,
    browser_plain: OnceLock<Vec<u8>>,
}

impl Notices {
    /// The generated `legal.rs` calls this.
    #[doc(hidden)]
    pub const fn new(
        payload: Option<(&'static [u8], usize)>,
        browser: Option<(usize, usize)>,
        marker: &'static str,
    ) -> Self {
        Self {
            payload,
            browser,
            marker,
            report: OnceLock::new(),
            browser_plain: OnceLock::new(),
        }
    }

    /// Keeps the build's plain marker, which release verification reads, in the executable.
    pub fn keep(&self) {
        std::hint::black_box(self.marker);
    }

    /// Inflates the report on first use. A build without notices gives `None`.
    pub fn report(&self) -> Option<&[u8]> {
        let (compressed, length) = self.payload?;
        let report = self.report.get_or_init(|| {
            let mut out = Vec::with_capacity(length);
            if let Some((offset, browser_len)) = self.browser {
                let prefix_len = length.checked_sub(browser_len).expect("valid browser len");
                out.extend_from_slice(
                    &gunzip_member(&compressed[..offset], prefix_len)
                        .expect("the build embeds a valid report prefix"),
                );
                out.extend_from_slice(
                    &gunzip_member(&compressed[offset..], browser_len)
                        .expect("the build embeds a valid browser notice"),
                );
            } else {
                out.extend_from_slice(
                    &gunzip_member(compressed, length).expect("the build embeds a complete report"),
                );
            }
            assert_eq!(out.len(), length, "the build embeds a complete report");
            out
        });
        Some(report)
    }

    /// The precompressed gzip browser notice bytes, served directly without allocation.
    pub fn browser_gzip(&self) -> Option<&'static [u8]> {
        let (compressed, _) = self.payload?;
        let (offset, _) = self.browser?;
        Some(&compressed[offset..])
    }

    /// Inflates only the browser notice member on first use, avoiding allocating the full report.
    pub fn browser(&self) -> Option<&[u8]> {
        let gz = self.browser_gzip()?;
        let (_, browser_len) = self.browser?;
        let plain = self.browser_plain.get_or_init(|| {
            gunzip_member(gz, browser_len).expect("the build embeds a valid browser notice")
        });
        Some(plain)
    }
}

fn gunzip_member(gz: &[u8], expected_len: usize) -> Option<Vec<u8>> {
    if gz.len() < 18 || gz[0] != 0x1f || gz[1] != 0x8b || gz[2] != 8 {
        return None;
    }
    let flags = gz[3];
    let mut pos = 10usize;
    if flags & 0x04 != 0 {
        let xlen = u16::from_le_bytes([*gz.get(pos)?, *gz.get(pos + 1)?]) as usize;
        pos = pos.checked_add(2 + xlen)?;
    }
    if flags & 0x08 != 0 {
        pos = gz.get(pos..)?.iter().position(|&b| b == 0)? + pos + 1;
    }
    if flags & 0x10 != 0 {
        pos = gz.get(pos..)?.iter().position(|&b| b == 0)? + pos + 1;
    }
    if flags & 0x02 != 0 {
        pos = pos.checked_add(2)?;
    }
    let end = gz.len().checked_sub(8)?;
    if pos > end {
        return None;
    }
    let out =
        miniz_oxide::inflate::decompress_to_vec_with_limit(&gz[pos..end], expected_len).ok()?;
    (out.len() == expected_len).then_some(out)
}

/// Writes `report` and returns the exit status, 141 into a closed pipe on Unix, as a process killed by SIGPIPE, else 0.
pub fn print(mut out: impl Write, report: &[u8]) -> io::Result<u8> {
    match out.write_all(report).and_then(|()| out.flush()) {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
            Ok(if cfg!(unix) { 141 } else { 0 })
        }
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
