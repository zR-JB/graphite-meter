//! One upload lane's bytes into its aggregate.

use super::uploads::Aggregate;
use crate::lane::Lane;
use std::sync::Arc;

/// A joined data lane; dropping it leaves the aggregate, which may then complete.
pub struct UploadSink {
    aggregate: Arc<Aggregate>,
    lane: Lane,
    bytes: u64,
}

impl UploadSink {
    pub(super) fn new(aggregate: Arc<Aggregate>, lane: Lane) -> Self {
        Self { aggregate, lane, bytes: 0 }
    }

    /// Counts bytes received from the peer, once per chunk or batch; receiving any is the lane's movement.
    pub fn record(&mut self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        self.lane.moved();
        self.aggregate.record(bytes as u64);
        self.bytes += bytes as u64;
    }

    /// The bytes this lane received.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for UploadSink {
    fn drop(&mut self) {
        self.aggregate.leave();
    }
}
