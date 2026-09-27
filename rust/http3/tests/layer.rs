//! The layer over real noq connections on loopback: our client against our server, and raw peers
//! that send what a conforming client never would. Deadlines run on tokio's paused clock.
use bytes::Bytes;
use graphite_meter_http3::{Code, Error, RequestStream, client, server};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

#[path = "../../test_identity.rs"]
mod test_identity;

type TestError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug)]
struct Budget {
    used: AtomicUsize,
    limit: AtomicUsize,
}

impl noq::SharedBudget for Budget {
    fn try_charge(&self, bytes: usize) -> bool {
        self.used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(bytes)
                    .filter(|&used| used <= self.limit.load(Ordering::Relaxed))
            })
            .is_ok()
    }

    fn refund(&self, bytes: usize) {
        self.used.fetch_sub(bytes, Ordering::Relaxed);
    }
}

struct Peers {
    server: noq::Connection,
    client: noq::Connection,
    budget: Arc<Budget>,
    _endpoints: [noq::Endpoint; 2],
}

async fn peers(limit: usize) -> Result<Peers, TestError> {
    let (certificate, key) = test_identity::generate_identity("localhost")?;
    let certificate = CertificateDer::from_pem_slice(certificate.as_bytes())?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut tls = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.clone()],
            PrivateKeyDer::from_pem_slice(key.as_bytes())?,
        )?;
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let mut transport = noq::TransportConfig::default();
    // Fake-clock jumps stay inside the idle timeout.
    transport.max_idle_timeout(Some(Duration::from_secs(120).try_into()?));
    let transport = Arc::new(transport);
    let mut server = noq::ServerConfig::with_crypto(Arc::new(noq::crypto::rustls::QuicServerConfig::try_from(tls)?));
    server.transport_config(transport.clone());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate)?;
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let mut client = noq::ClientConfig::new(Arc::new(noq::crypto::rustls::QuicClientConfig::try_from(tls)?));
    client.transport_config(transport);
    let server = noq::Endpoint::server(server, "127.0.0.1:0".parse()?)?;
    let client_endpoint = noq::Endpoint::client("127.0.0.1:0".parse()?)?;
    let connecting = client_endpoint.connect_with(client, server.local_addr()?, "localhost")?;
    let (client, accepted) = tokio::join!(connecting, async { server.accept().await.expect("incoming").await });
    Ok(Peers {
        server: accepted?,
        client: client?,
        budget: Arc::new(Budget {
            used: AtomicUsize::new(0),
            limit: AtomicUsize::new(limit),
        }),
        _endpoints: [server, client_endpoint],
    })
}

type Serving = tokio::task::JoinHandle<Result<(), Error>>;

/// Serves each request on its own task until the connection closes; `stop` sends GOAWAY.
fn serve<F, H>(peers: &Peers, handler: H) -> (Serving, Arc<Notify>)
where
    H: Fn(http::Request<()>, RequestStream) -> F + Send + Sync + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let mut connection = server::Connection::new(peers.server.clone(), Some(peers.budget.clone()));
    let (handler, stop) = (Arc::new(handler), Arc::new(Notify::new()));
    let stopping = stop.clone();
    let serving = tokio::spawn(async move {
        let mut stopped = false;
        loop {
            let request = tokio::select! {
                request = connection.next() => request?,
                () = stopping.notified(), if !stopped => {
                    stopped = true;
                    connection.shutdown();
                    continue;
                }
            };
            let Some(request) = request else { return Ok(()) };
            let handler = handler.clone();
            tokio::spawn(async move {
                if let Ok((request, stream)) = request.resolve().await {
                    handler(request, stream).await;
                }
            });
        }
    });
    (serving, stop)
}

fn client(peers: &Peers) -> (tokio::task::JoinHandle<Result<(), Error>>, client::SendRequest) {
    let (mut driver, requests) = client::new(peers.client.clone());
    (tokio::spawn(async move { driver.drive().await }), requests)
}

fn get(path: &str) -> http::Request<()> {
    http::Request::get(format!("https://localhost{path}")).body(()).unwrap()
}

