//! WebTransport sessions over loopback QUIC: the close codes, reliable reset, datagrams, early streams and head
//! gating.
mod support;

use bytes::Bytes;
use graphite_meter_http3::{Code, Error, WtCode};
use std::{sync::Arc, time::Duration};
use support::*;
use tokio::sync::{Notify, mpsc};

/// The WebTransport signal of Chromium and Firefox.
const DRAFT02_SETTINGS: &[(u64, u64)] = &[(0x2b603742, 1), (0x33, 1)];
const DRAFT02: Setup = raw(DRAFT02_SETTINGS);

/// Every lane ending's code and reason reach the peer as CLOSE then FIN, a peer that answers with FIN never
/// sees STOP_SENDING, and the connection outlasts the CLOSE by a second.
#[tokio::test]
async fn the_close_code_table_reaches_the_peer() -> Result<(), TestError> {
    for (code, reason) in [(0, ""), (1, "idle"), (2, "lifetime"), (3, "authentication required"), (4, "shutdown")] {
        let Served { peers, serving, mut outcomes, .. } = pair(DRAFT02)
            .await?
            .sessions(move |session| async move { session.close(code, reason).await });
        let (mut connect, response) = peers.connect().await?;
        let (bytes, end) = response.await?;
        assert!(bytes.ends_with(&close_capsule(code, reason)) && end.is_ok(), "CLOSE {code}, then FIN");
        connect.finish()?;
        assert_eq!(stopped(&connect).await, None);
        assert_eq!(outcomes.recv().await, Some(()));
        // The connection lingers a second so the CLOSE arrives first.
        settled(&peers.budget).await;
        assert!(peers.server.close_reason().is_none());
        jump(Duration::from_secs(1)).await;
        assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);
        assert_eq!(serving.await?, Ok(()));
    }
    Ok(())
}

#[tokio::test]
async fn cancelled_lanes_keep_their_association_header() -> Result<(), TestError> {
    // One-byte stream credit makes the header trickle, so the first lane is cancelled mid-header.
    for (reliable_reset, window) in [(true, Some(1)), (true, None), (false, Some(1)), (false, None)] {
        let cancel = Arc::new(Notify::new());
        let cancelled = cancel.clone();
        let setup = Setup { window, reliable_reset, ..DRAFT02 };
        let Served { peers, .. } = pair(setup).await?.sessions(move |session| {
            let cancelled = cancelled.clone();
            async move {
                if window.is_some() {
                    tokio::select! {
                        _ = session.open_uni() => panic!("the header outran its stream credit"),
                        () = cancelled.notified() => {}
                    }
                } else {
                    let mut lane = session.open_uni().await.unwrap();
                    lane.write_all(&[1; 64 * 1024]).await.unwrap();
                    lane.reset(WtCode(7));
                }
                let mut next = session.open_uni().await.unwrap();
                next.write_all(b"next").await.unwrap();
                next.finish().unwrap();
                let _ = session.closed().await;
            }
        });
        let (_connect, _response) = peers.connect().await?;
        // The server's control stream comes first.
        let _server_control = peers.client.accept_uni().await?;
        let mut lane = peers.client.accept_uni().await?;
        let mut bytes = Vec::new();
        if window.is_some() {
            bytes.extend_from_slice(&lane.read_chunk(1).await?.unwrap_or_default());
            cancel.notify_one();
        }
        let (rest, end) = raw_stream(&mut lane).await;
        bytes.extend(rest);
        let code = if window.is_some() { WtCode(0) } else { WtCode(7) };
        assert_eq!(end, Err(code.to_http()), "reset, never FIN");
        if reliable_reset {
            assert_eq!(&bytes[..3], [0x40, 0x54, 0x00], "RESET_STREAM_AT keeps the header");
        }
        let mut next = peers.client.accept_uni().await?;
        assert_eq!(raw_stream(&mut next).await, (b"\x40\x54\x00next".to_vec(), Ok(())));
        drop((lane, next));
        peers.client.close(0_u32.into(), b"done");
        settled(&peers.budget).await;
    }
    Ok(())
}

/// Session streams wait until the 200 head is buffered: sent first, their data would take the small
/// connection window the head needs, and the client reads no stream before its session starts.
#[tokio::test]
async fn session_streams_wait_for_the_head_with_a_small_connection_window() -> Result<(), TestError> {
    let setup = Setup { connection_window: Some(4096), ..PLAIN };
    let Served { peers, .. } = pair(setup).await?.sessions(|session| async move {
        let mut lane = session.open_uni().await.unwrap();
        lane.write_chunk(Bytes::from(vec![7; 64 * 1024])).await.unwrap();
        lane.finish().unwrap();
        let _ = session.closed().await;
    });
    let (_driver, requests) = client(&peers);
    let session = tokio::time::timeout(Duration::from_secs(5), accepted(&requests)).await??;
    let mut lane = session.accept_uni().await.ok_or("a download lane")?;
    let mut received = 0;
    while let Some(chunk) = lane.read_chunk().await? {
        received += chunk.len();
    }
    assert_eq!(received, 64 * 1024);
    Ok(())
}

