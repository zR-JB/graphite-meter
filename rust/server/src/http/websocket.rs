//! Upgrade ownership stays with the accepting HTTP/1 connection task.

use super::*;
use crate::websocket::{self, CloseReason};

pub(super) struct Upgrade {
    handshake: hyper::upgrade::OnUpgrade,
    deadline: tokio::time::Instant,
    _permit: Permit,
    lease: Option<AuthLease>,
    stopping: tokio::sync::watch::Sender<bool>,
}

impl Upgrade {
    pub(super) async fn run(self) {
        let stream = tokio::select! {
            biased;
            _ = lease_ended(self.lease.clone()) => return,
            result = tokio::time::timeout_at(self.deadline, self.handshake) => {
                match result {
                    Ok(Ok(stream)) => stream,
                    _ => return,
                }
            },
        };
        websocket::serve_ping(TokioIo::new(stream), self.deadline, async {
            tokio::select! {
                biased;
                _ = stopped(self.stopping) => CloseReason::Shutdown,
                _ = lease_ended(self.lease) => CloseReason::Revoked,
            }
        })
        .await;
    }
}

impl HttpServer {
    pub(super) fn upgrade_websocket<B>(
        &self,
        mut request: Request<B>,
        owner: &Owner,
        lease: Option<AuthLease>,
        pending: &Mutex<Option<Upgrade>>,
    ) -> Response<ResponseBody> {
        let permit = match self.admit(Route::Ping, owner) {
            Ok(permit) => permit,
            Err(refusal) => return *refusal,
        };
        let approved_origin = lease.as_ref().and_then(|lease| {
            lease.browser_origin().or_else(|| {
                (!lease.is_bearer()).then(|| {
                    self.auth
                        .as_ref()
                        .expect("cookie auth enabled")
                        .policy()
                        .public_origin()
                })
            })
        });
        let response = websocket::handshake(&request, approved_origin);
        if response.status() == StatusCode::SWITCHING_PROTOCOLS {
            *lock(pending) = Some(Upgrade {
                handshake: hyper::upgrade::on(&mut request),
                deadline: tokio::time::Instant::now() + self.config.max_operation_duration,
                _permit: permit,
                lease,
                stopping: self.stopping.clone(),
            });
        }
        response.map(|()| ResponseBody::empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use futures_util::{SinkExt, StreamExt};
    use graphite_meter_core::wire::decode_pong;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };
    use tokio_tungstenite::tungstenite::Message;

    async fn exchange(address: SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await.unwrap();
        answer
    }

    async fn within<T>(future: impl Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(2), future).await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_upgrade_retains_admission_and_shutdown_owns_the_socket() {
        let mut config = Config::default();
        config.limits.operations_per_client = 1;
        config.limits.sessions_per_client = 1;
        let server = Arc::new(HttpServer::new(config.validated().unwrap()).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(server.clone().serve(NativeKind::H1, listener, None, async {
            let _ = stopped.await;
        }));
        let url = format!("ws://{address}/ws/ping");
        let (mut socket, response) = tokio_tungstenite::client_async(url, TcpStream::connect(address).await.unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        socket.send(Message::Text("PING,23".into())).await.unwrap();
        let pong = within(socket.next()).await.unwrap().unwrap();
        assert_eq!(decode_pong(pong.to_text().unwrap()).unwrap().id, 23);
        // The upgraded socket holds its client's only permit.
        let download = "GET /download?bytes=1 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
        let refused = exchange(address, download).await;
        assert!(refused.starts_with("HTTP/1.1 429"), "{refused}");
        let wrong = "POST /ws/ping HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let answer = exchange(address, wrong).await;
        let refused = answer.starts_with("HTTP/1.1 405") && answer.contains("allow: GET, HEAD");
        assert!(refused, "{answer}");

        stop.send(()).unwrap();
        within(task).await.unwrap().unwrap();
        let Message::Close(Some(close)) = within(socket.next()).await.unwrap().unwrap() else {
            panic!("expected shutdown close")
        };
        assert_eq!(u16::from(close.code), 1001);
        assert_eq!(close.reason, "shutdown");
        // Shutdown released the socket's permit: the pipeline admits the client's next download.
        let download = super::super::tests::respond(&server, Method::GET, "/download?bytes=1").await;
        assert_eq!(download.status(), StatusCode::OK);
    }
}
