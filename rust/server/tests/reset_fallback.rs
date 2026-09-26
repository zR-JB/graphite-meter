mod support;
use graphite_meter_server::webtransport_send::ResetQueue;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{error::Error, sync::Arc, time::Duration};
type TestError = Box<dyn Error + Send + Sync>;
enum Case {
    DropAfterData,
    ExplicitReset,
    PartialPrefix,
}

#[tokio::test]
async fn no_reset_peer_sees_reset_after_drop_with_queued_data() -> Result<(), TestError> {
    no_reset_peer(Case::DropAfterData).await
}

#[tokio::test]
async fn no_reset_peer_sees_reset_after_explicit_reset() -> Result<(), TestError> {
    no_reset_peer(Case::ExplicitReset).await
}

#[tokio::test]
async fn no_reset_peer_partial_prefix_cleanup_keeps_connection() -> Result<(), TestError> {
    no_reset_peer(Case::PartialPrefix).await
}

async fn no_reset_peer(case: Case) -> Result<(), TestError> {
    tokio::time::timeout(Duration::from_secs(10), no_reset_peer_inner(case))
        .await
        .map_err(|_| "no-reset peer test timed out")?
}

async fn no_reset_peer_inner(case: Case) -> Result<(), TestError> {
    let identity = support::Identity::generate();
    let cert = CertificateDer::from_pem_file(identity.directory().join("identity.pem"))?;
    let key = PrivateKeyDer::from_pem_file(identity.directory().join("identity.key"))?;
    let mut server_config = quinn::ServerConfig::with_single_cert(vec![cert.clone()], key)?;
    let partial = matches!(case, Case::PartialPrefix);
    let mut transport = quinn::TransportConfig::default();
    if partial {
        transport.stream_receive_window(1_u32.into());
        transport.receive_window(1_u32.into());
    }
    server_config.transport_config(Arc::new(transport));
    let mut endpoint_config = quinn::EndpointConfig::default();
    endpoint_config.reliable_stream_reset(false);
    let receiver_endpoint = quinn::Endpoint::new(
        endpoint_config,
        Some(server_config),
        std::net::UdpSocket::bind("127.0.0.1:0")?,
        quinn::default_runtime().ok_or("runtime")?,
    )?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert)?;
    let sender_endpoint = quinn::Endpoint::client("127.0.0.1:0".parse()?)?;
    sender_endpoint.set_default_client_config(quinn::ClientConfig::with_root_certificates(
        Arc::new(roots),
    )?);
    let connecting = sender_endpoint.connect(receiver_endpoint.local_addr()?, "localhost")?;
    let (sender, receiver) =
        tokio::try_join!(async { Ok::<_, TestError>(connecting.await?) }, async {
            Ok::<_, TestError>(receiver_endpoint.accept().await.ok_or("closed")?.await?)
        })?;
    let (factory, mut cleanup) = ResetQueue::new(4);
    let code = quinn::VarInt::from_u32(73);
    let outcome = match case {
        Case::DropAfterData | Case::ExplicitReset => {
            let mut stream = factory.open(&sender, 256, code).await?;
            stream.write_all(&[1; 64 * 1024]).await?;
            match case {
                Case::ExplicitReset => {
                    stream.reset(code);
                }
                _ => drop(stream),
            }
            let mut incoming = receiver.accept_uni().await?;
            read_to_outcome(&mut incoming).await
        }
        Case::PartialPrefix => {
            let mut opening = Box::pin(factory.open(&sender, 256, code));
            let mut incoming = tokio::select! {
                result = &mut opening => panic!("open completed before prefix credit: {}", result.is_ok()),
                stream = receiver.accept_uni() => stream?,
            };
            assert_eq!(incoming.read_chunk(1).await?.unwrap().as_ref(), &[0x40]);
            drop(opening);
            let pending = cleanup.try_recv();
            let (completed, outcome) = match pending {
                Ok(pending) => {
                    let (completed, outcome) =
                        tokio::join!(pending.complete(), read_to_outcome(&mut incoming));
                    (completed.map_err(|e| e.to_string()), outcome)
                }
                Err(_) => (Ok(()), read_to_outcome(&mut incoming).await),
            };
            eprintln!("partial prefix cleanup toward no-reset peer: {completed:?}");
            assert!(
                completed.is_ok(),
                "cleanup error would close the whole connection (http_quic.rs:127): {completed:?}"
            );
            outcome
        }
    };
    eprintln!("receiver outcome: {outcome}");
    assert!(
        outcome.starts_with("reset"),
        "cancelled lane must end with RESET_STREAM, got {outcome}"
    );
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
            while let Some(chunk) = stream.read_chunk(usize::MAX).await? {
                bytes.extend_from_slice(&chunk);
            }
            Ok::<_, TestError>(bytes)
        }
    )?;
    assert_eq!(bytes, b"\x40\x54\x04next");
    sender.close(0_u32.into(), b"done");
    Ok(())
}

async fn read_to_outcome(stream: &mut quinn::RecvStream) -> String {
    let mut total = 0_usize;
    loop {
        match stream.read_chunk(usize::MAX).await {
            Ok(Some(chunk)) => total += chunk.len(),
            Ok(None) => return format!("FIN after {total} bytes"),
            Err(quinn::ReadError::Reset(code)) => {
                return format!("reset {code} after {total} bytes");
            }
            Err(error) => return format!("error {error} after {total} bytes"),
        }
    }
}