#[tokio::test]
async fn prepared_datagrams_repeat_and_the_last_session_ends_its_connection() -> Result<(), TestError> {
    let Served { peers, serving, .. } = pair(PLAIN).await?.sessions(|session| async move {
        for _ in 0..2 {
            let reply = session.read_datagram().await.unwrap();
            session.send_datagram(&[&b"echo "[..], &reply].concat()).unwrap();
        }
        let _ = session.closed().await;
    });
    let (driver, requests) = client(&peers);
    let session = accepted(&requests).await?;
    let mut repeated = session.prepare_datagram(b"PING,2")?;
    for _ in 0..2 {
        repeated.send_wait().await?;
        assert_eq!(session.read_datagram().await.as_deref(), Some(&b"echo PING,2"[..]));
    }
    drop(session);
    settled(&peers.budget).await;
    let ended = (driver.await?, serving.await?);
    assert_eq!(ended, (Ok(()), Ok(())), "a sessions-only connection ends with its session");
    Ok(())
}

/// Control data on a CONNECT stream is bounded to 1 MiB in at most 1024 chunks.
#[tokio::test]
async fn connect_stream_control_data_is_bounded() -> Result<(), TestError> {
    let grease = |length: usize| frame(0x00, &frame(0x21, &vec![0; length]));
    // A DATA frame split across packets counts once per part.
    for (data, bounded) in [(grease(1).repeat(1025), true), (grease(512 * 1024), false), (grease(1024 * 1024), true)] {
        let Served { peers, mut outcomes, .. } = pair(DRAFT02).await?.sessions(until_closed);
        peers.get().await?;
        let (mut connect, _response) = peers.connect().await?;
        connect.write_all(&data).await?;
        if bounded {
            assert_eq!(outcomes.recv().await, Some(Err(Error::Protocol(Code::H3_EXCESSIVE_LOAD))));
            assert_eq!(stopped(&connect).await, Some(Code::H3_EXCESSIVE_LOAD));
        } else {
            connect.write_all(&close_capsule(7, "bye")).await?;
            assert_eq!(outcomes.recv().await, Some(Ok((7, "bye".into()))), "{} bytes", data.len());
        }
    }
    Ok(())
}

/// A session stream the budget cannot hold gets the draft's code for one not buffered.
#[tokio::test]
async fn a_session_stream_over_the_budget_is_refused_as_unbuffered() -> Result<(), TestError> {
    let Served { peers, .. } = pair(Setup { limit: 0, ..PLAIN }).await?.serve(|_, _| async {});
    // The first stream is in the floor kept for critical ones; its session stream is not.
    let stream = uni(&peers.client, b"\x40\x54\x00").await?;
    assert_eq!(stopped(&stream).await, Some(Code::WT_BUFFERED_STREAM_REJECTED));
    Ok(())
}

#[tokio::test]
async fn a_peer_withholding_stream_credit_cannot_hold_a_session() -> Result<(), TestError> {
    // No stream credit: the 200 head never leaves, yet the close ends within its wait.
    let accepted = Arc::new(Notify::new());
    let accepting = accepted.clone();
    let setup = Setup { window: Some(0), ..DRAFT02 };
    let Served { peers, mut outcomes, .. } = pair(setup).await?.sessions(move |session| {
        let accepting = accepting.clone();
        async move {
            accepting.notify_one();
            session.close(2, "lifetime").await;
        }
    });
    let (_connect, response) = peers.connect().await?;
    accepted.notified().await;
    yields().await;
    jump(Duration::from_millis(1100)).await;
    tokio::time::timeout(Duration::from_secs(5), outcomes.recv()).await?;
    assert_eq!(response.await?, (Vec::new(), Err(Code::WT_SESSION_GONE)));
    Ok(())
}

