//! HTTP/2 requests and bodies; callers keep their own connections and assertions.
use h2::RecvStream;
use http::{Request, Version};

pub fn request(method: &str, path: &str) -> Request<()> {
    Request::builder()
        .method(method)
        .uri(format!("https://localhost{path}"))
        .version(Version::HTTP_2)
        .body(())
        .unwrap()
}

/// The whole body, its flow-control capacity released as it arrives.
pub async fn body(mut body: RecvStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk.unwrap();
        body.flow_control().release_capacity(chunk.len()).unwrap();
        bytes.extend_from_slice(&chunk);
    }
    bytes
}
