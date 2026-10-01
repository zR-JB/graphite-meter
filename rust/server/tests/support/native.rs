//! Native TCP listener fixture; callers retain protocol-specific clients and assertions.
use graphite_meter_server::{ServerError, config::NativeKind, http::HttpServer};
use std::{net::SocketAddr, sync::Arc};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

pub async fn serve(
    server: Arc<HttpServer>,
    kind: NativeKind,
    tls: Option<Arc<rustls::ServerConfig>>,
) -> (SocketAddr, oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.serve(kind, listener, tls, async {
        let _ = stopped.await;
    }));
    (address, stop, task)
}
