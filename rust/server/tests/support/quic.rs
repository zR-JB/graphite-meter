//! An HTTP/3 server fixture and request helpers; callers keep their own clients and assertions.
#[path = "../../../test_tls.rs"]
pub mod test_tls;

use bytes::Bytes;
use graphite_meter_http3::{self as http3, RecvHalf, client::SendRequest};
use graphite_meter_server::{ServerError, config::Config, http::HttpServer};
use http::{Request, Response, StatusCode};
use std::{error::Error, net::SocketAddr, sync::Arc};
use tokio::{sync::oneshot, task::JoinHandle};

pub type TestError = Box<dyn Error + Send + Sync>;

pub struct QuicServer {
    pub address: SocketAddr,
    /// Trusts the server and offers HTTP/3.
    pub client: noq::ClientConfig,
    pub stop: oneshot::Sender<()>,
    pub task: JoinHandle<Result<(), ServerError>>,
}

impl QuicServer {
    pub async fn stop(self) -> Result<(), TestError> {
        let _ = self.stop.send(());
        self.task.await?
    }
}

pub fn serve(config: Config) -> Result<QuicServer, TestError> {
    let (tls, client) = test_tls::configs("localhost", &[&rustls::version::TLS13], &[b"h3"])?;
    let server = Arc::new(HttpServer::new(config.validated()?)?);
    let endpoint = server.quic_endpoint(Arc::new(tls), "127.0.0.1:0".parse()?)?;
    let address = endpoint.local_addr()?;
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.serve_quic(endpoint, async {
        let _ = stopped.await;
    }));
    let client = noq::ClientConfig::new(Arc::new(noq::crypto::rustls::QuicClientConfig::try_from(client)?));
    Ok(QuicServer {
        address,
        client,
        stop,
        task,
    })
}

/// The task that drives `quic`'s HTTP/3 layer, and its request sender.
pub fn requests(quic: noq::Connection) -> (JoinHandle<Result<(), http3::Error>>, SendRequest) {
    let (mut driver, requests) = http3::client::new(quic);
    (tokio::spawn(async move { driver.drive().await }), requests)
}

/// Sends `upload` as the request body and returns the response with its body stream.
pub async fn send(
    requests: &SendRequest,
    method: &str,
    path: &str,
    upload: impl Into<Bytes>,
) -> Result<(Response<()>, RecvHalf), TestError> {
    let request = Request::builder()
        .method(method)
        .uri(format!("https://localhost{path}"))
        .body(())?;
    let (mut send, mut recv) = requests.send_request(request).await?.split();
    let upload = upload.into();
    if !upload.is_empty() {
        send.send_data(upload).await?;
    }
    send.finish().await?;
    Ok((recv.response().await?, recv))
}

pub async fn read(recv: &mut RecvHalf) -> Result<Vec<u8>, TestError> {
    let mut bytes = Vec::new();
    while let Some(data) = recv.data().await? {
        bytes.extend_from_slice(&data);
    }
    Ok(bytes)
}

/// A 200 reply's body.
pub async fn body(
    requests: &SendRequest,
    method: &str,
    path: &str,
    upload: impl Into<Bytes>,
) -> Result<Vec<u8>, TestError> {
    let (response, mut recv) = send(requests, method, path, upload).await?;
    assert_eq!(response.status(), StatusCode::OK);
    read(&mut recv).await
}

/// A JSON reply's body.
pub async fn json(requests: &SendRequest, method: &str, path: &str) -> Result<serde_json::Value, TestError> {
    let body = body(requests, method, path, Bytes::new()).await?;
    Ok(serde_json::from_slice(&body)?)
}
