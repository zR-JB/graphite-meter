//! Downloads, uploads and progress feeds over HTTP/1 with their endings.

use super::*;
use tokio::time::{Instant, sleep};

#[tokio::test]
async fn a_download_sends_its_length_and_keeps_the_connection() {
    let server = start(&[]).await;
    let mut client = server.connect().await;
    let answer = client.request("GET /download?bytes=1000000", "").await;
    assert_eq!((answer.status, answer.body.len()), (200, 1_000_000));
    assert_eq!(answer.header("content-type"), Some("application/octet-stream"));
    let answer = client.request("HEAD /download?bytes=5", "").await;
    assert_eq!(answer.header("content-length"), Some("5"), "HEAD keeps its length");
    assert!(answer.body.is_empty());
    assert_eq!(client.request("GET /download?bytes=0", "").await.body.len(), 0);
    assert_eq!(server.active().await, 0, "finished downloads release their handlers");
}

#[tokio::test]
async fn a_download_ends_at_the_operation_lifetime() {
    let server = start(&[("GM_MAX_OPERATION_DURATION", "1s")]).await;
    let mut client = server.connect().await;
    let started = Instant::now();
    client
        .send("GET /download?bytes=68719476736 HTTP/1.1\r\nHost: test\r\n\r\n")
        .await;
    let head = client.head().await.unwrap();
    assert_eq!(head.header("content-length"), Some("68719476736"));
    let mut buffer = vec![0; 16 << 10];
    while client.stream.read(&mut buffer).await.unwrap() > 0 {
        sleep(Duration::from_millis(2)).await;
    }
    // The next ending, the 30 s idle bound, is far off, so ending soon after 1 s is the lifetime.
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(10), "{elapsed:?}");
    server.until_active(0).await;
}

#[tokio::test]
async fn a_download_the_peer_stops_draining_closes_after_the_idle_bound() {
    let server = start(&[]).await;
    let mut client = server.connect().await;
    client
        .send("GET /download?bytes=68719476736 HTTP/1.1\r\nHost: test\r\n\r\n")
        .await;
    client.head().await.unwrap();
    tokio::time::pause();
    sleep(Duration::from_secs(29)).await;
    tokio::time::resume();
    assert_eq!(server.active().await, 1, "a blocked writer keeps its lane until the idle bound");
    tokio::time::pause();
    sleep(Duration::from_secs(2)).await;
    tokio::time::resume();
    assert!(client.drain().await < 64 << 20, "the connection closed with the socket buffers left");
    server.until_active(0).await;
}

#[tokio::test]
async fn shutdown_ends_running_downloads_and_uploads_at_once() {
    let server = start(&[]).await;
    let id = server.upload_id().await;
    let mut download = server.connect().await;
    download
        .send("GET /download?bytes=68719476736 HTTP/1.1\r\nHost: test\r\n\r\n")
        .await;
    download.head().await.unwrap();
    let mut upload = server.connect().await;
    upload
        .send(&format!(
            "POST /upload?id={id} HTTP/1.1\r\nHost: test\r\nContent-Length: 1000\r\n\r\npartial"
        ))
        .await;
    server.until_active(2).await;
    let stopped = server.stop();
    let bound = Duration::from_secs(2);
    tokio::time::timeout(bound, download.drain())
        .await
        .expect("the download ends");
    let answer = tokio::time::timeout(bound, upload.answer()).await.unwrap();
    assert!(answer.is_none(), "an upload ending with shutdown closes unanswered");
    let stopped = tokio::time::timeout(bound, stopped).await;
    stopped
        .expect("the server stops before its drain grace")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_finished_upload_answers_its_byte_count() {
    let server = start(&[]).await;
    let id = server.upload_id().await;
    let mut client = server.connect().await;
    let answer = client
        .request(&format!("POST /upload?id={id}"), &"x".repeat(100_000))
        .await;
    assert_eq!((answer.status, answer.body.as_slice()), (200, &br#"{"bytes":100000}"#[..]));
    let answer = client.request(&format!("POST /upload/checkpoint?id={id}"), "").await;
    let counters: serde_json::Value = serde_json::from_slice(&answer.body).unwrap();
    assert_eq!(counters["bytes"], 100_000, "the connection carries on after the upload");
}

#[tokio::test]
async fn an_upload_without_bytes_for_the_idle_bound_is_refused_idle() {
    let server = start(&[]).await;
    let id = server.upload_id().await;
    let mut client = server.connect().await;
    client
        .send(&format!("POST /upload?id={id} HTTP/1.1\r\nHost: test\r\nContent-Length: 10\r\n\r\n"))
        .await;
    server.until_active(1).await;
    tokio::time::pause();
    let started = Instant::now();
    let answer = client.answer().await.unwrap();
    let elapsed = started.elapsed();
    // Paused time may run ahead while the answer is in flight; the contract allows 30–45 s.
    assert!(elapsed > Duration::from_secs(29) && elapsed <= Duration::from_secs(45), "{elapsed:?}");
    assert_eq!((answer.status, answer.header("x-graphite-upload-refusal")), (408, Some("idle")));
}

#[tokio::test]
async fn an_upload_reaching_the_operation_lifetime_closes_unanswered() {
    let server = start(&[("GM_MAX_OPERATION_DURATION", "1s")]).await;
    let id = server.upload_id().await;
    let mut client = server.connect().await;
    let started = Instant::now();
    client
        .send(&format!(
            "POST /upload?id={id} HTTP/1.1\r\nHost: test\r\nContent-Length: 1000\r\n\r\npartial"
        ))
        .await;
    assert!(client.answer().await.is_none(), "no answer");
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(3), "{elapsed:?}");
    let answer = server
        .connect()
        .await
        .request(&format!("POST /upload/checkpoint?id={id}"), "")
        .await;
    let counters: serde_json::Value = serde_json::from_slice(&answer.body).unwrap();
    assert_eq!(counters["bytes"], 7, "the bytes of an unanswered upload count");
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
