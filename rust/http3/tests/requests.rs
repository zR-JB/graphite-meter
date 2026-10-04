//! The connection driver and request streams over loopback QUIC.
mod support;

use bytes::Bytes;
use graphite_meter_http3::{Code, Error, server};
use std::{
    future::{Future, poll_fn},
    pin::pin,
    sync::{Arc, atomic::Ordering},
    task::Poll,
    time::Duration,
};
use support::*;
use tokio::sync::Notify;

#[tokio::test]
async fn requests_round_trip_and_bodiless_responses_may_declare_a_length() -> Result<(), TestError> {
    let Served { peers, serving, .. } = pair(PLAIN).await?.serve(|request, stream| async move {
        let (mut send, mut recv) = stream.split();
        if request.method() == http::Method::POST {
            let echoed = body(&mut recv).await.unwrap();
            send.send_response(http::Response::new(())).await.unwrap();
            send.send_data(echoed.into()).await.unwrap();
        } else {
            let status: u16 = request.uri().path()[1..].parse().unwrap();
            let response = http::Response::builder().status(status).header("content-length", 11);
            send.send_response(response.body(()).unwrap()).await.unwrap();
        }
        send.finish().await.unwrap();
    });
    let (driver, requests) = client(&peers);
    let (mut send, mut recv) = requests
        .send_request(http::Request::post("https://localhost/").body(())?)
        .await?
        .split();
    send.send_data(Bytes::from_static(b"ping")).await?;
    send.finish().await?;
    assert_eq!(recv.response().await?.status(), http::StatusCode::OK);
    assert_eq!(body(&mut recv).await?, b"ping");
    for (method, status) in [("GET", 204), ("GET", 304), ("HEAD", 200)] {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://localhost/{status}"));
        let (mut send, mut recv) = requests.send_request(request.body(())?).await?.split();
        send.finish().await?;
        assert_eq!(recv.response().await?.status(), status);
        assert_eq!(body(&mut recv).await, Ok(Vec::new()), "{status}");
    }
    drop((send, recv));
    settled(&peers.budget).await;
    peers.client.close(0_u32.into(), b"done");
    assert!(driver.await?.is_ok() && serving.await?.is_ok(), "a peer close code 0 is graceful");
    Ok(())
}

#[tokio::test]
async fn abandoned_streams_carry_the_codes_table() -> Result<(), TestError> {
    let Served { peers, .. } = pair(PLAIN).await?.serve(|request, stream| async move {
        let (mut send, recv) = stream.split();
        send.send_response(http::Response::new(())).await.unwrap();
        if request.uri().path() == "/unread" {
            // The response completes while the request body is unread.
            send.send_data(Bytes::from_static(b"done")).await.unwrap();
            send.finish().await.unwrap();
            drop(recv);
        } else {
            send.send_data(Bytes::from_static(b"partial")).await.unwrap();
        }
    });
    let (_driver, requests) = client(&peers);
    let (mut send, mut recv) = requests.send_request(get("/unread")).await?.split();
    assert_eq!(recv.response().await?.status(), http::StatusCode::OK);
    assert_eq!(body(&mut recv).await, Ok(b"done".to_vec()));
    let stopped = loop {
        if let Err(error) = send.send_data(Bytes::from(vec![0; 64 * 1024])).await {
            break error;
        }
    };
    assert_eq!(stopped, Error::Stopped(Code::H3_NO_ERROR));
    // RESET_STREAM may discard the head along with the body.
    let (_send, mut recv) = requests.send_request(get("/unfinished")).await?.split();
    let received = async {
        recv.response().await?;
        body(&mut recv).await
    }
    .await;
    assert_eq!(received, Err(Error::Reset(Code::H3_REQUEST_CANCELLED)));
    drop(send);
    settled(&peers.budget).await;
    Ok(())
}

