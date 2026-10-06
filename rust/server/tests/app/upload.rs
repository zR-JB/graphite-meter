//! Uploads into their aggregate and checkpoints.

use super::*;
use futures_util::{Stream, StreamExt, stream};
use graphite_meter_proto::upload::Session;
use http::StatusCode;
use http_body::Frame;
use http_body_util::StreamBody;

type Frames = StreamBody<std::pin::Pin<Box<dyn Stream<Item = Result<Frame<Bytes>, Infallible>> + Send>>>;

/// An upload body of `chunks`, then an end, or a stall when `stalls`.
fn chunks(chunks: &'static [&'static [u8]], stalls: bool) -> Frames {
    let data = stream::iter(chunks.iter().map(|chunk| Ok(Frame::data(Bytes::from_static(chunk)))));
    let rest = if stalls { stream::pending().boxed() } else { stream::empty().boxed() };
    StreamBody::new(data.chain(rest).boxed())
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
    let unread = send(&app, Endpoint::H2, empty(request("POST", &format!("/upload/checkpoint?id={id}")))).await;
    assert_eq!(
        header(&unread, "x-graphite-upload-refusal"),
        Some("invalid"),
        "a checkpoint creates nothing"
    );
    let upload = |body| request("POST", &format!("/upload?id={id}")).body(body).unwrap();
    let response = send(&app, Endpoint::H1, upload(chunks(&[b"abc", b"", b"defgh"], false))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header(&response, "cache-control"), Some("no-store"));
    assert_eq!(text(response).await, r#"{"bytes":8}"#);
    let response = send(&app, Endpoint::Quic, upload(chunks(&[b"12"], false))).await;
    assert_eq!(text(response).await, r#"{"bytes":2}"#, "a reply counts its own bytes");
    let counters = checkpoint(&app, &id).await;
    assert_eq!(counters["bytes"], 10, "lanes of one ID feed one aggregate");
    assert!(counters.as_object().unwrap().len() == 2 && counters["nanos"].is_u64(), "{counters}");
    assert_eq!(active(&app).await, 0);
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
