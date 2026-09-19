//! Exercise adapter ownership after a real QUIC read returns Pending.
use std::{error::Error, future::poll_fn, sync::Arc, task::Poll, time::Duration};

use bytes::Bytes;
use h3::quic::RecvStream as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

mod support;
type TestError = Box<dyn Error + Send + Sync>;

#[tokio::test]
async fn pending_receive_can_be_stopped_and_queried_without_losing_stream_ownership() {
    tokio::time::timeout(Duration::from_secs(10), exercise())
        .await
        .expect("adapter cancellation test timed out")
        .unwrap();
}

async fn exercise() -> Result<(), TestError> {
    let identity = support::Identity::generate()?;
    let certificate = CertificateDer::from_pem_file(identity.directory().join("identity.pem"))?;
    let key = PrivateKeyDer::from_pem_file(identity.directory().join("identity.key"))?;
    let config = quinn::ServerConfig::with_single_cert(vec![certificate.clone()], key)?;
    let server = quinn::Endpoint::server(config, "127.0.0.1:0".parse()?)?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate)?;
    let client = quinn::Endpoint::client("127.0.0.1:0".parse()?)?;
    client.set_default_client_config(quinn::ClientConfig::with_root_certificates(Arc::new(
        roots,
    ))?);
    let connecting = client.connect(server.local_addr()?, "localhost")?;
    let (sender, receiver) =
        tokio::try_join!(async { Ok::<_, TestError>(connecting.await?) }, async {
            Ok::<_, TestError>(server.accept().await.ok_or("endpoint closed")?.await?)
        })?;
    let mut send = sender.open_uni().await?;
    send.write_all(b"x").await?;
    let mut adapter = h3_noq::Connection::new(receiver);
    let mut recv = poll_fn(|cx| {
        <h3_noq::Connection as h3::quic::Connection<Bytes>>::poll_accept_recv(&mut adapter, cx)
    })
    .await
    .map_err(|error| format!("accept failed: {error:?}"))?;
    let first = poll_fn(|cx| recv.poll_data(cx))
        .await
        .map_err(|error| format!("read failed: {error:?}"))?;
    assert_eq!(first.unwrap(), b"x"[..]);
    poll_fn(|cx| {
        assert!(recv.poll_data(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    // This used to panic: the pending boxed read future owned the raw stream.
    assert_eq!(recv.recv_id().into_inner(), u64::from(send.id()));
    recv.stop_sending(73);
    assert_eq!(send.stopped().await?, Some(73_u32.into()));
    sender.close(0_u32.into(), b"test complete");
    client.wait_idle().await;
    server.wait_idle().await;
    Ok(())
}
