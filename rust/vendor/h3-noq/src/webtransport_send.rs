//! Owned outgoing streams preserve the WebTransport association on cancellation.
use bytes::Bytes;
use h3::proto::varint::VarInt;
const MAX_VARINT: u64 = (1 << 62) - 1;

type TransportError = Box<dyn std::error::Error + Send + Sync>;

/// Bounded ownership of streams and cancellation cleanup. No tasks are spawned.
#[derive(Clone)]
pub struct ResetQueue {
    sender: tokio::sync::mpsc::Sender<PendingReset>,
    slots: std::sync::Arc<tokio::sync::Semaphore>,
}

impl ResetQueue {
    /// The receiver owner must drive `PendingReset::complete` until connection
    /// shutdown. Capacity includes open streams, queued and running cleanup.
    pub fn new(capacity: usize) -> (Self, tokio::sync::mpsc::Receiver<PendingReset>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
        (
            Self {
                sender,
                slots: std::sync::Arc::new(tokio::sync::Semaphore::new(capacity)),
            },
            receiver,
        )
    }

    /// Reserve cleanup capacity, then open and associate one outgoing stream.
    pub async fn open(
        &self,
        connection: &quinn::Connection,
        session_id: u64,
        cancel_code: quinn::VarInt,
    ) -> Result<SendStream, TransportError> {
        let prefix = association_prefix(session_id)?;
        let slot = self.slots.clone().acquire_owned().await?;
        let transfer = self.sender.clone().reserve_owned().await?;
        let stream = connection.open_uni().await?;
        let mut owned = SendStream {
            stream: Some(stream),
            prefix,
            written: 0,
            cancel_code,
            transfer: Some(transfer),
            slot: Some(slot),
        };
        while owned.written < owned.prefix.len() {
            let count = owned
                .stream
                .as_mut()
                .unwrap()
                .write(&owned.prefix[owned.written..])
                .await?;
            owned.written += count;
        }
        Ok(owned)
    }
}

/// A cancelled partial prefix, owned by the connection's cleanup driver.
/// Dropping this before completion abandons the delivery guarantee. Keep polling
/// until completion or close the underlying connection before dropping it.
pub struct PendingReset {
    stream: quinn::SendStream,
    remainder: Vec<u8>,
    reliable_size: quinn::VarInt,
    cancel_code: quinn::VarInt,
    _slot: tokio::sync::OwnedSemaphorePermit,
}

impl PendingReset {
    /// Deliver the remaining association prefix before issuing reliable reset.
    pub async fn complete(mut self) -> Result<(), TransportError> {
        self.stream.write_all(&self.remainder).await?;
        self.stream.reset_at(self.reliable_size, self.cancel_code)?;
        Ok(())
    }
}

/// An outgoing unidirectional stream. Dropping it reliably resets after its
/// association prefix; partial prefixes transfer to the bounded cleanup owner.
/// Reliable reset requires peer support. Connection loss cannot guarantee delivery.
pub struct SendStream {
    stream: Option<quinn::SendStream>,
    prefix: Vec<u8>,
    written: usize,
    cancel_code: quinn::VarInt,
    transfer: Option<tokio::sync::mpsc::OwnedPermit<PendingReset>>,
    slot: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl SendStream {
    /// QUIC stream identifier.
    pub fn id(&self) -> quinn::StreamId {
        self.stream.as_ref().unwrap().id()
    }

    /// Cancellation safe, with the same semantics as Noq's `write`.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<usize, quinn::WriteError> {
        self.stream.as_mut().unwrap().write(bytes).await
    }

    /// May write a prefix before cancellation, as with Noq's `write_all`.
    pub async fn write_all(&mut self, bytes: &[u8]) -> Result<(), quinn::WriteError> {
        self.stream.as_mut().unwrap().write_all(bytes).await
    }

    /// Passes owned bytes directly to Noq without an extra payload copy.
    pub async fn write_chunk(&mut self, bytes: Bytes) -> Result<(), quinn::WriteError> {
        self.stream.as_mut().unwrap().write_chunk(bytes).await
    }

    /// Finish normally; queued bytes retain Noq's normal retransmission behavior.
    pub fn finish(mut self) -> Result<(), quinn::ClosedStream> {
        self.stream.as_mut().unwrap().finish()?;
        self.stream.take();
        Ok(())
    }

    /// Abandon the payload while preserving the complete association prefix.
    pub fn reset(mut self, error_code: quinn::VarInt) -> Result<(), quinn::ResetStreamAtError> {
        self.cancel_code = error_code;
        self.stream.as_mut().unwrap().reset_at(
            quinn::VarInt::from_u32(self.prefix.len() as u32),
            error_code,
        )?;
        self.stream.take();
        Ok(())
    }
}

impl Drop for SendStream {
    fn drop(&mut self) {
        let Some(mut stream) = self.stream.take() else {
            return;
        };
        let reliable_size = quinn::VarInt::from_u32(self.prefix.len() as u32);
        if self.written == self.prefix.len() {
            // Do not fall back to a plain reset, which could erase association.
            let _ = stream.reset_at(reliable_size, self.cancel_code);
            return;
        }
        let remainder = self.prefix[self.written..].to_vec();
        let cancel_code = self.cancel_code;
        self.transfer.take().unwrap().send(PendingReset {
            stream,
            remainder,
            reliable_size,
            cancel_code,
            _slot: self.slot.take().unwrap(),
        });
    }
}

fn association_prefix(session_id: u64) -> Result<Vec<u8>, TransportError> {
    if session_id > MAX_VARINT || !session_id.is_multiple_of(4) {
        return Err("WebTransport session ID must be a client bidirectional QUIC stream ID".into());
    }
    let mut prefix = Vec::with_capacity(10);
    VarInt::from_u32(0x54).encode(&mut prefix);
    VarInt::from_u64(session_id)
        .expect("validated session ID")
        .encode(&mut prefix);
    Ok(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use h3::proto::coding::Decode;

    #[test]
    fn association_uses_full_nonzero_connect_id() {
        for id in [0, 4, 64, 16384, (1 << 30), MAX_VARINT - 3] {
            let prefix = association_prefix(id).unwrap();
            assert_eq!(&prefix[..2], &[0x40, 0x54]);
            let mut data = &prefix[2..];
            let decoded = VarInt::decode(&mut data).unwrap();
            assert_eq!(decoded.into_inner(), id);
            assert!(data.is_empty());
        }
    }

    #[test]
    fn rejects_other_stream_types_and_out_of_range_ids() {
        for id in [1, 2, 3, 5, MAX_VARINT, MAX_VARINT + 1, u64::MAX] {
            assert!(association_prefix(id).is_err());
        }
    }
}
