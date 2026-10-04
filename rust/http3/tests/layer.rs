//! The layer over real noq connections on loopback: our client against our server, and raw peers
//! that send what a conforming client never would. Deadlines run on tokio's paused clock.
use bytes::Bytes;
use graphite_meter_http3::{Code, Error, RequestStream, WtCode, client, server, webtransport::Session};
use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};
use tokio::{
    sync::{Notify, mpsc},
    task::JoinHandle,
};

#[path = "../../test_tls.rs"]
mod test_tls;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type Task = JoinHandle<Result<(), Error>>;

#[derive(Debug)]
struct Budget {
    used: AtomicUsize,
    limit: AtomicUsize,
}

impl noq::SharedBudget for Budget {
    fn try_charge(&self, bytes: usize) -> bool {
        self.used
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(bytes)
                    .filter(|&used| used <= self.limit.load(Ordering::Relaxed))
            })
            .is_ok()
    }

    fn refund(&self, bytes: usize) {
        self.used.fetch_sub(bytes, Ordering::Relaxed);
    }
}

/// How the peers connect: the limit of the budget the server's layer charges; the client's
/// per-stream receive window, reset_stream_at offer and max_datagram_frame_size transport
/// parameter; and the SETTINGS a raw client sends on the control stream it opens first.
#[derive(Clone, Copy)]
struct Setup {
    limit: usize,
    window: Option<u32>,
    reliable_reset: bool,
    datagrams: bool,
    settings: Option<&'static [(u64, u64)]>,
}

const PLAIN: Setup = Setup {
    limit: usize::MAX,
    window: None,
    reliable_reset: true,
    datagrams: true,
    settings: None,
};

const fn raw(settings: &'static [(u64, u64)]) -> Setup {
    Setup { settings: Some(settings), ..PLAIN }
}

/// The WebTransport signal of Chrome and Firefox.
const DRAFT02_SETTINGS: &[(u64, u64)] = &[(0x2b603742, 1), (0x33, 1)];
const DRAFT02: Setup = raw(DRAFT02_SETTINGS);

struct Peers {
    server: noq::Connection,
    client: noq::Connection,
    budget: Arc<Budget>,
    _held: (Option<noq::SendStream>, [noq::Endpoint; 2]),
}

async fn pair(setup: Setup) -> Result<Peers, TestError> {
    let (server_tls, client_tls) = test_tls::configs("localhost", &[&rustls::version::TLS13], &[b"h3"])?;
    let mut transport = noq::TransportConfig::default();
    // Fake-clock jumps stay inside the idle timeout.
    transport.max_idle_timeout(Some(Duration::from_secs(120).try_into()?));
    let mut server =
        noq::ServerConfig::with_crypto(Arc::new(noq::crypto::rustls::QuicServerConfig::try_from(server_tls)?));
    server.transport_config(Arc::new(transport));
    let mut client = noq::ClientConfig::new(Arc::new(noq::crypto::rustls::QuicClientConfig::try_from(client_tls)?));
    let mut client_transport = noq::TransportConfig::default();
    client_transport.max_idle_timeout(Some(Duration::from_secs(120).try_into()?));
    if let Some(window) = setup.window {
        client_transport.stream_receive_window(window.into());
    }
    if !setup.datagrams {
        client_transport.datagram_receive_buffer_size(None);
    }
    client.transport_config(Arc::new(client_transport));
    let server = noq::Endpoint::server(server, "127.0.0.1:0".parse()?)?;
    let mut endpoint = noq::EndpointConfig::default();
    endpoint.reliable_stream_reset(setup.reliable_reset);
    let socket = std::net::UdpSocket::bind("127.0.0.1:0")?;
    let client_endpoint = noq::Endpoint::new(endpoint, None, socket, noq::default_runtime().ok_or("runtime")?)?;
    let connecting = client_endpoint.connect_with(client, server.local_addr()?, "localhost")?;
    let (client, accepted) = tokio::join!(connecting, async { server.accept().await.expect("incoming").await });
    let (client, limit) = (client?, AtomicUsize::new(setup.limit));
    let control = match setup.settings {
        Some(pairs) => Some(uni(&client, &control(pairs)).await?),
        None => None,
    };
    Ok(Peers {
        server: accepted?,
        client,
        budget: Arc::new(Budget { used: AtomicUsize::new(0), limit }),
        _held: (control, [server, client_endpoint]),
    })
}

/// Peers whose server runs the layer; `stop` shuts it down, and `outcomes` passes on what each
/// session's scenario returned.
struct Served<T = ()> {
    peers: Peers,
    serving: Task,
    stop: Arc<Notify>,
    outcomes: mpsc::UnboundedReceiver<T>,
}

