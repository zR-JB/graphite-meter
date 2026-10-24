//! Tests uploads into their aggregate and checkpoints.

use super::*;
use futures_util::{Stream, StreamExt, stream};
use http::StatusCode;
use http_body::Frame;
use http_body_util::StreamBody;

type Frames =
    StreamBody<std::pin::Pin<Box<dyn Stream<Item = Result<Frame<Bytes>, Infallible>> + Send>>>;

/// Builds an upload body of `chunks`, then an end, or a stall when `stalls`.
fn chunks(chunks: &'static [&'static [u8]], stalls: bool) -> Frames {
    let data = stream::iter(
        chunks
            .iter()
            .map(|chunk| Ok(Frame::data(Bytes::from_static(chunk)))),
    );
    let rest = if stalls {
        stream::pending().boxed()
    } else {
        stream::empty().boxed()
    };
    StreamBody::new(data.chain(rest).boxed())
}

async fn checkpoint(app: &App, id: &str) -> Value {
    let response = send(
        app,
        Endpoint::H2,
        empty(request("POST", &format!("/upload/checkpoint?id={id}"))),
    )
    .await;
    assert_eq!(header(&response, "cache-control"), Some("no-store"));
    json(response).await
}

#[tokio::test]
async fn an_upload_counts_every_byte_into_its_aggregate_and_answers_its_own_total() {
    let app = app(&ALL_LISTENERS);
    let id = mint(&app).await;
    let unread = send(
        &app,
        Endpoint::H2,
        empty(request("POST", &format!("/upload/checkpoint?id={id}"))),
    )
    .await;
    assert_eq!(
        header(&unread, "graphite-meter-refusal"),
        Some("invalid"),
        "a checkpoint creates nothing"
    );
    let upload = |body| {
        request("POST", &format!("/upload?id={id}"))
            .body(body)
            .unwrap()
    };
    let response = send(
        &app,
        Endpoint::H1,
        upload(chunks(&[b"abc", b"", b"defgh"], false)),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header(&response, "cache-control"), Some("no-store"));
    assert_eq!(text(response).await, r#"{"bytes":8}"#);
    let response = send(&app, Endpoint::Quic, upload(chunks(&[b"12"], false))).await;
    assert_eq!(
        text(response).await,
        r#"{"bytes":2}"#,
        "a reply counts its own bytes"
    );
    let counters = checkpoint(&app, &id).await;
    assert_eq!(counters["bytes"], 10, "lanes of one ID feed one aggregate");
    assert!(
        counters.as_object().unwrap().len() == 2 && counters["nanos"].is_u64(),
        "{counters}"
    );
    assert_eq!(active(&app).await, 0);
}

#[tokio::test]
async fn an_upload_whose_data_never_pauses_still_sees_its_lane_end() {
    let shutdown = CancellationToken::new();
    let app = App::new(config(&[]), shutdown.clone()).unwrap();
    let id = mint(&app).await;
    shutdown.cancel();
    let ready = stream::iter(
        (0..10_000).map(|_| Ok::<_, Infallible>(Frame::data(Bytes::from_static(b"x")))),
    );
    let upload = request("POST", &format!("/upload?id={id}"))
        .body(StreamBody::new(ready.boxed()))
        .unwrap();
    assert!(matches!(
        outcome(&app, Endpoint::H1, "192.0.2.1", upload).await,
        Outcome::Abort
    ));
    assert_eq!(
        checkpoint(&app, &id).await["bytes"],
        32,
        "the first batch of 32 ready frames counts, then the shutdown ends it"
    );
}

fn leased(method: &str, path: &str, token: &str) -> Builder {
    request(method, path).header("graphite-meter-lease", token)
}

#[tokio::test]
async fn an_upload_follows_its_lease_to_any_origin_and_address() {
    let app = app(&ALL_LISTENERS);
    let body = serde_json::json!({"measurements": 4, "sessions": 0, "uploads": 1});
    let lease = json(take_lease(&app, "192.0.2.1", request("POST", "/lease"), body).await).await;
    let (token, id) = (
        lease["token"].as_str().unwrap(),
        lease["uploads"][0].as_str().unwrap(),
    );
    // The transfer origin reaches the server over another address than the discovery origin.
    let path = format!("/upload?id={id}");
    let posted = leased("POST", &path, token).body(chunks(&[b"abc"], false));
    let posted = send_from(&app, Endpoint::Quic, "198.51.100.1", posted.unwrap()).await;
    assert_eq!(text(posted).await, r#"{"bytes":3}"#);
    let checkpoint = format!("/upload/checkpoint?id={id}");
    let read = send(
        &app,
        Endpoint::H1,
        empty(leased("POST", &checkpoint, token)),
    )
    .await;
    assert_eq!(json(read).await["bytes"], 3);
    for (peer, unleased) in [("192.0.2.1", &checkpoint), ("198.51.100.1", &path)] {
        let refused = send_from(&app, Endpoint::H1, peer, empty(request("POST", unleased))).await;
        assert_eq!(
            header(&refused, "graphite-meter-refusal"),
            Some("owner-mismatch"),
            "{peer} without the lease"
        );
    }
    let feed = format!("/upload/progress?id={id}");
    let finished = send_from(
        &app,
        Endpoint::H1,
        "2001:db8::1",
        empty(leased("DELETE", &feed, token)),
    )
    .await;
    assert_eq!(finished.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn a_lease_admits_its_grant_then_charges_its_client_and_ends_on_delete() {
    let app = app(&[
        ALL_LISTENERS.as_slice(),
        &[
            ("GM_MAX_ACTIVE_MEASUREMENTS", "4"),
            ("GM_MAX_ACTIVE_MEASUREMENTS_PER_CLIENT", "3"),
            ("GM_MAX_ACTIVE_SESSIONS", "4"),
            ("GM_MAX_SESSIONS_PER_CLIENT", "3"),
        ],
    ]
    .concat());
    let ask = |peer: &'static str, measurements: u32| {
        let body = serde_json::json!({"measurements": measurements, "sessions": 0});
        take_lease(&app, peer, request("POST", "/lease"), body)
    };
    let clamped = json(ask("203.0.113.9", 9).await).await;
    assert_eq!(clamped["measurements"], 3, "a client's share at most");
    let token = clamped["token"].as_str().unwrap().to_owned();
    send(
        &app,
        Endpoint::H1,
        empty(leased("DELETE", "/lease", &token)),
    )
    .await;
    assert_eq!(active(&app).await, 0, "an ended lease holds nothing");
    let lease = json(ask("192.0.2.1", 2).await).await;
    let token = lease["token"].as_str().unwrap().to_owned();
    assert_eq!(active(&app).await, 2, "held before anything is measured");
    let download = || leased("GET", "/download?bytes=100000", &token);
    let mut held = Vec::new();
    for _ in 0..3 {
        let admitted = send(&app, Endpoint::H2, empty(download())).await;
        assert_eq!(admitted.status(), StatusCode::OK);
        held.push(admitted);
    }
    assert_eq!(
        active(&app).await,
        3,
        "the third past the grant, from the client's share"
    );
    let limited = send(&app, Endpoint::H2, empty(download())).await;
    assert_eq!(
        header(&limited, "graphite-meter-refusal"),
        Some("client-limit")
    );
    let busy = ask("192.0.2.2", 2).await;
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        header(&busy, "graphite-meter-refusal"),
        Some("server-capacity")
    );
    assert_eq!(header(&busy, "retry-after"), Some("1"));
    assert_eq!(active(&app).await, 3, "a refused lease holds nothing");
    drop(held);
    let other = json(ask("192.0.2.2", 2).await).await;
    let raise = leased("POST", "/lease", &token).body(http_body_util::Full::new(Bytes::from(
        r#"{"measurements":3,"sessions":0}"#,
    )));
    let refused = send(&app, Endpoint::H1, raise.unwrap()).await;
    assert_eq!(
        header(&refused, "graphite-meter-refusal"),
        Some("server-capacity")
    );
    assert_eq!(
        active(&app).await,
        4,
        "a refused raise keeps what the lease held"
    );
    let other = other["token"].as_str().unwrap();
    send(&app, Endpoint::H1, empty(leased("DELETE", "/lease", other))).await;
    let resized = leased("POST", "/lease", &token).body(http_body_util::Full::new(Bytes::from(
        r#"{"measurements":1,"sessions":0,"uploads":2}"#,
    )));
    let resized = json(send(&app, Endpoint::H1, resized.unwrap()).await).await;
    assert_eq!(
        (
            resized["token"].as_str(),
            resized["uploads"].as_array().unwrap().len()
        ),
        (Some(token.as_str()), 2)
    );
    assert_eq!(active(&app).await, 1);
    let ended = send(
        &app,
        Endpoint::H1,
        empty(leased("DELETE", "/lease", &token)),
    )
    .await;
    assert_eq!(ended.status(), StatusCode::NO_CONTENT);
    let mut stale = Vec::new();
    for token in [token.as_str(), "gml_unknown"] {
        let served = send(
            &app,
            Endpoint::H2,
            empty(leased("GET", "/download?bytes=100000", token)),
        )
        .await;
        assert_eq!(served.status(), StatusCode::OK, "served as without a lease");
        stale.push(served);
    }
    assert_eq!(active(&app).await, 2, "charged to the client's share");
    drop(stale);
    assert_eq!(active(&app).await, 0);
}

#[tokio::test]
async fn a_lease_admits_its_sessions_from_its_url_and_past_them_charges_the_client() {
    let app = app(&[
        ALL_LISTENERS.as_slice(),
        &[("GM_MAX_SESSIONS_PER_CLIENT", "2")],
    ]
    .concat());
    let body = serde_json::json!({"measurements": 3, "sessions": 1});
    let lease = json(take_lease(&app, "192.0.2.1", request("POST", "/lease"), body).await).await;
    assert_eq!(
        (&lease["measurements"], &lease["sessions"]),
        (&3.into(), &1.into())
    );
    let path = format!("/wt/download?lease={}", lease["token"].as_str().unwrap());
    let connect = || {
        outcome(
            &app,
            Endpoint::Quic,
            "198.51.100.1",
            empty(request("CONNECT", &path)),
        )
    };
    let (Outcome::WebTransport(_, first, _), Outcome::WebTransport(_, past, _)) =
        (connect().await, connect().await)
    else {
        panic!("its session, then one past it from the client's share");
    };
    assert_eq!(active(&app).await, 4, "three held, one past the grant");
    let Outcome::Response(refused) = connect().await else {
        panic!("a refusal");
    };
    assert_eq!(
        header(&refused, "graphite-meter-refusal"),
        Some("client-limit")
    );
    drop((first, past));
    assert_eq!(active(&app).await, 3);
}