#[tokio::test]
async fn protocol_violations_close_the_connection_with_their_code() -> Result<(), TestError> {
    let request = |bytes: Vec<u8>| (Vec::new(), bytes);
    let streams = |streams: Vec<(Vec<u8>, bool)>| (streams, Vec::new());
    let open = |bytes: Vec<u8>| streams(vec![(bytes, false)]);
    let cases = [
        (open([vec![0], frame(0x07, &[4])].concat()), Code::H3_MISSING_SETTINGS),
        (streams(vec![(control(&[]), false); 2]), Code::H3_STREAM_CREATION_ERROR),
        (streams(vec![(control(&[]), true)]), Code::H3_CLOSED_CRITICAL_STREAM),
        (open([control(&[]), frame(0x06, &[0; 8])].concat()), Code::H3_FRAME_UNEXPECTED),
        (open(control(&[(0x21, 0), (0x21, 1)])), Code::H3_SETTINGS_ERROR),
        (open(vec![0x02, 0xc1, 0x01, 0x61]), Code::QPACK_ENCODER_STREAM_ERROR),
        (open(vec![0x03, 0x80]), Code::QPACK_DECODER_STREAM_ERROR),
        (open(vec![0x01, 0x00]), Code::H3_STREAM_CREATION_ERROR),
        // A session ID must be a client-initiated bidirectional stream's.
        (open([varint(0x54), varint(2)].concat()), Code::H3_ID_ERROR),
        (request(frame(0x00, b"body")), Code::H3_FRAME_UNEXPECTED),
        (request([request_head(&[]), frame(0x41, &[])].concat()), Code::H3_FRAME_ERROR),
        (request(frame(0x41, &[0])), Code::H3_ID_ERROR),
        (request(frame(0x01, &[0x01, 0x00, 0xd1])), Code::QPACK_DECOMPRESSION_FAILED),
    ];
    for (index, ((streams, request), code)) in cases.into_iter().enumerate() {
        let Served { peers, serving, .. } = pair(PLAIN).await?.serve(|_, stream| async move {
            let _ = stream.split().1.data().await;
        });
        let mut held = Vec::new();
        for (bytes, finish) in streams {
            held.push(uni(&peers.client, &bytes).await?);
            if finish {
                held.last_mut().expect("opened").finish()?;
            }
        }
        if !request.is_empty() {
            held.push(peers.bi(&request).await?.0);
        }
        assert_eq!(closed_with(&peers.client).await, code, "case {index}");
        assert_eq!(serving.await?, Err(closed(code)));
    }
    Ok(())
}

#[tokio::test]
async fn a_stopped_control_stream_closes_the_connection() -> Result<(), TestError> {
    let critical = Err(closed(Code::H3_CLOSED_CRITICAL_STREAM));
    // The server's control stream is the first stream its peer accepts.
    let Served { peers, serving, .. } = pair(PLAIN).await?.serve(|_, _| async {});
    peers.client.accept_uni().await?.stop(0_u32.into())?;
    assert_eq!(closed_with(&peers.client).await, Code::H3_CLOSED_CRITICAL_STREAM);
    assert_eq!(serving.await?, critical);

    let peers = pair(PLAIN).await?;
    let (driver, _requests) = client(&peers);
    peers.server.accept_uni().await?.stop(0_u32.into())?;
    assert_eq!(closed_with(&peers.server).await, Code::H3_CLOSED_CRITICAL_STREAM);
    assert_eq!(driver.await?, critical);
    Ok(())
}

/// Each role sends exactly its SETTINGS profile on the control stream it opens first.
#[tokio::test]
async fn each_role_sends_its_settings() -> Result<(), TestError> {
    let server = [
        (0x06, 4096),
        (0x08, 1),
        (0x33, 1),
        (0x2b603742, 1),
        (0x2c7cf000, 1),
        (0x14e9cd29, (1 << 62) - 1),
    ];
    let client_settings = [(0x06, 32 * 1024), (0x33, 1), (0x2c7cf000, 1)];
    let Served { peers, .. } = pair(PLAIN).await?.serve(|_, _| async {});
    let mut received = vec![0; control(&server).len()];
    peers.client.accept_uni().await?.read_exact(&mut received).await?;
    assert_eq!(received, control(&server));

    let peers = pair(PLAIN).await?;
    let (_driver, _requests) = client(&peers);
    let mut received = vec![0; control(&client_settings).len()];
    peers.server.accept_uni().await?.read_exact(&mut received).await?;
    assert_eq!(received, control(&client_settings));
    Ok(())
}

/// A peer that grants unidirectional stream credit only after the handshake still gets the SETTINGS.
#[tokio::test]
async fn our_control_stream_opens_once_stream_credit_arrives() -> Result<(), TestError> {
    let setup = Setup { uni_streams: Some(0), ..PLAIN };
    let Served { peers, .. } = pair(setup).await?.serve(|_, _| async {});
    yields().await;
    peers.client.set_max_concurrent_uni_streams(3_u32.into());
    let mut control = tokio::time::timeout(Duration::from_secs(5), peers.client.accept_uni()).await??;
    let mut kind = [0xff];
    control.read_exact(&mut kind).await?;
    assert_eq!(kind, [0x00], "a control stream");
    Ok(())
}

