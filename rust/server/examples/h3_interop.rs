//! Transport compatibility probe, not a deployable measurement server.
use std::{
    collections::HashMap,
    error::Error,
    sync::{Arc, Mutex},
};

use bytes::Bytes;
use graphite_meter_server::webtransport::{ReceiveStream, TransportError};
use graphite_meter_server::webtransport_send::ResetQueue;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

const MAX_REQUESTS: usize = 64;
const MAX_SESSION_LANES: usize = 8;
const MAX_PROBE_UPLOAD_BYTES: u64 = 1024;
const WT_SESSION_GONE: u32 = 0x170d7b68;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 {
        return Err("usage: h3_interop ADDRESS CERT_PEM KEY_PEM".into());
    }
    let certs = CertificateDer::pem_file_iter(&args[2])?.collect::<Result<Vec<_>, _>>()?;
    let key = PrivateKeyDer::from_pem_file(&args[3])?;
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    let mut transport = quinn::TransportConfig::default();
    transport.datagram_receive_buffer_size(Some(65536));
    config.transport_config(Arc::new(transport));
    let endpoint = quinn::Endpoint::server(config, args[1].parse()?)?;
    println!("listening {}", endpoint.local_addr()?);
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            Some(result) = tasks.join_next() => {
                if let Err(error) = result {
                    eprintln!("connection task: {error}");
                }
            }
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else {
                    break;
                };
                tasks.spawn(async move {
                    if let Err(error) = serve(incoming).await {
                        eprintln!("connection: {error}");
                    }
                });
            }
        }
    }
    endpoint.close(0_u32.into(), b"probe complete");
    tasks.shutdown().await;
    endpoint.wait_idle().await;
    Ok(())
}

type RequestStream = h3::server::RequestStream<h3_noq::BidiStream<Bytes>, Bytes>;

enum SessionEvent {
    Datagram(Bytes),
    Stream(ReceiveStream),
}

#[derive(Clone, Default)]
struct Sessions(Arc<Mutex<HashMap<u64, mpsc::Sender<SessionEvent>>>>);

impl Sessions {
    fn register(&self, id: u64) -> (Registration, mpsc::Receiver<SessionEvent>) {
        let (sender, receiver) = mpsc::channel(32);
        self.0
            .lock()
            .expect("session registry poisoned")
            .insert(id, sender);
        let registration = Registration {
            sessions: self.clone(),
            id,
        };
        (registration, receiver)
    }

    fn sender(&self, id: u64) -> Option<mpsc::Sender<SessionEvent>> {
        self.0
            .lock()
            .expect("session registry poisoned")
            .get(&id)
            .cloned()
    }

    fn datagram(&self, id: u64, payload: Bytes) {
        if let Some(sender) = self.sender(id) {
            // Datagram overflow is loss, not backpressure on the connection.
            let _ = sender.try_send(SessionEvent::Datagram(payload));
        }
    }

    fn stream(&self, id: u64, mut stream: ReceiveStream) {
        use h3::quic::RecvStream;

        let Some(sender) = self.sender(id) else {
            stream.stop_sending(u64::from(WT_SESSION_GONE));
            return;
        };
        if let Err(error) = sender.try_send(SessionEvent::Stream(stream))
            && let SessionEvent::Stream(mut stream) = error.into_inner()
        {
            stream.stop_sending(u64::from(WT_SESSION_GONE));
        }
    }
}

struct Registration {
    sessions: Sessions,
    id: u64,
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.sessions
            .0
            .lock()
            .expect("session registry poisoned")
            .remove(&self.id);
    }
}

async fn serve(incoming: quinn::Incoming) -> Result<(), Box<dyn Error + Send + Sync>> {
    use graphite_meter_server::webtransport::{Connection, Incoming};
    let quic = incoming.await?;
    let mut connection = Connection::new(quic.clone(), 64).await?;
    let sessions = Sessions::default();
    let mut tasks = tokio::task::JoinSet::new();
    let (resets, mut pending_resets) = ResetQueue::new(64);
    let mut cleanup = tokio::task::JoinSet::new();
    let result = async {
        loop {
            tokio::select! {
                Some(reset) = pending_resets.recv() => {
                    cleanup.spawn(reset.complete());
                }
                Some(done) = cleanup.join_next() => {
                    match done {
                        Ok(result) => result?,
                        Err(error) => return Err(error.into()),
                    }
                }
                Some(done) = tasks.join_next() => {
                    match done {
                        Ok(Ok(())) => (),
                        Ok(Err(error)) => eprintln!("request: {error}"),
                        Err(error) => return Err(error.into()),
                    }
                }
                event = connection.next() => {
                    let Some(event) = event? else {
                        return Ok(());
                    };
                    match event {
                        Incoming::Request(request) => {
                            if tasks.len() >= MAX_REQUESTS {
                                drop(request);
                                continue;
                            }
                            let sessions = sessions.clone();
                            let quic = quic.clone();
                            let resets = resets.clone();
                            tasks.spawn(async move {
                                let (request, stream) = request.resolve_request().await?;
                                serve_request(quic, resets, sessions, request, stream).await
                            });
                        }
                        Incoming::Datagram { session_id, payload } => {
                            sessions.datagram(session_id, payload);
                        }
                        Incoming::Unidirectional { session_id, stream } => {
                            sessions.stream(session_id, stream);
                        }
                    }
                }
            }
        }
    }
    .await;
    // Closing the connection ends the reliable-prefix obligation before owned
    // request and cleanup tasks can be cancelled.
    quic.close(0_u32.into(), b"connection dispatcher stopped");
    tasks.shutdown().await;
    cleanup.shutdown().await;
    result
}

