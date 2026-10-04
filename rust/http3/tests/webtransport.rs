//! WebTransport sessions over loopback QUIC: the close sequence and its codes, reliable reset, streams and
//! datagrams, early streams and head gating.
mod support;

use bytes::Bytes;
use graphite_meter_http3::{Code, Error, WtCode, client, webtransport::Session};
use std::{sync::Arc, time::Duration};
use support::*;
use tokio::sync::{Notify, mpsc};

/// The WebTransport signal of Chromium and Firefox.
const DRAFT02_SETTINGS: &[(u64, u64)] = &[(0x2b603742, 1), (0x33, 1)];
const DRAFT02: Setup = raw(DRAFT02_SETTINGS);

/// Every lane ending's code and reason reach the peer as CLOSE then FIN, and a peer that answers with FIN
/// never sees STOP_SENDING.
#[tokio::test]
async fn the_close_code_table_reaches_the_peer() -> Result<(), TestError> {
    for (code, reason) in [(0, ""), (1, "idle"), (2, "lifetime"), (3, "authentication required"), (4, "shutdown")] {
        let Served { peers, mut outcomes, .. } = pair(DRAFT02)
            .await?
            .sessions(move |session| async move { session.close(code, reason).await });
        let (mut connect, response) = peers.connect().await?;
        let (bytes, end) = response.await?;
        assert!(bytes.ends_with(&close_capsule(code, reason)) && end.is_ok(), "CLOSE {code}, then FIN");
        connect.finish()?;
        assert_eq!(stopped(&connect).await, None);
        assert_eq!(outcomes.recv().await, Some(()));
    }
    Ok(())
}

#[tokio::test]
async fn closing_sends_close_then_fin_and_waits_for_the_peer() -> Result<(), TestError> {
    // The server ends the session; our client's FIN ends the wait long before 1 s.
    let Served { peers, serving, .. } = pair(PLAIN).await?.sessions(|session| async move {
        let started = std::time::Instant::now();
        session.close(2, "lifetime").await;
        assert!(started.elapsed() < Duration::from_millis(900));
    });
    let (driver, requests) = client(&peers);
    let session = accepted(&requests).await?;
    assert_eq!(session.closed().await?, (2, "lifetime".into()));
    session.close(0, "").await;
    settled(&peers.budget).await;
    jump(Duration::from_secs(1)).await;
    assert_eq!((driver.await?, serving.await?), (Ok(()), Ok(())));
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);

    // The peer ends it: the server only finishes its side, and the connection closes at once.
    let Served { peers, serving, .. } = pair(PLAIN).await?.sessions(|session| async move {
        assert_eq!(session.closed().await.unwrap(), (7, "bye".into()));
        session.close(1, "unused").await;
    });
    let (driver, requests) = client(&peers);
    accepted(&requests).await?.close(7, "bye").await;
    assert_eq!((driver.await?, serving.await?), (Ok(()), Ok(())));

    // A raw peer answering with FIN: the connection lingers a second so the CLOSE arrives first.
    let Served { peers, serving, .. } = pair(DRAFT02)
        .await?
        .sessions(|session| async move { session.close(2, "lifetime").await });
    let (mut connect, response) = peers.connect().await?;
    assert!(response.await?.1.is_ok(), "CLOSE, then FIN");
    connect.finish()?;
    assert_eq!(stopped(&connect).await, None);
    settled(&peers.budget).await;
    assert!(peers.server.close_reason().is_none());
    jump(Duration::from_secs(1)).await;
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);
    assert_eq!(serving.await?, Ok(()));

    // A silent peer gets STOP_SENDING WT_SESSION_GONE only once the second passes.
    let Served { peers, .. } = pair(DRAFT02)
        .await?
        .sessions(|session| async move { session.close(4, "shutdown").await });
    let (connect, response) = peers.connect().await?;
    let (bytes, end) = response.await?;
    assert!(bytes.ends_with(&close_capsule(4, "shutdown")) && end.is_ok(), "CLOSE, then FIN");
    jump(Duration::from_millis(900)).await;
    assert!(!stopped_yet(&connect).await, "STOP_SENDING before the second");
    jump(Duration::from_millis(200)).await;
    assert_eq!(stopped(&connect).await, Some(Code::WT_SESSION_GONE));
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