#[tokio::test]
async fn settings_the_server_refuses() -> Result<(), TestError> {
    let many: Vec<_> = (0..65).map(|index| (0x21 + 0x1f * index, 0)).collect();
    let oversized = [vec![0x00], varint(0x04), varint(8 * 1024 + 1)].concat();
    for (bytes, datagrams, code) in [
        (oversized, true, Code::H3_EXCESSIVE_LOAD),
        (control(&many), true, Code::H3_EXCESSIVE_LOAD),
        (control(&many[1..]), true, Code::H3_NO_ERROR),
        (control(&[(0x06, 1), (0x06, 1)]), true, Code::H3_SETTINGS_ERROR),
        // HTTP datagrams need the QUIC datagram transport parameter.
        (control(&[(0x33, 1)]), false, Code::H3_SETTINGS_ERROR),
    ] {
        let Served { peers, stop, .. } = pair(Setup { datagrams, ..PLAIN }).await?.serve(respond);
        let _control = uni(&peers.client, &bytes).await?;
        if code == Code::H3_NO_ERROR {
            peers.get().await?;
            stop.notify_one();
        }
        assert_eq!(closed_with(&peers.client).await, code, "{:x?}", &bytes[..4]);
    }
    Ok(())
}

/// A push the client never allowed closes the connection; five interim heads pass, as quic-go lets them,
/// and a sixth ends the request, as does 101, which HTTP/3 does not have (RFC 9114 §4.5).
#[tokio::test]
async fn responses_the_client_refuses() -> Result<(), TestError> {
    let head = |status: &str| frame(0x01, &section(&[(":status", status)]));
    for (bytes, expected) in [
        (frame(0x05, &[0x00, 0x00, 0x00]), Err(closed(Code::H3_ID_ERROR))),
        ([head("103").repeat(5), head("200")].concat(), Ok(http::StatusCode::OK)),
        (head("100").repeat(6), Err(Error::Protocol(Code::H3_EXCESSIVE_LOAD))),
        (head("101"), Err(Error::Protocol(Code::H3_MESSAGE_ERROR))),
        (frame(0x41, &[]), Err(closed(Code::H3_FRAME_ERROR))),
    ] {
        let peers = pair(PLAIN).await?;
        let (driver, requests) = client(&peers);
        let (_send, mut recv) = requests.send_request(get("/")).await?.split();
        let (mut response, _request) = peers.server.accept_bi().await?;
        response.write_all(&bytes).await?;
        let received = tokio::time::timeout(Duration::from_secs(5), recv.response()).await?;
        assert_eq!(received.map(|response| response.status()), expected);
        match expected {
            Err(Error::Protocol(code)) => assert_eq!(stopped(&response).await, Some(code)),
            Err(error @ Error::Connection { code, .. }) => {
                assert_eq!(closed_with(&peers.server).await, code);
                assert_eq!(driver.await?, Err(error));
            }
            _ => {}
        }
    }
    Ok(())
}

#[tokio::test]
async fn refusals_stay_on_their_stream() -> Result<(), TestError> {
    // Our QPACK: :status 431 as a literal after the static :status name, :status 400 indexed.
    let status_431 = frame(0x01, &[0x00, 0x00, 0x5f, 0x09, 0x03, b'4', b'3', b'1']);
    let status_400 = frame(0x01, &[0x00, 0x00, 0xff, 0x04]);
    let many: Vec<(&str, &str)> = std::iter::repeat_n(("a", ""), 130).collect();
    let lane_cancelled = Code(0x52e4a40fa8db);
    let message_error = Code::H3_MESSAGE_ERROR;
    let excess = [request_head(&[(":method", "POST"), ("content-length", "5")]), frame(0x00, b"abcdefgh")].concat();
    let cases = [
        (
            frame(0x01, &[0; 5000])[..100].to_vec(),
            Ok(status_431.clone()),
            Some(Code::H3_EXCESSIVE_LOAD),
        ),
        (request_head(&many), Ok(status_431), Some(Code::H3_EXCESSIVE_LOAD)),
        ([varint(0x41), varint(0)].concat(), Err(lane_cancelled), Some(lane_cancelled)),
        (excess, Err(Code::H3_REQUEST_CANCELLED), Some(message_error)),
        (request_head(&[("connection", "close")]), Err(message_error), Some(message_error)),
        (request_head(&[(":method", "CONNECT"), (":protocol", "websocket")]), Ok(status_400), None),
    ];
    let Served { peers, .. } = pair(raw(&[])).await?.serve(|_, stream| async move {
        let _ = stream.split().1.data().await;
    });
    for (index, (bytes, response, stop)) in cases.into_iter().enumerate() {
        // Bodies stay open, so each STOP_SENDING arrives before the stream could complete.
        let (send, mut recv) = peers.bi(&bytes).await?;
        let (bytes, end) = raw_stream(&mut recv).await;
        assert_eq!(end.map(|()| bytes), response, "case {index}");
        if stop.is_some() {
            assert_eq!(stopped(&send).await, stop, "case {index}");
        }
    }
    assert!(peers.client.close_reason().is_none(), "refusals leave the connection open");
    settled(&peers.budget).await;
    Ok(())
}

