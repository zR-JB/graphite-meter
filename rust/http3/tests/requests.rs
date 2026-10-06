//! The connection driver and request streams over loopback QUIC.
mod support;

use bytes::Bytes;
use graphite_meter_http3::{Code, Error, server};
use std::{
    future::{Future, poll_fn},
    pin::pin,
    sync::atomic::Ordering,
    task::Poll,
    time::Duration,
};
use support::*;

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
        (open(vec![0x02, 0xc1, 0x01, 0x61]), Code::QPACK_ENCODER_STREAM_ERROR),
        // A session ID must be a client-initiated bidirectional stream's.
        (open([varint(0x54), varint(2)].concat()), Code::H3_ID_ERROR),
        (request(frame(0x00, b"body")), Code::H3_FRAME_UNEXPECTED),
        (request([request_head(&[]), frame(0x41, &[])].concat()), Code::H3_FRAME_ERROR),
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
async fn refusals_stay_on_their_stream() -> Result<(), TestError> {
    // Our QPACK: :status 431 as a literal after the static :status name, :status 400 indexed.
    let status_431 = frame(0x01, &[0x00, 0x00, 0x5f, 0x09, 0x03, b'4', b'3', b'1']);
    let status_400 = frame(0x01, &[0x00, 0x00, 0xff, 0x04]);
    let lane_cancelled = Code(0x52e4a40fa8db);
    let message_error = Code::H3_MESSAGE_ERROR;
    let excess = [request_head(&[(":method", "POST"), ("content-length", "5")]), frame(0x00, b"abcdefgh")].concat();
    let cases = [
        (frame(0x01, &[0; 5000])[..100].to_vec(), Ok(status_431), Some(Code::H3_EXCESSIVE_LOAD)),
        ([varint(0x41), varint(0)].concat(), Err(lane_cancelled), Some(lane_cancelled)),
        (excess, Err(Code::H3_REQUEST_CANCELLED), Some(message_error)),
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

#[tokio::test]
async fn deadlines_close_idle_connections_and_stale_heads() -> Result<(), TestError> {
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