/// A close while the 200 head waits for connection credit follows the head, so the peer reads its code.
#[tokio::test]
async fn a_close_before_the_head_is_written_follows_it() -> Result<(), TestError> {
    let accepted = Arc::new(Notify::new());
    let accepting = accepted.clone();
    let setup = Setup { connection_window: Some(0), ..DRAFT02 };
    let Served { peers, mut outcomes, .. } = pair(setup).await?.sessions(move |session| {
        let accepting = accepting.clone();
        async move {
            accepting.notify_one();
            session.close(2, "lifetime").await;
        }
    });
    let (mut connect, response) = peers.connect().await?;
    accepted.notified().await;
    yields().await;
    peers.client.set_receive_window(noq::VarInt::from_u32(1 << 20));
    let (bytes, end) = tokio::time::timeout(Duration::from_millis(900), response).await??;
    let closed = bytes.starts_with(&[0x01]) && bytes.ends_with(&close_capsule(2, "lifetime"));
    assert!(closed && end.is_ok(), "200, CLOSE, then FIN");
    connect.finish()?;
    assert_eq!(stopped(&connect).await, None);
    assert_eq!(outcomes.recv().await, Some(()));
    Ok(())
}

/// A lane cancelled before its association header is written gets a plain reset once 10 s pass.
#[tokio::test]
async fn a_cancelled_lane_gives_up_its_header_after_10_seconds() -> Result<(), TestError> {
    let cancel = Arc::new(Notify::new());
    let cancelled = cancel.clone();
    let setup = Setup { window: Some(1), ..DRAFT02 };
    let Served { peers, .. } = pair(setup).await?.sessions(move |session| {
        let cancelled = cancelled.clone();
        async move {
            tokio::select! {
                _ = session.open_uni() => panic!("the header outran its stream credit"),
                () = cancelled.notified() => {}
            }
            let _ = session.closed().await;
        }
    });
    let (_connect, _response) = peers.connect().await?;
    let _server_control = peers.client.accept_uni().await?;
    let mut lane = peers.client.accept_uni().await?;
    cancel.notify_one();
    yields().await;
    jump(Duration::from_secs(9)).await;
    let early = tokio::time::timeout(Duration::from_millis(100), lane.received_reset()).await;
    assert!(early.is_err(), "reset before the deadline");
    jump(Duration::from_secs(2)).await;
    let (bytes, end) = raw_stream(&mut lane).await;
    assert_eq!(end, Err(WtCode(0).to_http()));
    assert!(bytes.len() < 3, "RESET_STREAM_AT kept a header that never completed");
    Ok(())
}

#[tokio::test]
async fn at_most_64_streams_wait_for_their_session_for_at_most_5_seconds() -> Result<(), TestError> {
    let Served { peers, .. } = pair(PLAIN).await?.serve(|_, _| async {});
    let (refusals, mut refused) = mpsc::unbounded_channel();
    for _ in 0..65 {
        let (stream, refusals) = (uni(&peers.client, b"\x40\x54\x04").await?, refusals.clone());
        tokio::spawn(async move { refusals.send(stopped(&stream).await) });
    }
    let full = Some(Some(Code::WT_BUFFERED_STREAM_REJECTED));
    assert_eq!(refused.recv().await, full, "one stream finds the queue full");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(refused.try_recv().is_err(), "the queued streams wait");
    jump(Duration::from_secs(6)).await;
    for _ in 0..64 {
        assert_eq!(refused.recv().await, full);
    }
    Ok(())
}

#[tokio::test]
async fn webtransport_needs_the_peer_signal_and_datagrams() -> Result<(), TestError> {
    // Our QPACK: :status 200 is static index 25, 400 is 67.
    let (ok, bad_request) = (frame(0x01, &[0x00, 0x00, 0xd9]), frame(0x01, &[0x00, 0x00, 0xff, 0x04]));
    for (setup, draft02, allowed) in [
        (DRAFT02, true, true),
        (raw(&[(0x2c7cf000, 1), (0x33, 1)]), false, true),
        (raw(&[]), false, false),
    ] {
        let Served { peers, .. } = pair(setup).await?.sessions(until_closed);
        let (send, mut recv) = peers.bi(&connect_head()).await?;
        let head = first_frame(&mut recv).await?;
        match (allowed, draft02) {
            // Draft 02 requires the response header naming it.
            (true, true) => assert!(head.len() > ok.len() && head[2..5] == ok[2..], "{head:x?}"),
            (true, false) => assert_eq!(head, ok),
            (false, _) => assert_eq!(head, bad_request, "{:x?}", setup.settings),
        }
        drop((send, recv));
    }

    // Without the peer's SETTINGS the CONNECT waits 5 s, then gets 400.
    let Served { peers, .. } = pair(PLAIN).await?.sessions(|_| async {});
    let (send, mut recv) = peers.bi(&connect_head()).await?;
    charged(&peers.budget, 0).await;
    tokio::task::yield_now().await;
    jump(Duration::from_secs(6)).await;
    assert_eq!(first_frame(&mut recv).await?, bad_request);
    drop(send);
    Ok(())
}
