//! Complete HTTP/1 exchanges; streaming and malformed requests keep their own wire handling.
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub async fn exchange(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    method: &str,
    path: &str,
    host: &str,
    headers: &str,
    body: &[u8],
) -> (String, Vec<u8>) {
    stream
        .write_all(
            format!(
                "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    stream.write_all(body).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let boundary = response.windows(4).position(|part| part == b"\r\n\r\n").unwrap();
    (
        String::from_utf8(response[..boundary + 4].to_vec()).unwrap(),
        response[boundary + 4..].to_vec(),
    )
}
