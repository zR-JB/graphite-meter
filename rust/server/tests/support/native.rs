//! Native TCP listener fixture; callers retain protocol-specific clients and assertions.
use graphite_meter_server::{ServerError, config::NativeKind, http::HttpServer};
use std::{net::SocketAddr, sync::Arc};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

pub struct NativeServer {
    pub address: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    pub task: JoinHandle<Result<(), ServerError>>,
}

impl NativeServer {
    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.send(()).unwrap();
        }
    }

    pub async fn shutdown(mut self) {
        self.stop();
        (&mut self.task).await.unwrap().unwrap();
    }
}

impl Drop for NativeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn serve(server: Arc<HttpServer>, kind: NativeKind, tls: Option<Arc<rustls::ServerConfig>>) -> NativeServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server.serve(kind, listener, tls, async {
        let _ = stopped.await;
    }));
    NativeServer { address, stop: Some(stop), task }
}
