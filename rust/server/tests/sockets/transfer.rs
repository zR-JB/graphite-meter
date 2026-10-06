//! Downloads, uploads and progress feeds over HTTP/1 with their endings.

use super::*;

#[tokio::test]
async fn downloads_and_uploads_keep_the_connection() {
    let server = start(&[]).await;
    let mut client = server.connect().await;
    let answer = client.request("GET /download?bytes=1000000", "").await;
    assert_eq!((answer.status, answer.body.len()), (200, 1_000_000));
    assert_eq!(answer.header("content-type"), Some("application/octet-stream"));
    let answer = client.request("HEAD /download?bytes=5", "").await;
    assert_eq!(answer.header("content-length"), Some("5"), "HEAD keeps its length");
    assert!(answer.body.is_empty());
    assert_eq!(client.request("GET /download?bytes=0", "").await.body.len(), 0);
    let id = server.upload_id().await;
    let answer = client
        .request(&format!("POST /upload?id={id}"), &"x".repeat(100_000))
        .await;
    assert_eq!((answer.status, answer.body.as_slice()), (200, &br#"{"bytes":100000}"#[..]));
    let answer = client.request(&format!("POST /upload/checkpoint?id={id}"), "").await;
    let counters: serde_json::Value = serde_json::from_slice(&answer.body).unwrap();
    assert_eq!(counters["bytes"], 100_000);
    assert_eq!(server.active().await, 0, "finished transfers release their handlers");
}

#[tokio::test]
async fn transfers_the_peer_leaves_idle_end_after_thirty_seconds() {
    let server = start(&[]).await;
    let id = server.upload_id().await;
    let mut download = server.connect().await;
    download
        .send(&format!("GET {ENDLESS} HTTP/1.1\r\nHost: test\r\n\r\n"))
        .await;
    download.head().await.unwrap();
    let mut upload = server.connect().await;
    upload
        .send(&format!("POST /upload?id={id} HTTP/1.1\r\nHost: test\r\nContent-Length: 10\r\n\r\n"))
        .await;
    server.until_active(2).await;
    advance_clock(Duration::from_secs(29)).await;
    assert_eq!(server.active().await, 2, "a blocked writer keeps its lane until the idle bound");
    advance_clock(Duration::from_secs(2)).await;
    assert!(download.drain().await < 64 << 20, "the connection closed with the socket buffers left");
    let answer = upload.answer().await.unwrap();
    assert_eq!((answer.status, answer.header("x-graphite-upload-refusal")), (408, Some("idle")));
    server.until_active(0).await;
}

#[tokio::test]
async fn a_progress_feed_streams_records_until_the_upload_completes() {
    let server = start(&[]).await;
    let id = server.upload_id().await;
    let mut feed = server.connect().await;
    feed.send(&format!("GET /upload/progress?id={id} HTTP/1.1\r\nHost: test\r\n\r\n"))
        .await;
    let head = feed.head().await.unwrap();
    assert_eq!(head.header("content-type"), Some("application/x-ndjson"));
    assert!(
        String::from_utf8(feed.chunk().await.unwrap())
            .unwrap()
            .contains(r#""type":"ready""#)
    );
    let mut upload = server.connect().await;
    upload.request(&format!("POST /upload?id={id}"), "12345").await;
    let progress = loop {
        let line = String::from_utf8(feed.chunk().await.unwrap()).unwrap();
        if line.contains(r#""type":"progress""#) {
            break line;
        }
    };
    assert!(progress.contains(r#""bytes":5"#), "{progress}");
    assert_eq!(
        upload
            .request(&format!("DELETE /upload/progress?id={id}"), "")
            .await
            .status,
        204
    );
    let complete = loop {
        let line = String::from_utf8(feed.chunk().await.expect("the feed ends after complete")).unwrap();
        if line.contains(r#""type":"complete""#) {
            break line;
        }
    };
    assert!(complete.contains(r#""bytes":5"#), "{complete}");
    assert!(feed.chunk().await.is_none(), "the feed ends");
    let answer = feed.request("GET /probe", "").await;
    assert_eq!(answer.status, 200, "the connection serves its next request");
}
