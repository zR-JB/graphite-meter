//! One upload lane's bytes into its aggregate.

use super::{meter::Transfer, uploads::Aggregate};
use crate::lane::Lane;
use std::{future::Future, pin::pin, sync::Arc};

/// A joined data lane; dropping it leaves the aggregate, which may then complete.
pub struct UploadSink {
    aggregate: Arc<Aggregate>,
    lane: Lane,
    bytes: u64,
    transfer: Option<Transfer>,
}

impl UploadSink {
    pub(super) fn new(aggregate: Arc<Aggregate>, lane: Lane, transfer: Option<Transfer>) -> Self {
        Self { aggregate, lane, bytes: 0, transfer }
    }

    /// Counts bytes received from the peer, once per chunk or batch; receiving any is the lane's movement.
    pub fn record(&mut self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        self.lane.moved();
        self.aggregate.record(bytes as u64);
        self.bytes += bytes as u64;
        if let Some(transfer) = &self.transfer {
            transfer.record(bytes);
        }
    }

    /// The bytes this lane received.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Completes once the upload is finalized or expired: the end of a lane without a stream FIN.
    pub fn finished(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        let aggregate = self.aggregate.clone();
        async move {
            loop {
                let mut changed = pin!(aggregate.changed.notified());
                changed.as_mut().enable();
                if aggregate.ended() {
                    return;
                }
                changed.await;
            }
        }
    }
}

impl Drop for UploadSink {
    fn drop(&mut self) {
        self.aggregate.leave();
    }
}
