//! Real QUIC cancellation with a one-byte stream window, not simulated writes.
use std::{error::Error, sync::Arc, time::Duration};

use graphite_meter_server::webtransport_send::ResetQueue;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

mod support;

type TestError = Box<dyn Error + Send + Sync>;

#[tokio::test]
async fn partial_prefix_cancellation_retains_association_and_releases_capacity() {
    tokio::time::timeout(Duration::from_secs(10), exercise(false))
        .await
        .expect("QUIC cancellation test timed out")
        .unwrap();
}

#[tokio::test]
async fn immediate_full_prefix_reset_retains_all_association_bytes() {
    tokio::time::timeout(Duration::from_secs(10), exercise(true))
        .await
        .expect("immediate reset test timed out")
        .unwrap();
}

async fn exercise(immediate: bool) -> Result<(), TestError> {
    let identity = support::Identity::generate();
    let cert = CertificateDer::from_pem_file(identity.directory().join("identity.pem"))?;
    let key = PrivateKeyDer::from_pem_file(identity.directory().join("identity.key"))?;
    let mut server_config = quinn::ServerConfig::with_single_cert(vec![cert.clone()], key)?;
    let mut transport = quinn::TransportConfig::default();
    transport.stream_receive_window(if immediate { 64_u32 } else { 1 }.into());
    transport.receive_window(if immediate { 64_u32 } else { 1 }.into());
    transport.max_concurrent_uni_streams(1_u32.into());
    server_config.transport_config(Arc::new(transport));
    let server = quinn::Endpoint::server(server_config, "127.0.0.1:0".parse()?)?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert)?;
    let client = quinn::Endpoint::client("127.0.0.1:0".parse()?)?;
    client.set_default_client_config(quinn::ClientConfig::with_root_certificates(Arc::new(
        roots,
    ))?);
    let connecting = client.connect(server.local_addr()?, "localhost")?;
    let (sender, receiver) =
        tokio::try_join!(async { Ok::<_, TestError>(connecting.await?) }, async {
            Ok::<_, TestError>(server.accept().await.ok_or("endpoint closed")?.await?)
        })?;

    let (factory, mut cleanup) = ResetQueue::new(1);
    let code = quinn::VarInt::from_u32(73);
    if immediate {
        factory.open(&sender, 256, code).await?.reset(code)?;
        let mut incoming = receiver.accept_uni().await?;
        let mut bytes = Vec::new();
        loop {
            match incoming.read_chunk(16).await {
                Ok(Some(chunk)) => bytes.extend_from_slice(&chunk),
                Err(quinn::ReadError::Reset(actual)) => {
                    assert_eq!(actual, code);
                    break;
                }
                other => panic!("expected complete prefix then reset: {other:?}"),
            }
        }
        assert_eq!(bytes, [0x40, 0x54, 0x41, 0x00]);
    } else {
        let mut opening = Box::pin(factory.open(&sender, 256, code));
        // Drive open only until the peer sees the stream. Its one-byte window
        // prevents completion of the four-byte association prefix.
        let mut incoming = tokio::select! {
            result = &mut opening => panic!("open completed before prefix credit: {}", result.is_ok()),
            stream = receiver.accept_uni() => stream?,
        };
        // Do not poll opening while consuming this byte: cancellation occurs before
        // its future can use the newly granted flow-control credit.
        assert_eq!(incoming.read_chunk(1).await?.unwrap().as_ref(), &[0x40]);
        drop(opening);
        let pending = cleanup
            .try_recv()
            .expect("cancelled open must transfer ownership");
        let ((), bytes) = tokio::try_join!(pending.complete(), async {
            let mut bytes = vec![0x40];
            loop {
                match incoming.read_chunk(16).await {
                    Ok(Some(chunk)) => bytes.extend_from_slice(&chunk),
                    Err(quinn::ReadError::Reset(actual)) => {
                        assert_eq!(actual, code);
                        break;
                    }
                    other => panic!("expected prefix then reliable reset, got {other:?}"),
                }
            }
            Ok::<_, TestError>(bytes)
        })?;
        assert_eq!(bytes, [0x40, 0x54, 0x41, 0x00]);
        drop(incoming);
    }

    // A fresh wrapper requires the sole capacity permit to have been returned.
    // It also proves cancellation did not close the shared QUIC connection.
    let ((), bytes) = tokio::try_join!(
        async {
            let mut stream = factory.open(&sender, 4, code).await?;
            stream.write_all(b"next").await?;
            stream.finish()?;
            Ok::<_, TestError>(())
        },
        async {
            let mut stream = receiver.accept_uni().await?;
            let mut bytes = Vec::new();
            while let Some(chunk) = stream.read_chunk(16).await? {
                bytes.extend_from_slice(&chunk);
            }
            Ok::<_, TestError>(bytes)
        }
    )?;
    assert_eq!(bytes, b"\x40\x54\x04next");
    assert!(cleanup.try_recv().is_err());
    sender.close(0_u32.into(), b"test complete");
    client.wait_idle().await;
    server.wait_idle().await;
    Ok(())
}
