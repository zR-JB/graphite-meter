//! Downloads and uploads over HTTP/1.

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
