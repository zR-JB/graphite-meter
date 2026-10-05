//! Uploads, their refusals and endings, checkpoints and progress feeds.

use super::*;
use futures_util::{Stream, StreamExt, stream};
use graphite_meter_proto::upload::{Record, Session};
use http::StatusCode;
use http_body::Frame;
use http_body_util::StreamBody;
use std::time::Duration;
use tokio::time::{Instant, sleep, timeout};

type Frames = StreamBody<std::pin::Pin<Box<dyn Stream<Item = Result<Frame<Bytes>, Infallible>> + Send>>>;

/// An upload body of `chunks`, then an end, or a stall when `stalls`.
fn chunks(chunks: &'static [&'static [u8]], stalls: bool) -> Frames {
    let data = stream::iter(chunks.iter().map(|chunk| Ok(Frame::data(Bytes::from_static(chunk)))));
    let rest = if stalls { stream::pending().boxed() } else { stream::empty().boxed() };
    StreamBody::new(data.chain(rest).boxed())
}

/// An upload body sending one byte every ten seconds without end.
fn trickle() -> Frames {
    let bytes = stream::repeat(()).then(|()| async {
        sleep(Duration::from_secs(10)).await;
        Ok(Frame::data(Bytes::from_static(b"x")))
    });
    StreamBody::new(bytes.boxed())
}

async fn mint(app: &App) -> String {
    let response = send(app, Endpoint::H1, empty(request("POST", "/upload/session"))).await;
    assert_eq!(header(&response, "cache-control"), Some("no-store"));
    Session::decode(text(response).await.as_bytes()).unwrap().upload_id
}

async fn checkpoint(app: &App, id: &str) -> Value {
    let response = send(app, Endpoint::H2, empty(request("POST", &format!("/upload/checkpoint?id={id}")))).await;
    assert_eq!(header(&response, "cache-control"), Some("no-store"));
    json(response).await
}