#[tokio::test]
async fn a_budget_refusal_rejects_only_the_new_request() -> Result<(), TestError> {
    let Served { peers, .. } = pair(Setup { limit: 0, ..PLAIN }).await?.serve(respond);
    let (_driver, requests) = client(&peers);
    let (_send, mut recv) = requests.send_request(get("/")).await?.split();
    assert_eq!(recv.response().await.err(), Some(Error::Reset(Code::H3_REQUEST_REJECTED)));
    peers.budget.limit.store(usize::MAX, Ordering::Relaxed);
    let (send, mut recv) = requests.send_request(get("/")).await?.split();
    assert_eq!(recv.response().await?.status(), http::StatusCode::OK);
    drop((send, recv));
    settled(&peers.budget).await;
    Ok(())
}

/// An admitted stream whose head the budget cannot hold, whole or in parts, was never processed: it gets
/// H3_REQUEST_REJECTED both ways, not the 431 of a head over the size limit.
#[tokio::test]
async fn a_head_over_the_budget_rejects_the_admitted_request() -> Result<(), TestError> {
    let head = request_head(&[]);
    // Its section is under 64 bytes, so the frame header takes two.
    let (header, section) = head.split_at(2);
    for part in [section, &section[..1]] {
        let Served { peers, .. } = pair(PLAIN)
            .await?
            .serve(|_, _| async { panic!("a refused head reached its route") });
        let (mut send, mut recv) = peers.bi(header).await?;
        // Admitted: from here the budget holds only the stream's two halves.
        let used = charged(&peers.budget, 0).await;
        peers.budget.limit.store(used, Ordering::Relaxed);
        send.write_all(part).await?;
        assert_eq!(raw_stream(&mut recv).await.1, Err(Code::H3_REQUEST_REJECTED));
        assert_eq!(stopped(&send).await, Some(Code::H3_REQUEST_REJECTED));
    }
    Ok(())
}

#[tokio::test]
async fn goaway_refuses_new_requests_and_closes_after_the_last() -> Result<(), TestError> {
    let (started, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let (running, held) = (started.clone(), release.clone());
    let Served { peers, serving, stop, .. } = pair(PLAIN).await?.serve(move |request, stream| {
        let (running, held) = (running.clone(), held.clone());
        async move {
            running.notify_one();
            held.notified().await;
            respond(request, stream).await;
        }
    });
    let (driver, requests) = client(&peers);
    let (send, mut recv) = requests.send_request(get("/held")).await?.split();
    started.notified().await;
    stop.notify_one();
    until_goaway(&requests).await;
    // A request that ignores the GOAWAY gets H3_REQUEST_REJECTED.
    let mut ignoring = peers.bi(&request_head(&[])).await?;
    assert_eq!(raw_stream(&mut ignoring.1).await.1, Err(Code::H3_REQUEST_REJECTED));
    release.notify_one();
    assert_eq!(recv.response().await?.status(), http::StatusCode::OK);
    drop((send, recv, ignoring));
    assert_eq!((driver.await?, serving.await?), (Ok(()), Ok(())));
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);
    Ok(())
}

