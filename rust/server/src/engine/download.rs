//! Serves download payloads from one random block that every download repeats.

use super::{Meter, Transfer};
use crate::limits::{Budget, Lease};
use bytes::Bytes;
#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd;
use std::sync::Arc;
use tokio::time::Instant;
use tokio_util::sync::{CancellationToken, DropGuard};

/// The block is this large, as is the largest chunk a source hands out.
pub const BLOCK_BYTES: usize = 256 << 10;

/// Sealed read-only memfd copy of the download block for zero-copy `sendfile` on plain TCP sockets.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub(crate) struct Sendfile {
    /// Retains the shared block allocation so `start..start + len` remains live as long as `Sendfile` exists.
    _backing: Bytes,
    start: usize,
    len: usize,
    file: Arc<OwnedFd>,
}

#[cfg(target_os = "linux")]
impl Sendfile {
    /// Returns the memfd and byte offset when `slice` points into the shared download block.
    pub fn locate(&self, slice: &[u8]) -> Option<(&OwnedFd, u64)> {
        if slice.is_empty() {
            return None;
        }
        let offset = (slice.as_ptr() as usize).checked_sub(self.start)?;
        (offset.checked_add(slice.len())? <= self.len).then_some((&self.file, offset as u64))
    }

    pub fn fd(&self) -> &OwnedFd {
        &self.file
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

#[cfg(target_os = "linux")]
fn create_memfd(bytes: &[u8]) -> Option<Arc<OwnedFd>> {
    use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, memfd_create};
    use std::{fs::File, io::Write};
    let fd = memfd_create(
        c"gm-download",
        MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
    )
    .ok()?;
    let mut file = File::from(fd);
    file.write_all(bytes).ok()?;
    let fd = OwnedFd::from(file);
    let _ = fcntl_add_seals(
        &fd,
        SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE | SealFlags::SEAL,
    );
    Some(Arc::new(fd))
}

/// The shared payload block is charged to the budget once for the server's lifetime.
#[derive(Debug)]
pub struct Block {
    bytes: Bytes,
    #[cfg(target_os = "linux")]
    sendfile: Option<Sendfile>,
    meter: Meter,
    _lease: Lease,
}

impl Block {
    /// Fills the block with random bytes, so that no hop compresses a download. `meter` counts what sources hand out.
    pub fn new(budget: &Budget, meter: Meter) -> Result<Self, String> {
        let lease = budget
            .lease(BLOCK_BYTES)
            .ok_or("the buffer budget cannot cover the download block")?;
        let mut bytes = vec![0; BLOCK_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| format!("download block: {error}"))?;
        let bytes: Bytes = bytes.into();
        #[cfg(target_os = "linux")]
        let sendfile = create_memfd(&bytes).map(|file| Sendfile {
            _backing: bytes.clone(),
            start: bytes.as_ptr() as usize,
            len: bytes.len(),
            file,
        });
        Ok(Self {
            bytes,
            #[cfg(target_os = "linux")]
            sendfile,
            meter,
            _lease: lease,
        })
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn sendfile(&self) -> Option<Sendfile> {
        self.sendfile.clone()
    }

    /// Opens a source of `bytes` payload bytes, one running transfer of the meter while any of its clones lives.
    pub fn source(&self, bytes: u64) -> DownloadSource {
        let transfer = (bytes > 0).then(|| self.meter.open()).flatten();
        DownloadSource {
            block: self.bytes.clone(),
            remaining: bytes,
            transfer,
            until: None,
            _ended: None,
        }
    }

    pub fn meter(&self) -> &Meter {
        &self.meter
    }
}

/// A source hands out one download's remaining payload as slices of the block without copying, until its deadline.
#[derive(Debug, Clone)]
pub struct DownloadSource {
    block: Bytes,
    remaining: u64,
    transfer: Option<Arc<Transfer>>,
    until: Option<Instant>,
    _ended: Option<Arc<DropGuard>>,
}

impl DownloadSource {
    /// Hands out nothing from `until` on, so the download ends there on the server's clock.
    pub fn ending_at(self, until: Instant) -> Self {
        Self {
            until: Some(until),
            ..self
        }
    }

    /// Cancels `ended` once the download and every clone of it are over, however they end.
    pub fn signalling(self, ended: CancellationToken) -> Self {
        Self {
            _ended: Some(Arc::new(ended.drop_guard())),
            ..self
        }
    }

    pub fn next(&mut self, max: usize) -> Option<Bytes> {
        let chunk = self.peek(max)?;
        self.remaining -= chunk.len() as u64;
        self.sent(chunk.len());
        Some(chunk)
    }

    pub fn peek(&self, max: usize) -> Option<Bytes> {
        if self.ended() {
            return None;
        }
        let remaining = usize::try_from(self.remaining).unwrap_or(usize::MAX);
        let length = remaining.min(max).min(self.block.len());
        (length > 0).then(|| self.block.slice(..length))
    }

    /// Counts block bytes sent outside `next`, as a datagram flood repeats them.
    pub fn sent(&self, bytes: usize) {
        if let Some(transfer) = &self.transfer {
            transfer.record(bytes);
        }
    }

    pub fn remaining(&self) -> u64 {
        self.remaining
    }

    pub fn ended(&self) -> bool {
        self.remaining == 0 || self.until.is_some_and(|until| Instant::now() >= until)
    }

    /// The exact length, unless a deadline may end it sooner.
    pub fn length(&self) -> Option<u64> {
        self.until.is_none().then_some(self.remaining)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_budget_that_cannot_cover_the_block_refuses_it() {
        assert!(Block::new(&Budget::new(BLOCK_BYTES - 1), Meter::default()).is_err());
    }

    #[test]
    fn every_download_repeats_one_block_charged_once() {
        let budget = Budget::new(BLOCK_BYTES + 10);
        let block = Block::new(&budget, Meter::new(true)).unwrap();
        assert_eq!(budget.usage().used, BLOCK_BYTES);
        let total = BLOCK_BYTES as u64 * 3 + 5;
        let mut sources = [block.source(total), block.source(total)];
        for source in &mut sources {
            let mut sent = 0;
            while let Some(chunk) = source.next(BLOCK_BYTES) {
                assert!(chunk.len() <= BLOCK_BYTES);
                sent += chunk.len() as u64;
            }
            assert_eq!((sent, source.remaining()), (total, 0));
        }
        assert_eq!(budget.usage().used, BLOCK_BYTES, "sources charge nothing");
        let line = block
            .meter()
            .line(std::time::Duration::from_secs(1))
            .unwrap();
        assert!(line.ends_with(" 2 transfers · 1.57 MB in 1.0 s"), "{line}");
        assert_eq!(block.source(10).next(4).map(|chunk| chunk.len()), Some(4));
        assert!(block.source(0).next(BLOCK_BYTES).is_none());
        drop((sources, block));
        assert_eq!(budget.usage().used, 0);
    }
}
