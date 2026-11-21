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
                    &gunzip_single_member(&compressed[..offset], prefix_len)
                        .filter(|bytes| bytes.len() == prefix_len)
                        .expect("the build embeds a valid report prefix"),
                );
                out.extend_from_slice(
                    &gunzip_single_member(&compressed[offset..], browser_len)
                        .filter(|bytes| bytes.len() == browser_len)
                        .expect("the build embeds a valid browser notice"),
                );
            } else {
                out.extend_from_slice(
                    &gunzip_single_member(compressed, length)
                        .filter(|bytes| bytes.len() == length)
                        .expect("the build embeds a complete report"),
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
            gunzip_single_member(gz, browser_len)
                .filter(|out| out.len() == browser_len)
                .expect("the build embeds a valid browser notice")
        });
        Some(plain)
    }
}

/// Computes the IEEE 802.3 CRC-32 checksum used by RFC 1952 gzip trailers.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

/// Inflates a single RFC 1952 gzip member, checking reserved flags, header CRC16, exact stream end, CRC32, and ISIZE.
pub fn gunzip_single_member(gz: &[u8], limit: usize) -> Option<Vec<u8>> {
    use miniz_oxide::{
        DataFormat, MZFlush, MZStatus,
        inflate::stream::{InflateState, inflate},
    };

    if gz.len() < 18 || gz[0] != 0x1f || gz[1] != 0x8b || gz[2] != 8 {
        return None;
    }
    let flags = gz[3];
    if flags & 0xe0 != 0 {
        return None;
    }
    let mut pos = 10usize;
    if flags & 0x04 != 0 {
        let xlen = u16::from_le_bytes([*gz.get(pos)?, *gz.get(pos + 1)?]) as usize;
        pos = pos.checked_add(2)?.checked_add(xlen)?;
    }
    if flags & 0x08 != 0 {
        pos = gz.get(pos..)?.iter().position(|&b| b == 0)? + pos + 1;
    }
    if flags & 0x10 != 0 {
        pos = gz.get(pos..)?.iter().position(|&b| b == 0)? + pos + 1;
    }
    if flags & 0x02 != 0 {
        let header_crc = u16::from_le_bytes([*gz.get(pos)?, *gz.get(pos + 1)?]);
        if header_crc != (crc32(&gz[..pos]) as u16) {
            return None;
        }
        pos = pos.checked_add(2)?;
    }
    let end = gz.len().checked_sub(8)?;
    if pos > end {
        return None;
    }

    let deflate = &gz[pos..end];
    let mut state = InflateState::new_boxed(DataFormat::Raw);
    let mut out = Vec::new();
    let mut chunk = [0u8; 16 << 10];
    let mut in_pos = 0usize;
    loop {
        let res = inflate(&mut state, &deflate[in_pos..], &mut chunk, MZFlush::None);
        in_pos += res.bytes_consumed;
        if out.len().checked_add(res.bytes_written)? > limit {
            return None;
        }
        out.extend_from_slice(&chunk[..res.bytes_written]);
        match res.status {
            Ok(MZStatus::StreamEnd) => break,
            Ok(MZStatus::Ok) => {
                if res.bytes_consumed == 0 && res.bytes_written == 0 {
                    return None;
                }
            }
            Ok(MZStatus::NeedDict) | Err(_) => return None,
        }
    }
    if in_pos != deflate.len() {
        return None;
    }
    let expected_crc = u32::from_le_bytes(gz[end..end + 4].try_into().ok()?);
    let expected_isize = u32::from_le_bytes(gz[end + 4..end + 8].try_into().ok()?);
    if crc32(&out) != expected_crc || (out.len() as u32) != expected_isize {
        return None;
    }
    Some(out)
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

    #[test]
    fn gunzip_single_member_validates_flags_crc_isize_limit_and_trailing_bytes() {
        let plain = b"graphite-meter rfc1952 single member check";
        let gz = build::gzip_member(plain);
        assert_eq!(
            gunzip_single_member(&gz, plain.len()).as_deref(),
            Some(&plain[..])
        );
        assert!(gunzip_single_member(&gz, plain.len() - 1).is_none());

        let mut bad_flags = gz.clone();
        bad_flags[3] |= 0x20;
        assert!(gunzip_single_member(&bad_flags, 1024).is_none());

        let mut bad_crc = gz.clone();
        let crc_pos = bad_crc.len() - 8;
        bad_crc[crc_pos] ^= 1;
        assert!(gunzip_single_member(&bad_crc, 1024).is_none());

        let mut bad_isize = gz.clone();
        let isize_pos = bad_isize.len() - 4;
        bad_isize[isize_pos] ^= 1;
        assert!(gunzip_single_member(&bad_isize, 1024).is_none());

        let mut multi = gz.clone();
        multi.extend_from_slice(&gz);
        assert!(gunzip_single_member(&multi, 1024).is_none());
    }
}