/// A download lane the server resets with plain RESET_STREAM, as a client without reliable reset gets
/// them, leaves the connection serving.
#[tokio::test]
async fn a_cancelled_lane_without_reliable_reset_leaves_the_connection_usable() -> Result<(), TestError> {
    let setup = Setup { reliable_reset: false, ..PLAIN };
    let Served { peers, .. } = pair(setup).await?.sessions(|session| async move {
        let mut lane = session.open_uni().await.unwrap();
        while lane.write_chunk(Bytes::from(vec![7; 64 * 1024])).await.is_ok() {}
        let _ = session.closed().await;
    });
    let (_driver, requests) = client(&peers);
    // A plain request keeps the connection past its sessions.
    requests.send_request(get("/")).await?.split().1.response().await?;
    for _ in 0..2 {
        let session = accepted(&requests).await?;
        let mut lane = session.accept_uni().await.ok_or("a download lane")?;
        assert!(lane.read_chunk().await?.is_some());
        session.close(0, "").await;
        assert_eq!(lane.read_chunk().await, Err(Error::Refused));
        assert!(peers.client.close_reason().is_none());
    }
    let (_send, mut recv) = requests.send_request(get("/")).await?.split();
    assert_eq!(recv.response().await?.status(), http::StatusCode::OK);
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

#[tokio::test]
async fn a_malformed_datagram_closes_the_connection() -> Result<(), TestError> {
    let Served { peers, serving, .. } = pair(PLAIN).await?.serve(|_, _| async {});
    // A quarter stream ID must fit a stream ID.
    peers.client.send_datagram(varint(1 << 60).into())?;
    assert_eq!(closed_with(&peers.client).await, Code::H3_DATAGRAM_ERROR);
    assert_eq!(serving.await?, Err(closed(Code::H3_DATAGRAM_ERROR)));
    Ok(())
}

#[tokio::test]
async fn sessions_end_with_their_connection() -> Result<(), TestError> {
    // The server's connection goes away: each side's session ends with it.
    let Served { peers, serving, mut outcomes, .. } = pair(PLAIN)
        .await?
        .sessions(|session| async move { session_end(&session).await });
    let (mut driver, requests) = client::new(peers.client.clone());
    let driving = tokio::spawn(async move { (driver.drive().await, driver) });
    let session = accepted(&requests).await?;
    peers.server.close(Code::H3_NO_ERROR.into(), b"restart");
    let (driven, _stopped) = driving.await?;
    assert_eq!(driven, Ok(()));
    let restart = Error::Connection {
        local: false,
        code: Code::H3_NO_ERROR,
        reason: Bytes::from_static(b"restart"),
    };
    assert_eq!(session_end(&session).await, (true, true, Err(restart)));
    let closed_here = Err(Error::Transport(noq::ConnectionError::LocallyClosed));
    assert_eq!(outcomes.recv().await, Some((true, true, closed_here)));
    assert_eq!(serving.await?, Ok(()));

    // Dropping the driver ends its session too.
    let Served { peers, .. } = pair(PLAIN).await?.sessions(until_closed);
    let (driver, requests) = client(&peers);
    let session = accepted(&requests).await?;
    driver.abort();
    assert!(driver.await.is_err_and(|error| error.is_cancelled()));
    assert_eq!(session_end(&session).await, (true, true, Err(closed(Code::H3_NO_ERROR))));
    Ok(())
}

/// A client ignores content-length in a successful response to CONNECT (RFC 9110 §9.3.6), so the
/// session's capsules follow one that says 0.
#[tokio::test]
async fn a_successful_connect_ignores_its_content_length() -> Result<(), TestError> {
    let peers = pair(PLAIN).await?;
    // A raw server whose SETTINGS allow WebTransport as Go's do.
    let _control = uni(&peers.server, &control(&[(0x08, 1), (0x33, 1), (0x2c7cf000, 1)])).await?;
    let (_driver, requests) = client(&peers);
    let serving = async {
        let (mut send, mut recv) = peers.server.accept_bi().await?;
        recv.read_chunk(usize::MAX).await?;
        let head = frame(0x01, &section(&[(":status", "200"), ("content-length", "0")]));
        send.write_all(&[head, close_capsule(7, "bye")].concat()).await?;
        send.finish()?;
        Ok::<_, TestError>((send, recv))
    };
    let (connected, served) = tokio::join!(Session::connect(&requests, get("/wt")), serving);
    let _streams = served?;
    let (session, _) = connected?.expect("accepted");
    assert_eq!(session.closed().await, Ok((7, "bye".into())));
    Ok(())
}

#[tokio::test]
async fn data_after_the_peers_close_is_a_message_error() -> Result<(), TestError> {
    let Served { peers, mut outcomes, .. } = pair(DRAFT02).await?.sessions(until_closed);
    // A plain request first, so the connection outlives the session and the stream's end shows.
    peers.get().await?;
    let (mut connect, _response) = peers.connect().await?;
    // Only FIN may follow a CLOSE; a DRAIN may not.
    let drain = frame(0x00, &frame(0x78ae, &[]));
    connect.write_all(&[close_capsule(7, "bye"), drain].concat()).await?;
    assert_eq!(outcomes.recv().await, Some(Ok((7, "bye".into()))));
    assert_eq!(stopped(&connect).await, Some(Code::H3_MESSAGE_ERROR));
    settled(&peers.budget).await;
    Ok(())
}

/// Control data on a CONNECT stream is bounded to 1 MiB in at most 1024 chunks.
#[tokio::test]
async fn connect_stream_control_data_is_bounded() -> Result<(), TestError> {
    let grease = |length: usize| frame(0x00, &frame(0x21, &vec![0; length]));
    // A DATA frame split across packets counts once per part.
    for (data, bounded) in [
        (grease(1).repeat(1000), false),
        (grease(1).repeat(1025), true),
        (grease(512 * 1024), false),
        (grease(1024 * 1024), true),
    ] {
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

/// Once a session ended, it opens no stream and sends no datagram, and its streams refuse reads and
/// writes and end with WT_SESSION_GONE. A read or write already waiting then wakes at once.
#[tokio::test]
async fn an_ended_session_ends_its_streams_and_datagrams() -> Result<(), TestError> {
    // The client's 16-byte window holds the server's write.
    let setup = Setup { window: Some(16), ..DRAFT02 };
    let Served { peers, mut outcomes, .. } = pair(setup).await?.sessions(|session| async move {
        let (mut lane, mut own) = (session.accept_uni().await.unwrap(), session.open_uni().await.unwrap());
        let mut datagram = session.prepare_datagram(b"late").unwrap();
        let waiting = tokio::join!(lane.read_chunk(), own.write_chunk(Bytes::from_static(&[0; 64])));
        vec![
            waiting.0.err(),
            waiting.1.err(),
            lane.read_chunk().await.err(),
            own.write_all(b"late").await.err(),
            session.send_datagram(b"late").err(),
            datagram.send_wait().await.err(),
            session.open_uni().await.err(),
        ]
    });
    // A plain request first, so the connection outlives the session and cannot wake them instead.
    peers.get().await?;
    let (mut connect, _response) = peers.connect().await?;
    // A stream of session 4, the second request stream, that sends the server's read nothing.
    let lane = uni(&peers.client, b"\x40\x54\x04").await?;
    let (_server_control, mut own) = (peers.client.accept_uni().await?, peers.client.accept_uni().await?);
    // A byte past the header: the server's write has begun and its read waits.
    own.read_exact(&mut [0; 4]).await?;
    connect.write_all(&close_capsule(2, "lifetime")).await?;
    let ended = tokio::time::timeout(Duration::from_secs(5), outcomes.recv()).await?;
    assert_eq!(ended, Some(vec![Some(Error::Refused); 7]));
    assert_eq!(stopped(&lane).await, Some(Code::WT_SESSION_GONE));
    assert_eq!(raw_stream(&mut own).await.1, Err(Code::WT_SESSION_GONE));
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

    // Nor can it hold the refusal of a CONNECT that never showed WebTransport SETTINGS.
    let setup = Setup { window: Some(0), ..PLAIN };
    let Served { peers, .. } = pair(setup)
        .await?
        .sessions(|_| async { panic!("accepted without SETTINGS") });
    let (_connect, response) = peers.connect().await?;
    charged(&peers.budget, 0).await;
    jump(Duration::from_millis(5100)).await;
    yields().await;
    jump(Duration::from_millis(10_100)).await;
    assert_eq!(response.await?, (Vec::new(), Err(Code::H3_REQUEST_CANCELLED)));
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
async fn shutdown_closes_every_session_before_the_connection() -> Result<(), TestError> {
    let Served { peers, serving, stop, .. } = pair(PLAIN).await?.sessions(until_closed);
    let (driver, requests) = client(&peers);
    let session = accepted(&requests).await?;
    stop.notify_one();
    assert_eq!(session.closed().await?, (4, "shutdown".into()));
    until_goaway(&requests).await;
    assert_eq!(Session::connect(&requests, get("/wt")).await.err(), Some(Error::GoingAway));
    drop(session);
    settled(&peers.budget).await;
    jump(Duration::from_secs(1)).await;
    assert_eq!((driver.await?, serving.await?), (Ok(()), Ok(())));
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);

    let Served { peers, serving, stop, mut outcomes } = pair(PLAIN).await?.sessions(until_closed);
    // Admitted before GOAWAY, this CONNECT waits for SETTINGS and is accepted only after it.
    let (mut connect, response) = peers.connect().await?;
    charged(&peers.budget, 0).await;
    stop.notify_one();
    let mut incoming = peers.client.accept_uni().await?;
    let mut received = Vec::new();
    while !received.ends_with(&frame(0x07, &varint(4))) {
        received.extend_from_slice(&incoming.read_chunk(usize::MAX).await?.ok_or("control stream ended")?);
    }
    let (_late, mut late_response) = peers.bi(&connect_head()).await?;
    assert_eq!(raw_stream(&mut late_response).await.1, Err(Code::H3_REQUEST_REJECTED));
    let _control = uni(&peers.client, &control(DRAFT02_SETTINGS)).await?;
    let (bytes, end) = response.await?;
    let closed = bytes.starts_with(&[0x01]) && bytes.ends_with(&close_capsule(4, "shutdown"));
    assert!(closed && end.is_ok(), "200, CLOSE, then FIN");
    assert_eq!(outcomes.recv().await, Some(Ok((4, "shutdown".into()))));
    connect.finish()?;
    assert_eq!(stopped(&connect).await, None);
    settled(&peers.budget).await;
    assert!(peers.server.close_reason().is_none(), "the CLOSE goes first");
    jump(Duration::from_secs(1)).await;
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);
    assert_eq!(serving.await?, Ok(()));
    Ok(())
}

#[tokio::test]
async fn one_session_per_connection_and_streams_wait_for_theirs() -> Result<(), TestError> {
    let release = Arc::new(Notify::new());
    let (started, mut sessions) = mpsc::unbounded_channel();
    let releasing = release.clone();
    let Served { peers, .. } = pair(DRAFT02).await?.sessions(move |session| {
        let (release, started) = (releasing.clone(), started.clone());
        async move {
            // A session reports 0 when it starts, then 1000 and the streams it took.
            let _ = started.send(0);
            let mut streams = 0;
            tokio::select! {
                () = release.notified() => {}
                () = async { while session.accept_uni().await.is_some() { streams += 1 } } => {}
            }
            let _ = started.send(streams + 1000);
        }
    });
    // A plain request first: this connection is not sessions-only, so it outlives its sessions.
    peers.get().await?;
    // A stream for session 12 arrives before its CONNECT and waits.
    let early = uni(&peers.client, &[varint(0x54), varint(12), b"early".to_vec()].concat()).await?;
    let (_first, _first_response) = peers.connect().await?;
    assert_eq!(sessions.recv().await, Some(0));
    let (second, mut second_response) = peers.bi(&connect_head()).await?;
    assert_eq!(raw_stream(&mut second_response).await.1, Err(Code::H3_REQUEST_REJECTED));
    assert_eq!(stopped(&second).await, Some(Code::H3_REQUEST_REJECTED));
    release.notify_one();
    assert_eq!(sessions.recv().await, Some(1000));
    // Session 4 is gone; a stream for it is refused at once.
    let gone = uni(&peers.client, &[varint(0x54), varint(4)].concat()).await?;
    assert_eq!(stopped(&gone).await, Some(Code::WT_SESSION_GONE));
    let (_third, _third_response) = peers.connect().await?;
    assert_eq!(sessions.recv().await, Some(0));
    drop(early);
    release.notify_one();
    assert_eq!(sessions.recv().await, Some(1001), "the early stream joined session 12");
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
        (raw(&[(0x2b603742, 1)]), false, false),
        (raw(&[(0xffd277, 1)]), false, false),
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

/// A client awaits the server's SETTINGS as long as its caller, as webtransport-go does, not 5 s, then
/// names why no session starts: SETTINGS without WebTransport, or the connection they closed.
#[tokio::test]
async fn a_client_awaits_settings_to_name_why_no_session_starts() -> Result<(), TestError> {
    for (pairs, expected) in [
        (&[(0x33, 1)][..], Error::NoWebTransport),
        (&[(0x21, 0), (0x21, 1)], closed(Code::H3_SETTINGS_ERROR)),
    ] {
        let peers = pair(PLAIN).await?;
        let (_driver, requests) = client(&peers);
        let connecting = tokio::spawn(async move { Session::connect(&requests, get("/wt")).await.err() });
        tokio::task::yield_now().await;
        jump(Duration::from_secs(6)).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!connecting.is_finished(), "gave up waiting for SETTINGS");
        let _control = uni(&peers.server, &control(pairs)).await?;
        assert_eq!(connecting.await?, Some(expected));
    }
    Ok(())
}