impl Peers {
    /// Serves each request on its own task until the connection closes.
    fn serve<F, H>(self, handler: H) -> Served
    where
        H: Fn(http::Request<()>, RequestStream) -> F + Send + Sync + 'static,
        F: Future<Output = ()> + Send + 'static,
    {
        let mut connection = server::Connection::new(self.server.clone(), Some(self.budget.clone()));
        let (handler, stop) = (Arc::new(handler), Arc::new(Notify::new()));
        let stopping = stop.clone();
        let serving = tokio::spawn(async move {
            loop {
                let request = tokio::select! {
                    request = connection.next() => request?,
                    () = stopping.notified() => {
                        connection.shutdown(4, "shutdown");
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
        let outcomes = mpsc::unbounded_channel().1;
        Served { peers: self, serving, stop, outcomes }
    }

    /// Accepts every CONNECT as a session and runs `scenario` on it; other requests get an empty 200.
    fn sessions<F, H>(self, scenario: H) -> Served<F::Output>
    where
        H: Fn(Session) -> F + Send + Sync + 'static,
        F: Future<Output: Send> + Send + 'static,
    {
        let (scenario, (outcomes, outcome)) = (Arc::new(scenario), mpsc::unbounded_channel());
        let Served { peers, serving, stop, .. } = self.serve(move |request, stream| {
            let (scenario, outcomes) = (scenario.clone(), outcomes.clone());
            async move {
                if request.method() != http::Method::CONNECT {
                    respond(request, stream).await;
                } else if let Ok(session) = Session::accept(stream, http::HeaderMap::new()).await {
                    let _ = outcomes.send(scenario(session).await);
                }
            }
        });
        Served { peers, serving, stop, outcomes: outcome }
    }

    /// A raw client request stream that has sent `bytes`.
    async fn bi(&self, bytes: &[u8]) -> Result<(noq::SendStream, noq::RecvStream), TestError> {
        let (mut send, recv) = self.client.open_bi().await?;
        send.write_all(bytes).await?;
        Ok((send, recv))
    }

    /// A raw GET answered in full, so the connection serves more than sessions and outlives them.
    async fn get(&self) -> Result<(), TestError> {
        let (mut send, mut recv) = self.bi(&request_head(&[])).await?;
        send.finish()?;
        assert_eq!(raw_stream(&mut recv).await.1, Ok(()));
        Ok(())
    }

    /// A raw CONNECT; a task reads its response stream so stream credit keeps flowing.
    async fn connect(&self) -> Result<(noq::SendStream, JoinHandle<(Vec<u8>, Result<(), Code>)>), TestError> {
        let (send, mut recv) = self.bi(&connect_head()).await?;
        Ok((send, tokio::spawn(async move { raw_stream(&mut recv).await })))
    }
}

/// Answers with an empty 200.
async fn respond(_: http::Request<()>, stream: RequestStream) {
    let mut send = stream.split().0;
    send.send_response(http::Response::new(())).await.unwrap();
    send.finish().await.unwrap();
}

fn client(peers: &Peers) -> (Task, client::SendRequest) {
    let (mut driver, requests) = client::new(peers.client.clone());
    (tokio::spawn(async move { driver.drive().await }), requests)
}

fn get(path: &str) -> http::Request<()> {
    http::Request::get(format!("https://localhost{path}")).body(()).unwrap()
}

/// Our client's session at /wt, which the server accepts.
async fn accepted(requests: &client::SendRequest) -> Result<Session, TestError> {
    Ok(Session::connect(requests, get("/wt")).await?.expect("accepted").0)
}

async fn body(stream: &mut graphite_meter_http3::RecvHalf) -> Result<Vec<u8>, Error> {
    let mut body = Vec::new();
    while let Some(chunk) = stream.data().await? {
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Yields until the layer holds more than `above` bytes of the budget, and returns how many.
async fn charged(budget: &Budget, above: usize) -> usize {
    loop {
        let used = budget.used.load(Ordering::Relaxed);
        if used > above {
            return used;
        }
        tokio::task::yield_now().await;
    }
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

async fn yields() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

/// Runs `deadline` of tokio and noq time at once; with a close in flight, noq would reset the peer instead.
async fn jump(deadline: Duration) {
    tokio::time::pause();
    tokio::time::advance(deadline).await;
    tokio::time::resume();
}

fn varint(value: u64) -> Vec<u8> {
    let size = [1, 2, 4, 8]
        .into_iter()
        .find(|size| value >> (8 * size - 2) == 0)
        .expect("a varint");
    let mut bytes = value.to_be_bytes()[8 - size..].to_vec();
    bytes[0] |= (size.trailing_zeros() as u8) << 6;
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

/// A GET of / with `fields` in place of or after its own.
fn request_head(fields: &[(&str, &str)]) -> Vec<u8> {
    let head = [(":method", "GET"), (":scheme", "https"), (":authority", "localhost"), (":path", "/")];
    let mut all: Vec<_> = head
        .into_iter()
        .filter(|(name, _)| !fields.iter().any(|(field, _)| field == name))
        .collect();
    all.extend(fields);
    frame(0x01, &section(&all))
}

fn connect_head() -> Vec<u8> {
    request_head(&[(":method", "CONNECT"), (":protocol", "webtransport"), (":path", "/wt")])
}

/// A CLOSE_WEBTRANSPORT_SESSION capsule in a DATA frame; capsules are framed as frames are.
fn close_capsule(code: u32, reason: &str) -> Vec<u8> {
    frame(0x00, &frame(0x2843, &[&code.to_be_bytes()[..], reason.as_bytes()].concat()))
}

/// A control stream's type, then SETTINGS of `pairs`.
fn control(pairs: &[(u64, u64)]) -> Vec<u8> {
    let payload: Vec<u8> = pairs
        .iter()
        .flat_map(|&(id, value)| [varint(id), varint(value)].concat())
        .collect();
    [vec![0x00], frame(0x04, &payload)].concat()
}

async fn uni(quic: &noq::Connection, bytes: &[u8]) -> Result<noq::SendStream, TestError> {
    let mut stream = quic.open_uni().await?;
    stream.write_all(bytes).await?;
    Ok(stream)
}

/// A raw stream's bytes, and how it ended: FIN, a reset code, or the connection's end as code 0.
async fn raw_stream(recv: &mut noq::RecvStream) -> (Vec<u8>, Result<(), Code>) {
    let mut bytes = Vec::new();
    loop {
        match recv.read_chunk(usize::MAX).await {
            Ok(Some(chunk)) => bytes.extend_from_slice(&chunk),
            Ok(None) => return (bytes, Ok(())),
            Err(noq::ReadError::Reset(code)) => return (bytes, Err(Code(code.into_inner()))),
            Err(_) => return (bytes, Err(Code(0))),
        }
    }
}

/// Reads a raw stream's first frame, whose type and length take a byte each.
async fn first_frame(recv: &mut noq::RecvStream) -> Result<Vec<u8>, TestError> {
    let mut frame = vec![0; 2];
    recv.read_exact(&mut frame).await?;
    frame.resize(2 + usize::from(frame[1]), 0);
    recv.read_exact(&mut frame[2..]).await?;
    Ok(frame)
}

async fn stopped(send: &noq::SendStream) -> Option<Code> {
    send.stopped()
        .await
        .expect("stream outcome")
        .map(|code| Code(code.into_inner()))
}

/// Whether STOP_SENDING has already arrived for a raw stream.
async fn stopped_yet(send: &noq::SendStream) -> bool {
    yields().await;
    std::future::poll_fn(|cx| Poll::Ready(std::pin::pin!(send.stopped()).poll(cx).is_ready())).await
}

/// The error this side reports for a connection it closed with `code`.
fn closed(code: Code) -> Error {
    Error::Connection { local: true, code, reason: Bytes::new() }
}

/// The code a connection closed with, within 5 s.
async fn closed_with(quic: &noq::Connection) -> Code {
    match tokio::time::timeout(Duration::from_secs(5), quic.closed()).await {
        Ok(noq::ConnectionError::ApplicationClosed(close)) => Code(close.error_code.into_inner()),
        error => panic!("closed without an application code: {error:?}"),
    }
}

/// Waits for the server's GOAWAY, after which no request starts.
async fn until_goaway(requests: &client::SendRequest) {
    while !requests.going_away() {
        tokio::task::yield_now().await;
    }
    assert_eq!(requests.send_request(get("/late")).await.err(), Some(Error::GoingAway));
}

async fn until_closed(session: Session) -> Result<(u32, String), Error> {
    session.closed().await
}

/// Whether a session's streams and datagrams ended, and how it closed; each must end within 5 s.
async fn session_end(session: &Session) -> (bool, bool, Result<(u32, String), Error>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        (
            session.accept_uni().await.is_none(),
            session.read_datagram().await.is_none(),
            session.closed().await,
        )
    })
    .await
    .expect("the session ends with its connection")
}

#[tokio::test]
async fn responses_without_content_may_declare_a_length() -> Result<(), TestError> {
    let Served { peers, serving, .. } = pair(PLAIN).await?.serve(|request, stream| async move {
        let status: u16 = request.uri().path()[1..].parse().unwrap();
        let mut send = stream.split().0;
        let response = http::Response::builder().status(status).header("content-length", 11);
        send.send_response(response.body(()).unwrap()).await.unwrap();
        send.finish().await.unwrap();
    });
    let (driver, requests) = client(&peers);
    for (method, status) in [("GET", 204), ("GET", 304), ("HEAD", 200)] {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://localhost/{status}"))
            .body(())?;
        let (mut send, mut recv) = requests.send_request(request).await?.split();
        send.finish().await?;
        assert_eq!(recv.response().await?.status(), status);
        assert_eq!(body(&mut recv).await, Ok(Vec::new()), "{status}");
    }
    settled(&peers.budget).await;
    peers.client.close(0_u32.into(), b"done");
    assert!(driver.await?.is_ok() && serving.await?.is_ok(), "a peer close code 0 is graceful");
    Ok(())
}

#[tokio::test]
async fn abandoned_streams_carry_the_codes_table() -> Result<(), TestError> {
    let Served { peers, .. } = pair(PLAIN).await?.serve(|request, stream| async move {
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
    let (_driver, requests) = client(&peers);
    let (mut send, mut recv) = requests.send_request(get("/unread")).await?.split();
    assert_eq!(recv.response().await.map(|response| response.status()), Ok(http::StatusCode::OK));
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
    Ok(())
}

#[tokio::test]
async fn protocol_violations_close_the_connection_with_their_code() -> Result<(), TestError> {
    let request = |bytes: Vec<u8>| (Vec::new(), bytes);
    let after_head = |bytes: Vec<u8>| request([request_head(&[]), bytes].concat());
    let streams = |streams: Vec<(Vec<u8>, bool)>| (streams, Vec::new());
    let open = |bytes: Vec<u8>| streams(vec![(bytes, false)]);
    let on_control = |bytes: Vec<u8>| open([control(&[]), bytes].concat());
    let cases = [
        (open([vec![0], frame(0x07, &[4])].concat()), Code::H3_MISSING_SETTINGS),
        (streams(vec![(control(&[]), false); 2]), Code::H3_STREAM_CREATION_ERROR),
        (streams(vec![(control(&[]), true)]), Code::H3_CLOSED_CRITICAL_STREAM),
        (on_control(frame(0x06, &[0; 8])), Code::H3_FRAME_UNEXPECTED),
        // The WebTransport signal only opens a stream, with a client bidirectional stream's ID.
        (on_control(frame(0x41, &[])), Code::H3_FRAME_ERROR),
        (after_head(frame(0x41, &[])), Code::H3_FRAME_ERROR),
        (request(frame(0x41, &[0])), Code::H3_ID_ERROR),
        (open(control(&[(0x21, 0), (0x21, 1)])), Code::H3_SETTINGS_ERROR),
        (open(vec![0x02, 0xc1, 0x01, 0x61]), Code::QPACK_ENCODER_STREAM_ERROR),
        (open(vec![0x03, 0x80]), Code::QPACK_DECODER_STREAM_ERROR),
        (open(vec![0x01, 0x00]), Code::H3_STREAM_CREATION_ERROR),
        // A session ID must be a client-initiated bidirectional stream's.
        (open([varint(0x54), varint(2)].concat()), Code::H3_ID_ERROR),
        (request(frame(0x00, b"body")), Code::H3_FRAME_UNEXPECTED),
        (after_head(frame(0x04, &[])), Code::H3_FRAME_UNEXPECTED),
        (request(frame(0x01, &[0x01, 0x00, 0xd1])), Code::QPACK_DECOMPRESSION_FAILED),
    ];
    for (index, ((streams, request), code)) in cases.into_iter().enumerate() {
        let Served { peers, serving, .. } = pair(PLAIN).await?.serve(|_, stream| async move {
            let _ = stream.split().1.data().await;
        });
        let mut held = Vec::new();
        for (bytes, finish) in streams {
            held.push(uni(&peers.client, &bytes).await?);
            if finish {
                held.last_mut().expect("opened").finish()?;
            }
        }
        if !request.is_empty() {
            held.push(peers.bi(&request).await?.0);
        }
        assert_eq!(closed_with(&peers.client).await, code, "case {index}");
        assert_eq!(serving.await?, Err(closed(code)));
    }
    // A datagram's quarter stream ID must fit a stream ID.
    let Served { peers, .. } = pair(PLAIN).await?.serve(|_, _| async {});
    peers.client.send_datagram(varint(1 << 60).into())?;
    assert_eq!(closed_with(&peers.client).await, Code::H3_DATAGRAM_ERROR);
    Ok(())
}

#[tokio::test]
async fn a_stopped_control_stream_closes_the_connection() -> Result<(), TestError> {
    let critical = Err(closed(Code::H3_CLOSED_CRITICAL_STREAM));
    // The server's control stream is the first stream its peer accepts.
    let Served { peers, serving, .. } = pair(PLAIN).await?.serve(|_, _| async {});
    peers.client.accept_uni().await?.stop(0_u32.into())?;
    assert_eq!(closed_with(&peers.client).await, Code::H3_CLOSED_CRITICAL_STREAM);
    assert_eq!(serving.await?, critical);

    let peers = pair(PLAIN).await?;
    let (driver, _requests) = client(&peers);
    peers.server.accept_uni().await?.stop(0_u32.into())?;
    assert_eq!(closed_with(&peers.server).await, Code::H3_CLOSED_CRITICAL_STREAM);
    assert_eq!(driver.await?, critical);
    Ok(())
}

/// A push the client never allowed closes the connection; five interim heads pass, as quic-go lets
/// them, and a sixth ends the request, as does 101, which HTTP/3 does not have (RFC 9114 §4.5).
#[tokio::test]
async fn responses_the_client_refuses() -> Result<(), TestError> {
    let head = |status: &str| frame(0x01, &section(&[(":status", status)]));
    for (bytes, expected) in [
        // Push ID 0 and an empty field section; our client sends no MAX_PUSH_ID.
        (frame(0x05, &[0x00, 0x00, 0x00]), Err(closed(Code::H3_ID_ERROR))),
        ([head("103").repeat(5), head("200")].concat(), Ok(http::StatusCode::OK)),
        (head("100").repeat(6), Err(Error::Protocol(Code::H3_EXCESSIVE_LOAD))),
        (head("101"), Err(Error::Protocol(Code::H3_MESSAGE_ERROR))),
        (frame(0x41, &[]), Err(closed(Code::H3_FRAME_ERROR))),
    ] {
        let peers = pair(PLAIN).await?;
        let (driver, requests) = client(&peers);
        let (_send, mut recv) = requests.send_request(get("/")).await?.split();
        let (mut response, _request) = peers.server.accept_bi().await?;
        response.write_all(&bytes).await?;
        let received = tokio::time::timeout(Duration::from_secs(5), recv.response()).await?;
        assert_eq!(received.map(|response| response.status()), expected);
        match expected {
            Err(Error::Protocol(code)) => assert_eq!(stopped(&response).await, Some(code)),
            Err(error @ Error::Connection { code, .. }) => {
                assert_eq!(closed_with(&peers.server).await, code);
                assert_eq!(driver.await?, Err(error));
            }
            _ => {}
        }
    }
    Ok(())
}

#[tokio::test]
async fn refusals_stay_on_their_stream() -> Result<(), TestError> {
    // Our QPACK: :status 431 as a literal after the static :status name, :status 400 indexed.
    let status_431 = frame(0x01, &[0x00, 0x00, 0x5f, 0x09, 0x03, b'4', b'3', b'1']);
    let status_400 = frame(0x01, &[0x00, 0x00, 0xff, 0x04]);
    let many: Vec<(&str, &str)> = std::iter::repeat_n(("a", ""), 130).collect();
    let wt = Code(0x52e4a40fa8db);
    let mut cases = vec![
        (
            frame(0x01, &[0; 5000])[..100].to_vec(),
            Ok(status_431.clone()),
            Some(Code::H3_EXCESSIVE_LOAD),
        ),
        (request_head(&many), Ok(status_431), Some(Code::H3_EXCESSIVE_LOAD)),
        ([varint(0x41), varint(0)].concat(), Err(wt), Some(wt)),
        (
            [request_head(&[(":method", "POST"), ("content-length", "5")]), frame(0x00, b"abcdefgh")].concat(),
            Err(Code::H3_REQUEST_CANCELLED),
            Some(Code::H3_MESSAGE_ERROR),
        ),
    ];
    let get = [(":method", "GET"), (":scheme", "https"), (":authority", "meter.example"), (":path", "/")];
    let with = |extra: &[(&'static str, &'static str)]| [&get[..], extra].concat();
    let connect = |fields: &[(&'static str, &'static str)]| [&[(":method", "CONNECT")][..], fields].concat();
    let malformed = [
        with(&[("Upper", "x")]),
        with(&[("x", "a\nb")]),
        [&[("x", "1")][..], &get].concat(),
        with(&[(":status", "200")]),
        with(&[(":path", "/again")]),
        with(&[("connection", "close")]),
        with(&[("te", "gzip")]),
        with(&[("content-length", "7"), ("content-length", "8")]),
        with(&[("content-length", "+7")]),
        with(&[("host", "other.example")]),
        with(&[(":protocol", "webtransport")]),
        get[1..].to_vec(),
        vec![get[0], get[1], get[3]],
        vec![get[0], get[2], get[3]],
        vec![get[0], get[1], get[2], (":path", "relative")],
        vec![get[0], get[1], get[2], (":path", "")],
        vec![get[0], get[1], (":authority", "user@meter.example"), get[3]],
        connect(&[(":protocol", "webtransport"), get[1], get[2]]),
        connect(&[(":protocol", "webtransport"), get[1], get[2], (":path", "")]),
        connect(&[get[1], get[2], get[3]]),
    ];
    let error = Code::H3_MESSAGE_ERROR;
    cases.extend(malformed.map(|fields| (frame(0x01, &section(&fields)), Err(error), Some(error))));
    let unsupported = [
        connect(&[(":authority", "meter.example:443")]),
        connect(&[(":protocol", "websocket"), get[1], get[2], get[3]]),
    ];
    cases.extend(unsupported.map(|fields| (frame(0x01, &section(&fields)), Ok(status_400.clone()), None)));
    let Served { peers, .. } = pair(raw(&[])).await?.serve(|_, stream| async move {
        let _ = stream.split().1.data().await;
    });
    for (index, (bytes, response, stop)) in cases.into_iter().enumerate() {
        // Bodies stay open, so each STOP_SENDING arrives before the stream could complete.
        let (send, mut recv) = peers.bi(&bytes).await?;
        let (bytes, end) = raw_stream(&mut recv).await;
        assert_eq!(end.map(|()| bytes), response, "case {index}");
        if stop.is_some() {
            assert_eq!(stopped(&send).await, stop, "case {index}");
        }
    }
    assert!(peers.client.close_reason().is_none(), "refusals leave the connection open");
    settled(&peers.budget).await;
    Ok(())
}

#[tokio::test]
async fn a_budget_refusal_rejects_only_the_new_request() -> Result<(), TestError> {
    let Served { peers, .. } = pair(Setup { limit: 0, ..PLAIN }).await?.serve(respond);
    let (_driver, requests) = client(&peers);
    let (_send, mut recv) = requests.send_request(get("/")).await?.split();
    assert_eq!(recv.response().await.err(), Some(Error::Reset(Code::H3_REQUEST_REJECTED)));
    peers.budget.limit.store(usize::MAX, Ordering::Relaxed);
    let (_send, mut recv) = requests.send_request(get("/")).await?.split();
    assert_eq!(recv.response().await?.status(), http::StatusCode::OK);
    drop((_send, recv));
    settled(&peers.budget).await;
    Ok(())
}

/// A head the budget cannot hold once its stream was admitted was never processed, so it gets
/// H3_REQUEST_REJECTED both ways, not the 431 of a head over the size limit: whole, the decoded
/// head is refused, and in parts the stream cannot buffer the rest.
#[tokio::test]
async fn a_head_over_the_budget_rejects_the_admitted_request() -> Result<(), TestError> {
    let head = request_head(&[]);
    // Its section is under 64 bytes, so the frame header takes two.
    let (header, section) = head.split_at(2);
    for part in [section, &section[..1]] {
        let Served { peers, .. } = pair(PLAIN)
            .await?
            .serve(|_, _| async { panic!("a refused head reached its route") });
        let (mut send, mut recv) = peers.bi(header).await?;
        // Admitted: from here the budget holds only the stream's two halves.
        let used = charged(&peers.budget, 0).await;
        peers.budget.limit.store(used, Ordering::Relaxed);
        send.write_all(part).await?;
        assert_eq!(raw_stream(&mut recv).await.1, Err(Code::H3_REQUEST_REJECTED));
        assert_eq!(stopped(&send).await, Some(Code::H3_REQUEST_REJECTED));
    }
    Ok(())
}

#[tokio::test]
async fn goaway_refuses_new_requests_and_closes_after_the_last() -> Result<(), TestError> {
    let (started, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let (running, held) = (started.clone(), release.clone());
    let Served { peers, serving, stop, .. } = pair(PLAIN).await?.serve(move |request, stream| {
        let (running, held) = (running.clone(), held.clone());
        async move {
            running.notify_one();
            held.notified().await;
            respond(request, stream).await;
        }
    });
    let (driver, requests) = client(&peers);
    let (_send, mut recv) = requests.send_request(get("/held")).await?.split();
    started.notified().await;
    stop.notify_one();
    until_goaway(&requests).await;
    // No session starts after it either, and the refusal says why.
    assert_eq!(Session::connect(&requests, get("/wt")).await.err(), Some(Error::GoingAway));
    // A request that ignores the GOAWAY gets H3_REQUEST_REJECTED.
    let mut ignoring = peers.bi(&request_head(&[])).await?;
    assert_eq!(raw_stream(&mut ignoring.1).await.1, Err(Code::H3_REQUEST_REJECTED));
    release.notify_one();
    assert_eq!(recv.response().await?.status(), http::StatusCode::OK);
    drop((_send, recv, ignoring));
    assert_eq!((driver.await?, serving.await?), (Ok(()), Ok(())));
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);
    Ok(())
}

#[tokio::test]
async fn deadlines_close_idle_and_draining_connections_and_stale_heads() -> Result<(), TestError> {
    let Served { peers, serving, .. } = pair(PLAIN).await?.serve(|_, _| async {});
    let (driver, _requests) = client(&peers);
    let (send, mut recv) = peers.bi(&frame(0x01, &[0; 10])[..4]).await?;
    // Jump only once the server holds the stream and has started its header deadline.
    charged(&peers.budget, 0).await;
    tokio::task::yield_now().await;
    jump(Duration::from_secs(11)).await;
    assert_eq!(raw_stream(&mut recv).await.1, Err(Code::H3_REQUEST_INCOMPLETE));
    assert_eq!(stopped(&send).await, Some(Code::H3_REQUEST_INCOMPLETE));
    jump(Duration::from_secs(16)).await;
    assert_eq!((driver.await?, serving.await?), (Ok(()), Ok(())));
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);

    let held = Arc::new(Notify::new());
    let holding = held.clone();
    let Served { peers, serving, stop, .. } = pair(PLAIN).await?.serve(move |_, stream| {
        let holding = holding.clone();
        async move {
            holding.notify_one();
            std::future::pending::<()>().await;
            drop(stream);
        }
    });
    let (driver, requests) = client(&peers);
    let _held = requests.send_request(get("/held")).await?;
    held.notified().await;
    stop.notify_one();
    until_goaway(&requests).await;
    assert!(
        peers.server.close_reason().is_none(),
        "only the drain closes a connection with a held request"
    );
    jump(Duration::from_secs(6)).await;
    assert_eq!((driver.await?, serving.await?), (Ok(()), Ok(())));
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);
    Ok(())
}

#[tokio::test]
async fn cancelled_lanes_keep_their_association_header() -> Result<(), TestError> {
    // One-byte stream credit makes the header trickle, so the first lane is cancelled mid-header.
    for (reliable_reset, window) in [(true, Some(1)), (true, None), (false, Some(1)), (false, None)] {
        let cancel = Arc::new(Notify::new());
        let cancelled = cancel.clone();
        let setup = Setup { window, reliable_reset, ..DRAFT02 };
        let Served { peers, .. } = pair(setup).await?.sessions(move |session| {
            let cancelled = cancelled.clone();
            async move {
                if window.is_some() {
                    tokio::select! {
                        _ = session.open_uni() => panic!("the header outran its stream credit"),
                        () = cancelled.notified() => {}
                    }
                } else {
                    let mut lane = session.open_uni().await.unwrap();
                    lane.write_all(&[1; 64 * 1024]).await.unwrap();
                    lane.reset(WtCode(7));
                }
                let mut next = session.open_uni().await.unwrap();
                next.write_all(b"next").await.unwrap();
                next.finish().unwrap();
                let _ = session.closed().await;
            }
        });
        let (_connect, _response) = peers.connect().await?;
        // The server's control stream comes first.
        let _server_control = peers.client.accept_uni().await?;
        let mut lane = peers.client.accept_uni().await?;
        let mut bytes = Vec::new();
        if window.is_some() {
            bytes.extend_from_slice(&lane.read_chunk(1).await?.unwrap_or_default());
            cancel.notify_one();
        }
        let (rest, end) = raw_stream(&mut lane).await;
        bytes.extend(rest);
        let code = if window.is_some() { WtCode(0) } else { WtCode(7) };
        assert_eq!(end, Err(code.to_http()), "reset, never FIN");
        if reliable_reset {
            assert_eq!(&bytes[..3], [0x40, 0x54, 0x00], "RESET_STREAM_AT keeps the header");
        }
        let mut next = peers.client.accept_uni().await?;
        assert_eq!(raw_stream(&mut next).await, (b"\x40\x54\x00next".to_vec(), Ok(())));
        drop((lane, next));
        peers.client.close(0_u32.into(), b"done");
        settled(&peers.budget).await;
    }
    Ok(())
}

#[tokio::test]
async fn prepared_datagrams_repeat_and_the_last_session_ends_its_connection() -> Result<(), TestError> {
    let Served { peers, serving, .. } = pair(PLAIN).await?.sessions(|session| async move {
        for _ in 0..2 {
            let reply = session.read_datagram().await.unwrap();
            session.send_datagram(&[&b"echo "[..], &reply].concat()).unwrap();
        }
        let _ = session.closed().await;
    });
    let (driver, requests) = client(&peers);
    let session = accepted(&requests).await?;
    {
        let mut repeated = session.prepare_datagram(b"PING,2")?;
        for _ in 0..2 {
            repeated.send_wait().await?;
            assert_eq!(session.read_datagram().await.as_deref(), Some(&b"echo PING,2"[..]));
        }
    }
    drop(session);
    settled(&peers.budget).await;
    assert_eq!(
        (driver.await?, serving.await?),
        (Ok(()), Ok(())),
        "a sessions-only connection ends with its session"
    );
    Ok(())
}

#[tokio::test]
async fn sessions_end_with_their_connection() -> Result<(), TestError> {
    // The server's connection goes away: each side's session ends with it, though the client
    // still holds its stopped driver.
    let Served { peers, serving, mut outcomes, .. } = pair(PLAIN)
        .await?
        .sessions(|session| async move { session_end(&session).await });
    let (mut driver, requests) = client::new(peers.client.clone());
    let driving = tokio::spawn(async move { (driver.drive().await, driver) });
    let session = accepted(&requests).await?;
    peers.server.close(Code::H3_NO_ERROR.into(), b"restart");
    let (driven, _stopped) = driving.await?;
    assert_eq!(driven, Ok(()));
    let restart = Error::Connection {
        local: false,
        code: Code::H3_NO_ERROR,
        reason: Bytes::from_static(b"restart"),
    };
    assert_eq!(session_end(&session).await, (true, true, Err(restart)));
    let closed_here = Err(Error::Transport(noq::ConnectionError::LocallyClosed));
    assert_eq!(outcomes.recv().await, Some((true, true, closed_here)));
    assert_eq!(serving.await?, Ok(()));

    // Dropping the driver ends its session too.
    let Served { peers, .. } = pair(PLAIN).await?.sessions(until_closed);
    let (driver, requests) = client(&peers);
    let session = accepted(&requests).await?;
    driver.abort();
    assert!(driver.await.is_err_and(|error| error.is_cancelled()));
    assert_eq!(session_end(&session).await, (true, true, Err(closed(Code::H3_NO_ERROR))));
    Ok(())
}

/// A client ignores content-length in a successful response to CONNECT (RFC 9110 §9.3.6), so the
/// session's capsules follow one that says 0.
#[tokio::test]
async fn a_successful_connect_ignores_its_content_length() -> Result<(), TestError> {
    let peers = pair(PLAIN).await?;
    // A raw server whose SETTINGS allow WebTransport as Go's do.
    let _control = uni(&peers.server, &control(&[(0x08, 1), (0x33, 1), (0x2c7cf000, 1)])).await?;
    let (driver, requests) = client(&peers);
    let serving = async {
        let (mut send, mut recv) = peers.server.accept_bi().await?;
        recv.read_chunk(usize::MAX).await?;
        let head = frame(0x01, &section(&[(":status", "200"), ("content-length", "0")]));
        send.write_all(&[head, close_capsule(7, "bye")].concat()).await?;
        send.finish()?;
        Ok::<_, TestError>((send, recv))
    };
    let (connected, served) = tokio::join!(Session::connect(&requests, get("/wt")), serving);
    let _streams = served?;
    let (session, _) = connected?.expect("accepted");
    assert_eq!(session.closed().await, Ok((7, "bye".into())));
    drop(driver);
    Ok(())
}

#[tokio::test]
async fn closing_sends_close_then_fin_and_waits_for_the_peer() -> Result<(), TestError> {
    // The server ends the session; our client's FIN ends the wait long before 1 s.
    let Served { peers, serving, .. } = pair(PLAIN).await?.sessions(|session| async move {
        let started = std::time::Instant::now();
        session.close(2, "lifetime").await;
        assert!(started.elapsed() < Duration::from_millis(900));
    });
    let (driver, requests) = client(&peers);
    let session = accepted(&requests).await?;
    assert_eq!(session.closed().await?, (2, "lifetime".into()));
    session.close(0, "").await;
    settled(&peers.budget).await;
    jump(Duration::from_secs(1)).await;
    assert_eq!((driver.await?, serving.await?), (Ok(()), Ok(())));
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);

    // The peer ends it: the server only finishes its side, and the connection closes at once.
    let Served { peers, serving, .. } = pair(PLAIN).await?.sessions(|session| async move {
        assert_eq!(session.closed().await.unwrap(), (7, "bye".into()));
        session.close(1, "unused").await;
    });
    let (driver, requests) = client(&peers);
    accepted(&requests).await?.close(7, "bye").await;
    assert_eq!((driver.await?, serving.await?), (Ok(()), Ok(())));

    // A raw peer answering with FIN never sees STOP_SENDING, and the connection lingers a second
    // so the CLOSE arrives before CONNECTION_CLOSE.
    let Served { peers, serving, .. } = pair(DRAFT02)
        .await?
        .sessions(|session| async move { session.close(2, "lifetime").await });
    let (mut connect, response) = peers.connect().await?;
    let (bytes, end) = response.await?;
    assert!(bytes.ends_with(&close_capsule(2, "lifetime")) && end.is_ok(), "CLOSE, then FIN");
    connect.finish()?;
    assert_eq!(stopped(&connect).await, None);
    settled(&peers.budget).await;
    assert!(peers.server.close_reason().is_none());
    jump(Duration::from_secs(1)).await;
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);
    assert_eq!(serving.await?, Ok(()));

    // A silent peer gets STOP_SENDING WT_SESSION_GONE only once the second passes.
    let Served { peers, .. } = pair(DRAFT02)
        .await?
        .sessions(|session| async move { session.close(4, "shutdown").await });
    let (connect, response) = peers.connect().await?;
    let (bytes, end) = response.await?;
    assert!(bytes.ends_with(&close_capsule(4, "shutdown")) && end.is_ok(), "CLOSE, then FIN");
    jump(Duration::from_millis(900)).await;
    assert!(!stopped_yet(&connect).await, "STOP_SENDING before the second");
    jump(Duration::from_millis(200)).await;
    assert_eq!(stopped(&connect).await, Some(Code::WT_SESSION_GONE));
    Ok(())
}

#[tokio::test]
async fn data_after_the_peers_close_is_a_message_error() -> Result<(), TestError> {
    let Served { peers, mut outcomes, .. } = pair(DRAFT02).await?.sessions(until_closed);
    // A plain request first, so the connection outlives the session and the stream's end shows.
    peers.get().await?;
    let (mut connect, _response) = peers.connect().await?;
    // Only FIN may follow a CLOSE; a DRAIN may not.
    let drain = frame(0x00, &frame(0x78ae, &[]));
    connect.write_all(&[close_capsule(7, "bye"), drain].concat()).await?;
    assert_eq!(outcomes.recv().await, Some(Ok((7, "bye".into()))));
    assert_eq!(stopped(&connect).await, Some(Code::H3_MESSAGE_ERROR));
    settled(&peers.budget).await;
    Ok(())
}

/// Once a session ended, it opens no stream and sends no datagram, and its streams refuse reads
/// and writes and end with WT_SESSION_GONE, as the drafts require and webtransport-go does. A read
/// or write already waiting then wakes at once, as closeWithSession wakes it.
#[tokio::test]
async fn an_ended_session_ends_its_streams_and_datagrams() -> Result<(), TestError> {
    // The client's 16-byte window holds the server's write.
    let setup = Setup { window: Some(16), ..DRAFT02 };
    let Served { peers, mut outcomes, .. } = pair(setup).await?.sessions(|session| async move {
        let (mut lane, mut own) = (session.accept_uni().await.unwrap(), session.open_uni().await.unwrap());
        let mut datagram = session.prepare_datagram(b"late").unwrap();
        let waiting = tokio::join!(lane.read_chunk(), own.write_chunk(Bytes::from_static(&[0; 64])));
        vec![
            waiting.0.err(),
            waiting.1.err(),
            lane.read_chunk().await.err(),
            own.write_all(b"late").await.err(),
            session.send_datagram(b"late").err(),
            datagram.send_wait().await.err(),
            session.open_uni().await.err(),
        ]
    });
    // A plain request first, so the connection outlives the session and cannot wake them instead.
    peers.get().await?;
    let (mut connect, _response) = peers.connect().await?;
    // A stream of session 4 (0x54, 4), the second request stream, that sends the server's read nothing.
    let lane = uni(&peers.client, b"\x40\x54\x04").await?;
    let (_server_control, mut own) = (peers.client.accept_uni().await?, peers.client.accept_uni().await?);
    // A byte past the header: the server's write has begun and its read waits.
    own.read_exact(&mut [0; 4]).await?;
    connect.write_all(&close_capsule(2, "lifetime")).await?;
    let ended = tokio::time::timeout(Duration::from_secs(5), outcomes.recv()).await?;
    assert_eq!(ended, Some(vec![Some(Error::Refused); 7]));
    assert_eq!(stopped(&lane).await, Some(Code::WT_SESSION_GONE));
    assert_eq!(raw_stream(&mut own).await.1, Err(Code::WT_SESSION_GONE));
    Ok(())
}

/// A session stream the budget cannot hold gets the draft's code for one not buffered.
#[tokio::test]
async fn a_session_stream_over_the_budget_is_refused_as_unbuffered() -> Result<(), TestError> {
    let Served { peers, .. } = pair(Setup { limit: 0, ..PLAIN }).await?.serve(|_, _| async {});
    // The first stream is in the floor kept for critical ones; its session stream is not.
    let stream = uni(&peers.client, b"\x40\x54\x00").await?;
    assert_eq!(stopped(&stream).await, Some(Code::WT_BUFFERED_STREAM_REJECTED));
    Ok(())
}

#[tokio::test]
async fn a_peer_withholding_stream_credit_cannot_hold_a_session() -> Result<(), TestError> {
    // No stream credit: the 200 head never leaves, yet the close ends within its drain.
    let accepted = Arc::new(Notify::new());
    let accepting = accepted.clone();
    let setup = Setup { window: Some(0), ..DRAFT02 };
    let Served { peers, mut outcomes, .. } = pair(setup).await?.sessions(move |session| {
        let accepting = accepting.clone();
        async move {
            accepting.notify_one();
            session.close(2, "lifetime").await;
        }
    });
    let (_connect, response) = peers.connect().await?;
    accepted.notified().await;
    yields().await;
    jump(Duration::from_millis(1100)).await;
    tokio::time::timeout(Duration::from_secs(5), outcomes.recv()).await?;
    assert_eq!(response.await?, (Vec::new(), Err(Code::WT_SESSION_GONE)));

    // Nor can it hold the refusal of a CONNECT that never showed WebTransport SETTINGS.
    let setup = Setup { window: Some(0), ..PLAIN };
    let Served { peers, .. } = pair(setup)
        .await?
        .sessions(|_| async { panic!("accepted without SETTINGS") });
    let (_connect, response) = peers.connect().await?;
    charged(&peers.budget, 0).await;
    jump(Duration::from_millis(5100)).await;
    yields().await;
    jump(Duration::from_millis(10_100)).await;
    assert_eq!(response.await?, (Vec::new(), Err(Code::H3_REQUEST_CANCELLED)));
    Ok(())
}

#[tokio::test]
async fn shutdown_closes_every_session_before_the_connection() -> Result<(), TestError> {
    let Served { peers, serving, stop, .. } = pair(PLAIN).await?.sessions(until_closed);
    let (driver, requests) = client(&peers);
    let session = accepted(&requests).await?;
    stop.notify_one();
    assert_eq!(session.closed().await?, (4, "shutdown".into()));
    drop(session);
    settled(&peers.budget).await;
    jump(Duration::from_secs(1)).await;
    assert_eq!((driver.await?, serving.await?), (Ok(()), Ok(())));
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);

    let Served { peers, serving, stop, mut outcomes } = pair(PLAIN).await?.sessions(until_closed);
    // Admitted before GOAWAY, this CONNECT waits for SETTINGS and is accepted only after it.
    let (mut connect, response) = peers.connect().await?;
    charged(&peers.budget, 0).await;
    stop.notify_one();
    let mut incoming = peers.client.accept_uni().await?;
    let mut received = Vec::new();
    while !received.ends_with(&frame(0x07, &varint(4))) {
        received.extend_from_slice(&incoming.read_chunk(usize::MAX).await?.ok_or("control stream ended")?);
    }
    let (_late, mut late_response) = peers.bi(&connect_head()).await?;
    assert_eq!(raw_stream(&mut late_response).await.1, Err(Code::H3_REQUEST_REJECTED));
    let _control = uni(&peers.client, &control(DRAFT02_SETTINGS)).await?;
    let (bytes, end) = response.await?;
    assert!(
        bytes.starts_with(&[0x01]) && bytes.ends_with(&close_capsule(4, "shutdown")) && end.is_ok(),
        "200, CLOSE, then FIN"
    );
    assert_eq!(outcomes.recv().await, Some(Ok((4, "shutdown".into()))));
    connect.finish()?;
    assert_eq!(stopped(&connect).await, None);
    settled(&peers.budget).await;
    assert!(peers.server.close_reason().is_none(), "the CLOSE goes first");
    jump(Duration::from_secs(1)).await;
    assert_eq!(closed_with(&peers.client).await, Code::H3_NO_ERROR);
    assert_eq!(serving.await?, Ok(()));
    Ok(())
}

#[tokio::test]
async fn one_session_per_connection_and_streams_wait_for_theirs() -> Result<(), TestError> {
    let release = Arc::new(Notify::new());
    let (started, mut sessions) = mpsc::unbounded_channel();
    let releasing = release.clone();
    let Served { peers, .. } = pair(DRAFT02).await?.sessions(move |session| {
        let (release, started) = (releasing.clone(), started.clone());
        async move {
            // A session reports 0 when it starts, then 1000 and the streams it took.
            let _ = started.send(0);
            let mut streams = 0;
            tokio::select! {
                () = release.notified() => {}
                () = async { while session.accept_uni().await.is_some() { streams += 1 } } => {}
            }
            let _ = started.send(streams + 1000);
        }
    });
    // A plain request first: this connection is not sessions-only, so it outlives its sessions.
    peers.get().await?;
    // A stream for session 12 arrives before its CONNECT and waits.
    let early = uni(&peers.client, &[varint(0x54), varint(12), b"early".to_vec()].concat()).await?;
    let (_first, _first_response) = peers.connect().await?;
    assert_eq!(sessions.recv().await, Some(0));
    let (second, mut second_response) = peers.bi(&connect_head()).await?;
    assert_eq!(raw_stream(&mut second_response).await.1, Err(Code::H3_REQUEST_REJECTED));
    assert_eq!(stopped(&second).await, Some(Code::H3_REQUEST_REJECTED));
    release.notify_one();
    assert_eq!(sessions.recv().await, Some(1000));
    // Session 4 is gone; a stream for it is refused at once.
    let gone = uni(&peers.client, &[varint(0x54), varint(4)].concat()).await?;
    assert_eq!(stopped(&gone).await, Some(Code::WT_SESSION_GONE));
    let (_third, _third_response) = peers.connect().await?;
    assert_eq!(sessions.recv().await, Some(0));
    drop(early);
    // A stream for a session that never comes is refused after 5 s.
    let before = peers.budget.used.load(Ordering::Relaxed);
    let orphan = uni(&peers.client, &[varint(0x54), varint(64)].concat()).await?;
    charged(&peers.budget, before).await;
    jump(Duration::from_secs(6)).await;
    assert_eq!(stopped(&orphan).await, Some(Code::WT_BUFFERED_STREAM_REJECTED));
    release.notify_one();
    assert_eq!(sessions.recv().await, Some(1001), "the early stream joined session 12");
    Ok(())
}

#[tokio::test]
async fn webtransport_needs_the_peer_signal_and_datagrams() -> Result<(), TestError> {
    // Our QPACK: :status 200 is static index 25, 400 is 67.
    let (ok, bad_request) = (frame(0x01, &[0x00, 0x00, 0xd9]), frame(0x01, &[0x00, 0x00, 0xff, 0x04]));
    for (setup, draft02, allowed) in [
        (DRAFT02, true, true),
        (raw(&[(0x2c7cf000, 1), (0x33, 1)]), false, true),
        (raw(&[(0x2b603742, 1)]), false, false),
        (raw(&[(0xffd277, 1)]), false, false),
        (raw(&[]), false, false),
    ] {
        let Served { peers, .. } = pair(setup).await?.sessions(until_closed);
        let (send, mut recv) = peers.bi(&connect_head()).await?;
        let head = first_frame(&mut recv).await?;
        match (allowed, draft02) {
            // Draft 02 requires the response header naming it.
            (true, true) => assert!(head.len() > ok.len() && head[2..5] == ok[2..], "{head:x?}"),
            (true, false) => assert_eq!(head, ok),
            (false, _) => assert_eq!(head, bad_request, "{:x?}", setup.settings),
        }
        drop((send, recv));
    }

    // Without the peer's SETTINGS the CONNECT waits 5 s, then gets 400.
    let Served { peers, .. } = pair(PLAIN).await?.sessions(|_| async {});
    let (send, mut recv) = peers.bi(&connect_head()).await?;
    charged(&peers.budget, 0).await;
    tokio::task::yield_now().await;
    jump(Duration::from_secs(6)).await;
    assert_eq!(first_frame(&mut recv).await?, bad_request);
    drop(send);
    Ok(())
}

/// A client awaits the server's SETTINGS as long as its caller, as webtransport-go does, not 5 s,
/// then names why no session starts: SETTINGS without WebTransport, or the connection they closed.
#[tokio::test]
async fn a_client_awaits_settings_to_name_why_no_session_starts() -> Result<(), TestError> {
    for (pairs, expected) in [
        (&[(0x33, 1)][..], Error::NoWebTransport),
        (&[(0x21, 0), (0x21, 1)], closed(Code::H3_SETTINGS_ERROR)),
    ] {
        let peers = pair(PLAIN).await?;
        let (_driver, requests) = client(&peers);
        let connecting = tokio::spawn(async move { Session::connect(&requests, get("/wt")).await.err() });
        tokio::task::yield_now().await;
        jump(Duration::from_secs(6)).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!connecting.is_finished(), "gave up waiting for SETTINGS");
        let _control = uni(&peers.server, &control(pairs)).await?;
        assert_eq!(connecting.await?, Some(expected));
    }
    Ok(())
}

#[tokio::test]
async fn http_datagrams_need_the_quic_datagram_parameter() -> Result<(), TestError> {
    let setup = Setup { datagrams: false, ..raw(&[(0x33, 1)]) };
    let Served { peers, serving, .. } = pair(setup).await?.serve(|_, _| async {});
    assert_eq!(closed_with(&peers.client).await, Code::H3_SETTINGS_ERROR);
    assert_eq!(serving.await?, Err(closed(Code::H3_SETTINGS_ERROR)));
    Ok(())
}