#[tokio::test]
async fn an_upload_counts_every_byte_into_its_aggregate_and_answers_its_own_total() {
    let app = app(&ALL_LISTENERS);
    let id = mint(&app).await;
    let upload = |body| request("POST", &format!("/upload?id={id}")).body(body).unwrap();
    let response = send(&app, Endpoint::H1, upload(chunks(&[b"abc", b"", b"defgh"], false))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header(&response, "cache-control"), Some("no-store"));
    assert_eq!(text(response).await, r#"{"bytes":8}"#);
    let response = send(&app, Endpoint::Quic, upload(chunks(&[b"12"], false))).await;
    assert_eq!(text(response).await, r#"{"bytes":2}"#, "a reply counts its own bytes");
    assert_eq!(checkpoint(&app, &id).await["bytes"], 10, "lanes of one ID feed one aggregate");
    assert_eq!(active(&app).await, 0);
}

#[tokio::test]
async fn an_upload_without_its_own_valid_id_is_refused_before_its_body_is_read() {
    let app = app(&ALL_LISTENERS);
    let id = mint(&app).await;
    let opened = send(
        &app,
        Endpoint::H1,
        request("POST", &format!("/upload?id={id}"))
            .body(chunks(&[], false))
            .unwrap(),
    );
    assert_eq!(text(opened.await).await, r#"{"bytes":0}"#);
    let foreign = format!("/upload?id={id}");
    for (uri, peer, status, name) in [
        ("/upload", "192.0.2.1", StatusCode::BAD_REQUEST, "invalid"),
        ("/upload?id=gmu_forged", "192.0.2.1", StatusCode::BAD_REQUEST, "invalid"),
        (foreign.as_str(), "192.0.2.9", StatusCode::FORBIDDEN, "ownerMismatch"),
    ] {
        let unread = request("POST", uri).body(Unending).unwrap();
        let response = timeout(Duration::from_secs(5), send_from(&app, Endpoint::H1, peer, unread))
            .await
            .unwrap();
        assert_eq!((response.status(), header(&response, "x-graphite-upload-refusal")), (status, Some(name)));
        let message = if name == "invalid" {
            "unknown upload id"
        } else {
            "upload id belongs to another client"
        };
        assert_eq!(text(response).await, format!("{message}\n"));
    }
    assert_eq!(checkpoint(&app, &id).await["bytes"], 0);
}

#[tokio::test(start_paused = true)]
async fn an_upload_that_stops_sending_is_answered_idle_and_keeps_its_bytes() {
    let app = app(&ALL_LISTENERS);
    let id = mint(&app).await;
    let start = Instant::now();
    let stalled = request("POST", &format!("/upload?id={id}"))
        .body(chunks(&[b"0123456789"], true))
        .unwrap();
    let response = send(&app, Endpoint::H1, stalled).await;
    assert_eq!(start.elapsed(), Duration::from_secs(30));
    assert_eq!(
        (response.status(), header(&response, "x-graphite-upload-refusal")),
        (StatusCode::REQUEST_TIMEOUT, Some("idle"))
    );
    assert_eq!(text(response).await, "idle\n");
    assert_eq!(checkpoint(&app, &id).await["bytes"], 10);
}

#[tokio::test(start_paused = true)]
async fn an_upload_reaching_its_lifetime_or_shutdown_closes_without_an_answer() {
    let app = app(&[&ALL_LISTENERS[..], &[("GM_MAX_OPERATION_DURATION", "45s")]].concat());
    let id = mint(&app).await;
    let start = Instant::now();
    let upload = || request("POST", &format!("/upload?id={id}")).body(trickle()).unwrap();
    assert!(matches!(outcome(&app, Endpoint::H1, "192.0.2.1", upload()).await, Outcome::Abort));
    assert_eq!(start.elapsed(), Duration::from_secs(45));
    assert_eq!(checkpoint(&app, &id).await["bytes"], 4, "bytes count in every ending");

    let shutdown = CancellationToken::new();
    let app = App::new(config(&ALL_LISTENERS), shutdown.clone()).unwrap();
    let id = mint(&app).await;
    tokio::spawn(async move {
        sleep(Duration::from_secs(25)).await;
        shutdown.cancel();
    });
    let upload = request("POST", &format!("/upload?id={id}")).body(trickle()).unwrap();
    assert!(matches!(outcome(&app, Endpoint::H1, "192.0.2.1", upload).await, Outcome::Abort));
    assert_eq!(checkpoint(&app, &id).await["bytes"], 2);
}

#[tokio::test]
async fn an_upload_whose_data_never_pauses_still_sees_its_lane_end() {
    let shutdown = CancellationToken::new();
    let app = App::new(config(&[]), shutdown.clone()).unwrap();
    let id = mint(&app).await;
    shutdown.cancel();
    let ready = stream::iter((0..10_000).map(|_| Ok::<_, Infallible>(Frame::data(Bytes::from_static(b"x")))));
    let upload = request("POST", &format!("/upload?id={id}"))
        .body(StreamBody::new(ready.boxed()))
        .unwrap();
    assert!(matches!(outcome(&app, Endpoint::H1, "192.0.2.1", upload).await, Outcome::Abort));
    assert_eq!(
        checkpoint(&app, &id).await["bytes"],
        1,
        "the first chunk counts, then the shutdown ends it"
    );
}

/// The record a feed line holds, `None` for a heartbeat.
fn record(line: &[u8]) -> Option<Record> {
    let line = line.strip_suffix(b"\n").expect("one line");
    (!line.is_empty()).then(|| Record::decode(line).unwrap())
}

#[tokio::test(start_paused = true)]
async fn a_progress_feed_attaches_reports_and_completes_after_finalization() {
    let app = app(&ALL_LISTENERS);
    let id = mint(&app).await;
    let progress = |method, peer| {
        send_from(&app, Endpoint::H2, peer, empty(request(method, &format!("/upload/progress?id={id}"))))
    };
    let replaced = progress("GET", "192.0.2.1").await;
    let mut replaced = replaced.into_body();
    assert_eq!(
        record(&replaced.frame().await.unwrap().unwrap().into_data().unwrap()),
        Some(Record::Ready)
    );
    let feed = progress("GET", "192.0.2.1").await;
    assert!(replaced.frame().await.is_none(), "a newer reader ends the older feed");
    drop(replaced);
    let headers = ["content-type", "cache-control", "x-accel-buffering"].map(|name| header(&feed, name));
    assert_eq!(headers, [Some("application/x-ndjson"), Some("no-store, no-transform"), Some("no")]);
    assert_eq!(active(&app).await, 1, "a feed holds a handler");

    let upload = request("POST", &format!("/upload?id={id}"))
        .body(chunks(&[b"hello"], false))
        .unwrap();
    assert_eq!(text(send(&app, Endpoint::H1, upload).await).await, r#"{"bytes":5}"#);
    assert_eq!(progress("HEAD", "192.0.2.1").await.headers()["allow"], "GET, DELETE");
    assert_eq!(progress("DELETE", "192.0.2.9").await.status(), StatusCode::FORBIDDEN);
    assert_eq!(progress("DELETE", "192.0.2.1").await.status(), StatusCode::NO_CONTENT);
    let lines = feed.into_body().collect().await.unwrap().to_bytes();
    let records: Vec<_> = lines
        .split_inclusive(|byte| *byte == b'\n')
        .filter_map(record)
        .collect();
    assert_eq!(records.first(), Some(&Record::Ready));
    assert!(matches!(records.last(), Some(Record::Complete(counters)) if counters.bytes() == 5));
    assert!(
        records[1..records.len() - 1]
            .iter()
            .all(|record| matches!(record, Record::Progress(_)))
    );
    assert_eq!(active(&app).await, 0);
    let unknown = format!("/upload/progress?id={}", mint(&app).await);
    let response = send(&app, Endpoint::H1, empty(request("DELETE", &unknown))).await;
    assert_eq!(header(&response, "x-graphite-upload-refusal"), Some("invalid"));
}

#[tokio::test]
async fn a_checkpoint_reads_only_an_existing_aggregate_of_its_owner() {
    let app = app(&ALL_LISTENERS);
    let id = mint(&app).await;
    let read =
        |peer| send_from(&app, Endpoint::H1, peer, empty(request("POST", &format!("/upload/checkpoint?id={id}"))));
    let response = read("192.0.2.1").await;
    assert_eq!(
        header(&response, "x-graphite-upload-refusal"),
        Some("invalid"),
        "a checkpoint creates nothing"
    );
    let upload = request("POST", &format!("/upload?id={id}"))
        .body(chunks(&[b"abcd"], false))
        .unwrap();
    send(&app, Endpoint::H1, upload).await;
    let counters = json(read("192.0.2.1").await).await;
    assert_eq!(counters["bytes"], 4);
    assert_eq!(counters.as_object().unwrap().len(), 2);
    assert!(counters["nanos"].is_u64());
    assert_eq!(read("192.0.2.9").await.status(), StatusCode::FORBIDDEN);
}