async fn body(stream: &mut graphite_meter_http3::RecvHalf) -> Result<Vec<u8>, Error> {
    let mut body = Vec::new();
    while let Some(chunk) = stream.data().await? {
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn settled(budget: &Budget) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while budget.used.load(Ordering::Relaxed) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("layer charges return to zero");
}

#[tokio::test]
async fn requests_carry_heads_and_bodies_both_ways() -> Result<(), TestError> {
    let peers = peers(usize::MAX).await?;
    let (serving, _) = serve(&peers, |request, stream| async move {
        let (mut send, mut recv) = stream.split();
        let received = body(&mut recv).await.unwrap();
        let response = http::Response::builder()
            .header("x-method", request.method().as_str())
            .header("content-length", 11)
            .body(())
            .unwrap();
        send.send_response(response).await.unwrap();
        if request.method() != http::Method::HEAD {
            let echo = if received.is_empty() {
                &b"hello world"[..]
            } else {
                &received[..]
            };
            send.send_data(Bytes::copy_from_slice(&echo[..5])).await.unwrap();
            send.send_data(Bytes::copy_from_slice(&echo[5..])).await.unwrap();
        }
        send.finish().await.unwrap();
    });
    let (driver, requests) = client(&peers);
    for (method, sent) in [("GET", &b""[..]), ("POST", b"upload body"), ("HEAD", b"")] {
        let request = http::Request::builder().method(method).uri("https://localhost/echo");
        let request = request.header("content-length", sent.len()).body(())?;
        let (mut send, mut recv) = requests.send_request(request).await?.split();
        if !sent.is_empty() {
            send.send_data(Bytes::copy_from_slice(sent)).await?;
        }
        send.finish().await?;
        let response = recv.response().await?;
        assert_eq!(
            (response.status(), response.headers()["x-method"].to_str()?),
            (http::StatusCode::OK, method)
        );
        let expected = match method {
            "HEAD" => &b""[..],
            "POST" => sent,
            _ => b"hello world",
        };
        assert_eq!(body(&mut recv).await?, expected);
    }
    settled(&peers.budget).await;
    peers.client.close(0_u32.into(), b"done");
    assert!(
        driver.await?.is_ok() && serving.await?.is_ok(),
        "a peer's close code 0 is graceful"
    );
    Ok(())
}

#[tokio::test]
async fn abandoned_streams_carry_the_codes_table() -> Result<(), TestError> {
    let peers = peers(usize::MAX).await?;
    let (serving, _) = serve(&peers, |request, stream| async move {
        let (mut send, recv) = stream.split();
        match request.uri().path() {
            // The response completes while the request body is unread.
            "/unread" => {
                send.send_response(http::Response::new(())).await.unwrap();
                send.send_data(Bytes::from_static(b"done")).await.unwrap();
                send.finish().await.unwrap();
                drop(recv);
            }
            "/unfinished" => {
                send.send_response(http::Response::new(())).await.unwrap();
                send.send_data(Bytes::from_static(b"partial")).await.unwrap();
            }
            _ => {}
        }
    });
    let (driver, requests) = client(&peers);
    let (mut send, mut recv) = requests.send_request(get("/unread")).await?.split();
    assert_eq!(
        recv.response().await.map(|response| response.status()),
        Ok(http::StatusCode::OK)
    );
    assert_eq!(body(&mut recv).await, Ok(b"done".to_vec()));
    let stopped = loop {
        if let Err(error) = send.send_data(Bytes::from(vec![0; 64 * 1024])).await {
            break error;
        }
    };
    assert_eq!(stopped, Error::Stopped(Code::H3_NO_ERROR));
    // RESET_STREAM may discard the head along with the body.
    let (_send, mut recv) = requests.send_request(get("/unfinished")).await?.split();
    let received = async {
        body(&mut {
            recv.response().await?;
            recv
        })
        .await
    }
    .await;
    assert_eq!(received, Err(Error::Reset(Code::H3_REQUEST_CANCELLED)));
    drop(send);
    settled(&peers.budget).await;
    drop((driver, serving));
    Ok(())
}

fn varint(value: u64) -> Vec<u8> {
    let (size, tag) = match value {
        0..64 => (1, 0x00),
        64..16_384 => (2, 0x40),
        16_384..1_073_741_824 => (4, 0x80),
        _ => (8, 0xc0),
    };
    let mut bytes = value.to_be_bytes()[8 - size..].to_vec();
    bytes[0] |= tag;
    bytes
}

fn frame(kind: u64, payload: &[u8]) -> Vec<u8> {
    [varint(kind), varint(payload.len() as u64), payload.to_vec()].concat()
}

/// A static-only QPACK section of literal names and values, as an independent encoder.
fn section(fields: &[(&str, &str)]) -> Vec<u8> {
    fn integer(value: usize, bits: u32, pattern: u8, output: &mut Vec<u8>) {
        let max = (1 << bits) - 1;
        if value < max {
            return output.push(pattern | value as u8);
        }
        output.push(pattern | max as u8);
        let mut rest = value - max;
        while rest >= 0x80 {
            output.push(0x80 | rest as u8);
            rest >>= 7;
        }
        output.push(rest as u8);
    }
    let mut section = vec![0, 0];
    for (name, value) in fields {
        integer(name.len(), 3, 0x20, &mut section);
        section.extend_from_slice(name.as_bytes());
        integer(value.len(), 7, 0x00, &mut section);
        section.extend_from_slice(value.as_bytes());
    }
    section
}

fn request_head(fields: &[(&str, &str)]) -> Vec<u8> {
    let head = [
        (":method", "GET"),
        (":scheme", "https"),
        (":authority", "localhost"),
        (":path", "/"),
    ];
    let mut all: Vec<_> = head
        .iter()
        .filter(|(name, _)| !fields.iter().any(|(field, _)| field == name))
        .collect();
    all.extend(fields);
    frame(0x01, &section(&all.into_iter().copied().collect::<Vec<_>>()))
}

const CONTROL: [u8; 3] = [0x00, 0x04, 0x00];

async fn uni(quic: &noq::Connection, bytes: &[u8], finish: bool) -> Result<noq::SendStream, TestError> {
    let mut stream = quic.open_uni().await?;
    stream.write_all(bytes).await?;
    if finish {
        stream.finish()?;
    }
    Ok(stream)
}

async fn closed_with(quic: &noq::Connection) -> Code {
    match quic.closed().await {
        noq::ConnectionError::ApplicationClosed(close) => Code(close.error_code.into_inner()),
        error => panic!("closed without an application code: {error}"),
    }
}

#[tokio::test]
async fn protocol_violations_close_the_connection_with_their_code() -> Result<(), TestError> {
    let request = |bytes: Vec<u8>| (None, Some(bytes));
    let streams = |streams: Vec<(Vec<u8>, bool)>| (Some(streams), None);
    let cases = [
        (
            streams(vec![([&[0x00][..], &frame(0x07, &[4])].concat(), false)]),
            Code::H3_MISSING_SETTINGS,
        ),
        (
            streams(vec![(CONTROL.to_vec(), false), (CONTROL.to_vec(), false)]),
            Code::H3_STREAM_CREATION_ERROR,
        ),
        (streams(vec![(CONTROL.to_vec(), true)]), Code::H3_CLOSED_CRITICAL_STREAM),
        (
            streams(vec![([&CONTROL[..], &frame(0x06, &[0; 8])].concat(), false)]),
            Code::H3_FRAME_UNEXPECTED,
        ),
        (
            streams(vec![(
                [&[0x00][..], &frame(0x04, &[0x21, 0x00, 0x21, 0x01])].concat(),
                false,
            )]),
            Code::H3_SETTINGS_ERROR,
        ),
        (
            streams(vec![(vec![0x02, 0xc1, 0x01, 0x61], false)]),
            Code::QPACK_ENCODER_STREAM_ERROR,
        ),
        (
            streams(vec![(vec![0x03, 0x80], false)]),
            Code::QPACK_DECODER_STREAM_ERROR,
        ),
        (streams(vec![(vec![0x01, 0x00], false)]), Code::H3_STREAM_CREATION_ERROR),
        (request(frame(0x00, b"body")), Code::H3_FRAME_UNEXPECTED),
        (
            request([request_head(&[]), frame(0x04, &[])].concat()),
            Code::H3_FRAME_UNEXPECTED,
        ),
        (
            request(frame(0x01, &[0x01, 0x00, 0xd1])),
            Code::QPACK_DECOMPRESSION_FAILED,
        ),
    ];
    for (index, ((uni_streams, bidi), code)) in cases.into_iter().enumerate() {
        let peers = peers(usize::MAX).await?;
        let (serving, _) = serve(&peers, |_, stream| async move {
            let _ = stream.split().1.data().await;
        });
        let _held = match uni_streams {
            Some(streams) => {
                let mut held = Vec::new();
                for (bytes, finish) in streams {
                    held.push(uni(&peers.client, &bytes, finish).await?);
                }
                held
            }
            None => {
                let (mut send, _recv) = peers.client.open_bi().await?;
                send.write_all(&bidi.expect("request bytes")).await?;
                vec![send]
            }
        };
        assert_eq!(closed_with(&peers.client).await, code, "case {index}");
        assert_eq!(
            serving.await?,
            Err(Error::Connection {
                local: true,
                code,
                reason: Bytes::new()
            })
        );
    }
    Ok(())
}

/// Reads a raw response stream: its bytes up to FIN, or the reset code.
async fn response_bytes(recv: &mut noq::RecvStream) -> Result<Vec<u8>, Code> {
    match recv.read_to_end(64 * 1024).await {
        Ok(bytes) => Ok(bytes),
        Err(noq::ReadToEndError::Read(noq::ReadError::Reset(code))) => Err(Code(code.into_inner())),
        Err(error) => panic!("response: {error}"),
    }
}

async fn stopped(send: &noq::SendStream) -> Option<Code> {
    send.stopped()
        .await
        .expect("stream outcome")
        .map(|code| Code(code.into_inner()))
}

#[tokio::test]
async fn refusals_stay_on_their_stream() -> Result<(), TestError> {
    // Our QPACK: :status 431 as a literal after the static :status name, :status 400 indexed.
    let status_431 = frame(0x01, &[0x00, 0x00, 0x5f, 0x09, 0x03, b'4', b'3', b'1']);
    let status_400 = frame(0x01, &[0x00, 0x00, 0xff, 0x04]);
    let many: Vec<(&str, &str)> = std::iter::repeat_n(("a", ""), 130).collect();
    let wt = Code(0x52e4a40fa8db);
    let cases = [
        (
            frame(0x01, &[0; 5000])[..100].to_vec(),
            Ok(status_431.clone()),
            Some(Code::H3_EXCESSIVE_LOAD),
        ),
        (request_head(&many), Ok(status_431), Some(Code::H3_EXCESSIVE_LOAD)),
        (
            request_head(&[("Upper", "x")]),
            Err(Code::H3_MESSAGE_ERROR),
            Some(Code::H3_MESSAGE_ERROR),
        ),
        (
            frame(
                0x01,
                &section(&[(":method", "CONNECT"), (":authority", "localhost:443")]),
            ),
            Ok(status_400.clone()),
            None,
        ),
        (
            request_head(&[(":method", "CONNECT"), (":protocol", "websocket")]),
            Ok(status_400),
            None,
        ),
        ([varint(0x41), varint(0)].concat(), Err(wt), Some(wt)),
        (
            [
                request_head(&[(":method", "POST"), ("content-length", "5")]),
                frame(0x00, b"abcdefgh"),
            ]
            .concat(),
            Err(Code::H3_REQUEST_CANCELLED),
            Some(Code::H3_MESSAGE_ERROR),
        ),
    ];
    let peers = peers(usize::MAX).await?;
    let (serving, _) = serve(&peers, |_, stream| async move {
        let _ = stream.split().1.data().await;
    });
    let _control = uni(&peers.client, &CONTROL, false).await?;
    for (index, (bytes, response, stop)) in cases.into_iter().enumerate() {
        // Bodies stay open, so each STOP_SENDING arrives before the stream could complete.
        let (mut send, mut recv) = peers.client.open_bi().await?;
        send.write_all(&bytes).await?;
        assert_eq!(response_bytes(&mut recv).await, response, "case {index}");
        if stop.is_some() {
            assert_eq!(stopped(&send).await, stop, "case {index}");
        }
    }
    assert!(
        peers.client.close_reason().is_none(),
        "refusals leave the connection open"
    );
    settled(&peers.budget).await;
    drop(serving);
    Ok(())
}

#[tokio::test]
async fn a_budget_refusal_rejects_only_the_new_request() -> Result<(), TestError> {
    let peers = peers(0).await?;
    let (serving, _) = serve(&peers, |_, stream| async move {
        let mut send = stream.split().0;
        send.send_response(http::Response::new(())).await.unwrap();
        send.finish().await.unwrap();
    });
    let (driver, requests) = client(&peers);
    let (_send, mut recv) = requests.send_request(get("/")).await?.split();
    assert_eq!(
        recv.response().await.err(),
        Some(Error::Reset(Code::H3_REQUEST_REJECTED))
    );
    peers.budget.limit.store(usize::MAX, Ordering::Relaxed);
    let (_send, mut recv) = requests.send_request(get("/")).await?.split();
    assert_eq!(recv.response().await?.status(), http::StatusCode::OK);
    drop((_send, recv));
    settled(&peers.budget).await;
    drop((driver, serving));
    Ok(())
}

#[tokio::test]
async fn goaway_refuses_new_requests_and_closes_after_the_last() -> Result<(), TestError> {
    let peers = peers(usize::MAX).await?;
    let (started, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let (running, held) = (started.clone(), release.clone());
    let (serving, stop) = serve(&peers, move |_, stream| {
        let (running, held) = (running.clone(), held.clone());
        async move {
            running.notify_one();
            held.notified().await;
            let mut send = stream.split().0;
            send.send_response(http::Response::new(())).await.unwrap();
            send.finish().await.unwrap();
        }
    });
    let (driver, requests) = client(&peers);
    let (_send, mut recv) = requests.send_request(get("/held")).await?.split();
    started.notified().await;
    stop.notify_one();
    // A request that ignores the GOAWAY gets H3_REQUEST_REJECTED.
    let mut ignoring = loop {
        tokio::task::yield_now().await;
        match requests.send_request(get("/late")).await {
            Err(Error::Refused) => break peers.client.open_bi().await?,
            Ok(late) => drop(late),
            Err(error) => return Err(error.into()),
        }
    };
    ignoring.0.write_all(&request_head(&[])).await?;
    assert_eq!(response_bytes(&mut ignoring.1).await, Err(Code::H3_REQUEST_REJECTED));
    release.notify_one();
    assert_eq!(recv.response().await?.status(), http::StatusCode::OK);
    drop((_send, recv, ignoring));
    assert_eq!((driver.await?, serving.await?), (Ok(()), Ok(())));
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);
    Ok(())
}

/// Runs `deadline` of tokio time at once; noq's timers see the jump too, well inside its idle timeout.
async fn jump(deadline: Duration) {
    tokio::time::pause();
    tokio::time::advance(deadline).await;
    tokio::time::resume();
}

#[tokio::test]
async fn deadlines_close_idle_and_draining_connections_and_stale_heads() -> Result<(), TestError> {
    let peers = peers(usize::MAX).await?;
    let (serving, _) = serve(&peers, |_, _| async {});
    let (driver, _requests) = client(&peers);
    let (mut send, mut recv) = peers.client.open_bi().await?;
    send.write_all(&frame(0x01, &[0; 10])[..4]).await?;
    // Jump only once the server holds the stream and has started its header deadline.
    while peers.budget.used.load(Ordering::Relaxed) == 0 {
        tokio::task::yield_now().await;
    }
    tokio::task::yield_now().await;
    jump(Duration::from_secs(11)).await;
    assert_eq!(response_bytes(&mut recv).await, Err(Code::H3_REQUEST_INCOMPLETE));
    assert_eq!(stopped(&send).await, Some(Code::H3_REQUEST_INCOMPLETE));
    jump(Duration::from_secs(16)).await;
    assert_eq!((driver.await?, serving.await?), (Ok(()), Ok(())));
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);

    let peers = self::peers(usize::MAX).await?;
    let (serving, stop) = serve(&peers, |_, stream| async move {
        std::future::pending::<()>().await;
        drop(stream);
    });
    let (driver, requests) = client(&peers);
    let _held = requests.send_request(get("/held")).await?;
    tokio::task::yield_now().await;
    stop.notify_one();
    tokio::task::yield_now().await;
    jump(Duration::from_secs(6)).await;
    assert_eq!((driver.await?, serving.await?), (Ok(()), Ok(())));
    Ok(())
}