#[tokio::test]
async fn deadlines_close_idle_and_draining_connections_and_stale_heads() -> Result<(), TestError> {
    let Served { peers, serving, .. } = pair(PLAIN).await?.serve(|_, _| async {});
    let (driver, _requests) = client(&peers);
    let (send, mut recv) = peers.bi(&frame(0x01, &[0; 10])[..4]).await?;
    // Jump only once the server holds the stream and has started its header deadline.
    charged(&peers.budget, 0).await;
    tokio::task::yield_now().await;
    jump(Duration::from_secs(11)).await;
    assert_eq!(raw_stream(&mut recv).await.1, Err(Code::H3_REQUEST_INCOMPLETE));
    assert_eq!(stopped(&send).await, Some(Code::H3_REQUEST_INCOMPLETE));
    assert!(peers.server.close_reason().is_none(), "the stale head made the connection live");
    jump(Duration::from_secs(16)).await;
    assert_eq!((driver.await?, serving.await?), (Ok(()), Ok(())));
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);

    let held = Arc::new(Notify::new());
    let holding = held.clone();
    let Served { peers, serving, stop, .. } = pair(PLAIN).await?.serve(move |_, stream| {
        let holding = holding.clone();
        async move {
            holding.notify_one();
            std::future::pending::<()>().await;
            drop(stream);
        }
    });
    let (driver, requests) = client(&peers);
    let _held = requests.send_request(get("/held")).await?;
    held.notified().await;
    stop.notify_one();
    until_goaway(&requests).await;
    assert!(
        peers.server.close_reason().is_none(),
        "only the drain closes a connection with a held request"
    );
    jump(Duration::from_secs(6)).await;
    assert_eq!((driver.await?, serving.await?), (Ok(()), Ok(())));
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);
    Ok(())
}

/// A request that ends before the driver's next pass still restarts the 15 s idle period.
#[tokio::test]
async fn a_request_finished_between_passes_restarts_the_idle_period() -> Result<(), TestError> {
    let peers = pair(PLAIN).await?;
    let mut connection = server::Connection::new(peers.server.clone(), Some(peers.budget.clone()));
    // One pass starts the idle period; the connection then waits unpolled.
    let waiting = poll_fn(|cx| Poll::Ready(pin!(connection.next()).poll(cx).is_pending())).await;
    assert!(waiting);
    jump(Duration::from_secs(10)).await;
    let (mut send, mut recv) = peers.bi(&request_head(&[])).await?;
    send.finish()?;
    let (request, stream) = connection.next().await?.ok_or("a request")?.resolve().await?;
    respond(request, stream).await;
    assert_eq!(raw_stream(&mut recv).await.1, Ok(()));
    let serving = tokio::spawn(async move {
        while connection.next().await?.is_some() {}
        Ok::<_, Error>(())
    });
    yields().await;
    jump(Duration::from_secs(14)).await;
    yields().await;
    assert!(peers.server.close_reason().is_none(), "closed within 15 s of the request");
    jump(Duration::from_secs(2)).await;
    assert_eq!(serving.await?, Ok(()));
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);
    Ok(())
}

#[tokio::test]
async fn a_peer_stream_declares_its_type_within_10_seconds() -> Result<(), TestError> {
    let Served { peers, .. } = pair(PLAIN).await?.serve(|_, _| async {});
    // Streams of an unknown type are stopped unread; these three take the floor kept for critical ones.
    for _ in 0..3 {
        let unknown = uni(&peers.client, &varint(0x21)).await?;
        assert_eq!(stopped(&unknown).await, Some(Code::H3_STREAM_CREATION_ERROR));
    }
    // The first byte of a two-byte type; the stream is charged once the server holds it.
    let silent = uni(&peers.client, &[0x40]).await?;
    charged(&peers.budget, 0).await;
    jump(Duration::from_secs(9)).await;
    assert!(!stopped_yet(&silent).await, "stopped before the deadline");
    jump(Duration::from_secs(2)).await;
    assert_eq!(stopped(&silent).await, Some(Code::H3_STREAM_CREATION_ERROR));
    assert!(peers.client.close_reason().is_none());
    Ok(())
}

/// A download the client cancels is reset with plain RESET_STREAM when the client does not offer reliable
/// reset, and the connection carries the next request.
#[tokio::test]
async fn a_cancelled_download_without_reliable_reset_leaves_the_connection_usable() -> Result<(), TestError> {
    let setup = Setup { reliable_reset: false, ..PLAIN };
    let Served { peers, .. } = pair(setup).await?.serve(|request, stream| async move {
        let mut send = stream.split().0;
        send.send_response(http::Response::new(())).await.unwrap();
        if request.uri().path() == "/download" {
            while send.send_data(Bytes::from(vec![7; 64 * 1024])).await.is_ok() {}
        } else {
            send.finish().await.unwrap();
        }
    });
    let (_driver, requests) = client(&peers);
    for _ in 0..2 {
        let (send, mut recv) = requests.send_request(get("/download")).await?.split();
        recv.response().await?;
        assert!(recv.data().await?.is_some());
        drop((send, recv));
    }
    let (send, mut recv) = requests.send_request(get("/")).await?.split();
    assert_eq!(recv.response().await?.status(), http::StatusCode::OK);
    assert!(peers.client.close_reason().is_none());
    drop((send, recv));
    settled(&peers.budget).await;
    Ok(())
}