async fn send_uni(
    quic: &quinn::Connection,
    resets: &ResetQueue,
    id: u64,
    payload: &[u8],
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut stream = resets.open(quic, id, WT_SESSION_GONE.into()).await?;
    stream.write_all(payload).await?;
    stream.finish()?;
    Ok(())
}

async fn serve_request(
    quic: quinn::Connection,
    resets: ResetQueue,
    sessions: Sessions,
    request: http::Request<()>,
    mut stream: RequestStream,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    use bytes::Buf;
    use graphite_meter_core::capsule::{Capsule, Decoder, encode_close};
    if request.method() != http::Method::CONNECT {
        stream
            .send_response(http::Response::builder().status(200).body(())?)
            .await?;
        stream
            .send_data(Bytes::from_static(b"transport probe\n"))
            .await?;
        stream.finish().await?;
        return Ok(());
    }
    if request.extensions().get::<h3::ext::Protocol>() != Some(&h3::ext::Protocol::WEB_TRANSPORT) {
        stream
            .send_response(http::Response::builder().status(400).body(())?)
            .await?;
        stream.finish().await?;
        return Ok(());
    }
    let id = stream.send_id().into_inner();
    let (_registration, mut events) = sessions.register(id);
    stream
        .send_response(http::Response::builder().status(200).body(())?)
        .await?;
    let path = request.uri().path();
    if path == "/wt/download" {
        send_uni(&quic, &resets, id, b"webtransport download\n").await?;
    }
    if path == "/wt/reset" {
        // WT application error 7 mapped to the HTTP/3 error range.
        let reset_code = quinn::VarInt::from_u64(0x52e4a40fa8db + 7)?;
        resets
            .open(&quic, id, reset_code)
            .await?
            .reset(reset_code)?;
    }
    if path == "/wt/close" {
        stream
            .send_data(Bytes::from(encode_close(17, "probe closed")))
            .await?;
        stream.finish().await?;
        return Ok(());
    }
    let mut capsules = Decoder::new();
    let mut lanes = tokio::task::JoinSet::new();
    let result = async {
        loop {
            tokio::select! {
                Some(done) = lanes.join_next() => {
                    match done {
                        Ok(result) => result?,
                        Err(error) => return Err(error.into()),
                    }
                }
                data = stream.recv_data() => {
                    let Some(mut data) = data? else {
                        capsules.finish()?;
                        return Ok(());
                    };
                    let bytes = data.copy_to_bytes(data.remaining());
                    for capsule in capsules.feed(&bytes)? {
                        if matches!(capsule, Capsule::CloseSession { .. }) {
                            stream.finish().await?;
                            return Ok(());
                        }
                    }
                }
                event = events.recv() => {
                    match event {
                        Some(SessionEvent::Datagram(payload)) => {
                            reply_ping(&quic, id, &payload)?;
                        }
                        Some(SessionEvent::Stream(mut receive)) => {
                            if lanes.len() >= MAX_SESSION_LANES {
                                use h3::quic::RecvStream;
                                receive.stop_sending(u64::from(WT_SESSION_GONE));
                                continue;
                            }
                            let quic = quic.clone();
                            let resets = resets.clone();
                            lanes.spawn(echo_upload(quic, resets, id, receive));
                        }
                        None => return Ok(()),
                    }
                }
            }
        }
    }
    .await;
    lanes.shutdown().await;
    result
}

fn reply_ping(
    quic: &quinn::Connection,
    session_id: u64,
    payload: &[u8],
) -> Result<(), TransportError> {
    use graphite_meter_core::capsule::encode_varint;
    use graphite_meter_server::ping;

    let Some(pong) = ping::reply(payload) else {
        return Ok(());
    };
    let mut reply = Vec::with_capacity(8 + pong.len());
    encode_varint(session_id / 4, &mut reply)?;
    reply.extend_from_slice(pong.as_bytes());
    quic.send_datagram(Bytes::from(reply))?;
    Ok(())
}

async fn echo_upload(
    quic: quinn::Connection,
    resets: ResetQueue,
    session_id: u64,
    mut receive: ReceiveStream,
) -> Result<(), TransportError> {
    let mut payload = Vec::new();
    (&mut receive)
        .take(MAX_PROBE_UPLOAD_BYTES + 1)
        .read_to_end(&mut payload)
        .await?;
    if payload.len() as u64 > MAX_PROBE_UPLOAD_BYTES {
        return Err("probe upload exceeds 1024 bytes".into());
    }
    send_uni(&quic, &resets, session_id, &payload).await
}
