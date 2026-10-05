//! Download payloads: one random block that every download repeats.

use crate::limits::{Budget, Lease};
use bytes::Bytes;

/// The block's size, also the largest chunk a source hands out.
pub const BLOCK_BYTES: usize = 256 << 10;

/// The shared payload block, charged to the budget once for the server's lifetime.
#[derive(Debug)]
pub struct Block {
    bytes: Bytes,
    _lease: Lease,
}

impl Block {
    /// Random bytes, so that no hop compresses a download.
    pub fn new(budget: &Budget) -> Result<Self, String> {
        let lease = budget
            .lease(BLOCK_BYTES)
            .ok_or("the buffer budget cannot cover the download block")?;
        let mut bytes = vec![0; BLOCK_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| format!("download block: {error}"))?;
        Ok(Self { bytes: bytes.into(), _lease: lease })
    }

    /// A source of `bytes` payload bytes.
    pub fn source(&self, bytes: u64) -> DownloadSource {
        DownloadSource { block: self.bytes.clone(), remaining: bytes }
    }
}

/// One download's remaining payload, handed out as slices of the block without copying.
#[derive(Debug)]
pub struct DownloadSource {
    block: Bytes,
    remaining: u64,
}

impl DownloadSource {
    /// The next chunk of at most `max` bytes; `None` once the payload is sent.
    pub fn next(&mut self, max: usize) -> Option<Bytes> {
        let length = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(max)
            .min(self.block.len());
        self.remaining -= length as u64;
        (length > 0).then(|| self.block.slice(..length))
    }

    pub fn remaining(&self) -> u64 {
        self.remaining
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_download_repeats_one_block_charged_once() {
        let budget = Budget::new(BLOCK_BYTES + 10);
        let block = Block::new(&budget).unwrap();
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
        assert_eq!(block.source(10).next(4).map(|chunk| chunk.len()), Some(4));
        assert!(block.source(0).next(BLOCK_BYTES).is_none());
        drop((sources, block));
        assert_eq!(budget.usage().used, 0);
    }

    #[test]
    fn a_budget_that_cannot_cover_the_block_refuses_it() {
        assert!(Block::new(&Budget::new(BLOCK_BYTES - 1)).is_err());
    }
}
